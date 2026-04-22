//! CLI entry point for the `post-commit` subcommand.
//!
//! Invoked by the `.git/hooks/post-commit` script that `project_setup`
//! installs. Resolves the current repo's HEAD and tells the indexer daemon
//! about it. Any failure (daemon offline, no git repo, etc.) is logged and
//! swallowed — the hook must never break the user's commit.

use std::path::{Path, PathBuf};

use aide_core::AidePaths;
use aide_git::resolve_head;
use aide_proto::{default_indexer_socket, Request};
use anyhow::Result;

use crate::indexer;

pub async fn run_post_commit() -> Result<()> {
    let cwd = std::env::current_dir()?;
    match notify(&cwd).await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::info!(error = %e, "post-commit: skipped");
            Ok(())
        }
    }
}

async fn notify(cwd: &Path) -> Result<()> {
    let (repo_root, sha) = resolve_head(cwd)?;
    let paths = AidePaths::from_home()?;
    let socket: PathBuf = default_indexer_socket(&paths);

    let request = Request::Enqueue {
        repo_root: repo_root.display().to_string(),
        sha,
    };
    let response = indexer::send(&socket, &request).await?;
    tracing::debug!(?response, "post-commit enqueue");
    Ok(())
}

/// Install (or refresh) the `.git/hooks/post-commit` script that forwards
/// commits to `aide-indexer`. Returns a short status string describing what
/// happened, used by `project_setup` to report to the agent.
pub fn install_post_commit_hook(repo_root: &Path) -> Result<HookInstallOutcome> {
    let hooks_dir = repo_root.join(".git").join("hooks");
    if !hooks_dir.is_dir() {
        return Ok(HookInstallOutcome {
            status: "skipped-no-hooks-dir",
            path: None,
        });
    }
    let hook_path = hooks_dir.join("post-commit");
    let exe = std::env::current_exe()?;
    let desired = hook_script(&exe);

    if hook_path.exists() {
        let existing = std::fs::read_to_string(&hook_path)?;
        if existing == desired {
            return Ok(HookInstallOutcome {
                status: "already-installed",
                path: Some(hook_path),
            });
        }
        if !existing.contains(HOOK_MARKER) {
            return Ok(HookInstallOutcome {
                status: "skipped-foreign-hook",
                path: Some(hook_path),
            });
        }
    }

    write_hook(&hook_path, &desired)?;
    Ok(HookInstallOutcome {
        status: "installed",
        path: Some(hook_path),
    })
}

#[derive(Debug, Clone)]
pub struct HookInstallOutcome {
    pub status: &'static str,
    pub path: Option<PathBuf>,
}

const HOOK_MARKER: &str = "# aide-mcp post-commit hook";

fn hook_script(exe: &Path) -> String {
    format!(
        "#!/bin/sh\n{HOOK_MARKER}\nexec {} post-commit >/dev/null 2>&1 || true\n",
        exe.display()
    )
}

fn write_hook(path: &Path, content: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("aide.tmp");
    std::fs::write(&tmp, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir.join(".git").join("hooks")).unwrap();
    }

    #[test]
    fn install_writes_executable_hook() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let outcome = install_post_commit_hook(dir.path()).unwrap();
        assert_eq!(outcome.status, "installed");
        let script = outcome.path.expect("path");
        assert!(script.exists());
        let content = std::fs::read_to_string(&script).unwrap();
        assert!(content.contains(HOOK_MARKER));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&script).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111);
        }
    }

    #[test]
    fn second_install_is_idempotent() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        install_post_commit_hook(dir.path()).unwrap();
        let outcome = install_post_commit_hook(dir.path()).unwrap();
        assert_eq!(outcome.status, "already-installed");
    }

    #[test]
    fn leaves_foreign_hook_alone() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let hook = dir.path().join(".git").join("hooks").join("post-commit");
        std::fs::write(&hook, "#!/bin/sh\n# someone else's hook\n").unwrap();
        let outcome = install_post_commit_hook(dir.path()).unwrap();
        assert_eq!(outcome.status, "skipped-foreign-hook");
    }

    #[test]
    fn missing_hooks_dir_is_reported() {
        let dir = TempDir::new().unwrap();
        let outcome = install_post_commit_hook(dir.path()).unwrap();
        assert_eq!(outcome.status, "skipped-no-hooks-dir");
    }
}
