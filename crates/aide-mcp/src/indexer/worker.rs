//! Background worker task that produces SCIP indexes.
//!
//! One worker per MCP server, processing jobs serially off an mpsc
//! channel. Callers push jobs in via [`Indexer::enqueue`](super::Indexer);
//! the worker pops them, runs the language plugin's SCIP indexer, and
//! writes the output under `~/.aide/scip/<slug(repo_root)>/<sha>.scip`.
//!
//! Each job exports the commit's tree to a fresh temp dir via
//! [`aide_git::export::export_commit`] and runs the indexer there, so
//! the SCIP output reflects the commit exactly — never the dirty
//! working tree of the source repo.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aide_core::AidePaths;
use aide_git::export::export_commit;
use aide_lang::{LanguagePlugin, Registry};
use tempfile::TempDir;
use tokio::process::Command;
use tokio::sync::mpsc;

use super::state::Store;

/// A single piece of work: index `sha` in `repo_root`.
#[derive(Debug, Clone)]
pub struct Job {
    pub repo_root: String,
    pub sha: String,
}

pub fn spawn(paths: AidePaths, store: Store) -> mpsc::UnboundedSender<Job> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(paths, store, rx));
    tx
}

async fn run(paths: AidePaths, store: Store, mut rx: mpsc::UnboundedReceiver<Job>) {
    let registry = Arc::new(Registry::builtin());
    while let Some(job) = rx.recv().await {
        process(&paths, &registry, &store, &job).await;
    }
}

async fn process(paths: &AidePaths, registry: &Registry, store: &Store, job: &Job) {
    tracing::info!(repo = %job.repo_root, sha = %job.sha, "indexing");

    if let Err(e) = store.mark_in_progress(&job.repo_root, &job.sha).await {
        tracing::warn!(error = %e, "could not mark in-progress");
        return;
    }

    let index_path = index_file_path(paths, &job.repo_root, &job.sha);
    match build_index(paths, registry, job, &index_path).await {
        Ok(()) => {
            if let Err(e) = store
                .mark_ready(&job.repo_root, &job.sha, index_path.clone())
                .await
            {
                tracing::warn!(error = %e, "could not mark ready");
            } else {
                tracing::info!(index = %index_path.display(), "ready");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "index build failed");
            if let Err(e) = store
                .mark_failed(&job.repo_root, &job.sha, e.to_string())
                .await
            {
                tracing::warn!(error = %e, "could not mark failed");
            }
        }
    }
}

async fn build_index(
    paths: &AidePaths,
    registry: &Registry,
    job: &Job,
    output: &Path,
) -> Result<(), IndexError> {
    let repo_path = Path::new(&job.repo_root);
    let plugin = registry
        .detect(repo_path)
        .into_iter()
        .next()
        .ok_or_else(|| IndexError::NoPlugin(job.repo_root.clone()))?;
    let scip = plugin.scip().ok_or_else(|| IndexError::LanguageHasNoScip {
        language: plugin.id().as_str().to_string(),
    })?;

    let bin = paths.bin().join(scip.executable);
    if !bin.exists() {
        return Err(IndexError::BinaryMissing {
            name: scip.name.to_string(),
            path: bin,
        });
    }

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(IndexError::Io)?;
    }

    // Materialise the commit into a throwaway dir so the SCIP output
    // reflects the committed state, not the source repo's working tree.
    let snapshot = TempDir::new().map_err(IndexError::Io)?;
    let snapshot_path = snapshot.path();
    export_commit(repo_path, &job.sha, snapshot_path).map_err(IndexError::Export)?;

    run_indexer(plugin.as_ref(), &bin, snapshot_path, output).await?;

    drop(snapshot);
    Ok(())
}

async fn run_indexer(
    plugin: &dyn LanguagePlugin,
    bin: &Path,
    workdir: &Path,
    output: &Path,
) -> Result<(), IndexError> {
    let args: Vec<OsString> = plugin.scip_args(workdir, output);
    let output_res = Command::new(bin)
        .args(&args)
        .output()
        .await
        .map_err(IndexError::Io)?;

    if !output_res.status.success() {
        let stderr = String::from_utf8_lossy(&output_res.stderr).into_owned();
        return Err(IndexError::IndexerFailed {
            status: output_res.status.code(),
            stderr,
        });
    }

    if !output.exists() {
        return Err(IndexError::NoOutputFile {
            expected: output.to_path_buf(),
        });
    }
    Ok(())
}

fn index_file_path(paths: &AidePaths, repo_root: &str, sha: &str) -> PathBuf {
    paths
        .scip()
        .join(slugify_repo(repo_root))
        .join(format!("{sha}.scip"))
}

/// Turn an absolute repo path into a filename-safe directory name.
/// Collisions are avoided because two absolute paths always differ at
/// some character.
fn slugify_repo(repo_root: &str) -> String {
    repo_root
        .trim_start_matches('/')
        .chars()
        .map(|c| match c {
            '/' | ':' | '\\' | ' ' => '_',
            other => other,
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
enum IndexError {
    #[error("no language plugin matched repo root {0}")]
    NoPlugin(String),
    #[error("language {language} does not declare a SCIP indexer")]
    LanguageHasNoScip { language: String },
    #[error("indexer binary {name} is not installed at {path}")]
    BinaryMissing { name: String, path: PathBuf },
    #[error("I/O error: {0}")]
    Io(std::io::Error),
    #[error("exporting commit to snapshot dir failed: {0}")]
    Export(aide_git::GitError),
    #[error("indexer exited with status {status:?}: {stderr}")]
    IndexerFailed { status: Option<i32>, stderr: String },
    #[error("indexer reported success but did not write {expected}", expected = expected.display())]
    NoOutputFile { expected: PathBuf },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_escapes_path_separators() {
        assert_eq!(
            slugify_repo("/home/jirka/workspace/aide-mcp"),
            "home_jirka_workspace_aide-mcp"
        );
        assert_eq!(slugify_repo("/a b/c:d"), "a_b_c_d");
    }

    #[test]
    fn index_file_path_layout() {
        let paths = AidePaths::at("/tmp/aide-test");
        let p = index_file_path(&paths, "/home/u/repo", "deadbeef");
        assert_eq!(
            p,
            Path::new("/tmp/aide-test/scip/home_u_repo/deadbeef.scip")
        );
    }
}
