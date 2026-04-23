//! Enumerate files in the project according to a [`Scope`].
//!
//! `Scope::Tracked` reads the libgit2 index directly. `Scope::All` uses
//! the `ignore` crate (ripgrep's walker). `Scope::Dirty` / `Scope::Staged`
//! use `git2::Statuses` / HEAD-vs-index diff. An optional glob filter is
//! applied after the scope has produced a candidate list.

use std::path::Path;

use git2::{DiffOptions, Status, StatusOptions};
use globset::GlobBuilder;
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};

use crate::{Scope, SearchError};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LsOptions {
    /// Optional glob over the repo-relative path. `**/*.rs`, `crates/*/src/**`,
    /// etc. Matches are against the full relative path, so leading `**/`
    /// is usually wanted.
    pub glob: Option<String>,

    /// Cap on the number of returned entries. `None` = no cap.
    pub max_results: Option<usize>,

    /// Hidden dotfiles. Defaults to false (hidden files excluded).
    /// Only affects `Scope::All`; git scopes include whatever git tracks
    /// regardless of this flag.
    pub include_hidden: bool,
}

/// List files under `repo_root` according to `scope`.
///
/// Returns paths **relative to `repo_root`**, using forward slashes for
/// cross-platform stability. The list is sorted by path.
pub fn list_files(
    repo_root: &Path,
    scope: &Scope,
    options: &LsOptions,
) -> Result<Vec<String>, SearchError> {
    let matcher = options
        .glob
        .as_deref()
        .map(|pattern| {
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map(|g| g.compile_matcher())
                .map_err(|source| SearchError::InvalidGlob {
                    pattern: pattern.to_string(),
                    source,
                })
        })
        .transpose()?;

    let mut files = match scope {
        Scope::Tracked => tracked(repo_root)?,
        Scope::All => all(repo_root, options.include_hidden)?,
        Scope::Dirty => dirty(repo_root)?,
        Scope::Staged => staged(repo_root)?,
    };

    if let Some(m) = matcher {
        files.retain(|p| m.is_match(p));
    }

    files.sort();
    files.dedup();

    if let Some(cap) = options.max_results {
        files.truncate(cap);
    }

    Ok(files)
}

fn tracked(repo_root: &Path) -> Result<Vec<String>, SearchError> {
    let repo = git2::Repository::discover(repo_root)
        .map_err(|_| SearchError::NotARepo(repo_root.to_path_buf()))?;
    let index = repo.index()?;
    let mut out = Vec::with_capacity(index.len());
    for entry in index.iter() {
        let path = String::from_utf8_lossy(&entry.path).into_owned();
        out.push(path);
    }
    Ok(out)
}

fn all(repo_root: &Path, include_hidden: bool) -> Result<Vec<String>, SearchError> {
    let mut builder = WalkBuilder::new(repo_root);
    builder
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .parents(true)
        .hidden(!include_hidden)
        .require_git(false);
    let mut out = Vec::new();
    for result in builder.build() {
        let entry = result?;
        if entry.file_type().is_none_or(|ft| !ft.is_file()) {
            continue;
        }
        if let Ok(rel) = entry.path().strip_prefix(repo_root) {
            out.push(normalise(rel));
        }
    }
    Ok(out)
}

fn dirty(repo_root: &Path) -> Result<Vec<String>, SearchError> {
    let repo = git2::Repository::discover(repo_root)
        .map_err(|_| SearchError::NotARepo(repo_root.to_path_buf()))?;
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut opts))?;

    let mut out = Vec::with_capacity(statuses.len());
    for entry in statuses.iter() {
        let bits = entry.status();
        if bits.is_ignored() || bits == Status::CURRENT {
            continue;
        }
        if let Some(path) = entry.path() {
            out.push(path.to_string());
        }
    }
    Ok(out)
}

fn staged(repo_root: &Path) -> Result<Vec<String>, SearchError> {
    let repo = git2::Repository::discover(repo_root)
        .map_err(|_| SearchError::NotARepo(repo_root.to_path_buf()))?;

    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());

    let mut diff_opts = DiffOptions::new();
    let diff = repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut diff_opts))?;

    let mut out = Vec::new();
    diff.foreach(
        &mut |delta, _| {
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().into_owned());
            if let Some(p) = path {
                out.push(p);
            }
            true
        },
        None,
        None,
        None,
    )?;
    Ok(out)
}

fn normalise(rel: &Path) -> String {
    let s = rel.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        s.into_owned()
    } else {
        s.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        path: PathBuf,
    }

    fn init_repo() -> Fixture {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        let repo = git2::Repository::init(&path).unwrap();
        fs::write(path.join("a.rs"), "fn a() {}").unwrap();
        fs::write(path.join("b.rs"), "fn b() {}").unwrap();
        fs::create_dir_all(path.join("sub")).unwrap();
        fs::write(path.join("sub/c.rs"), "fn c() {}").unwrap();
        let mut index = repo.index().unwrap();
        for p in ["a.rs", "b.rs", "sub/c.rs"] {
            index.add_path(Path::new(p)).unwrap();
        }
        let tree_id = index.write_tree().unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        Fixture { _dir: dir, path }
    }

    #[test]
    fn tracked_lists_index_entries() {
        let f = init_repo();
        let files = list_files(&f.path, &Scope::Tracked, &LsOptions::default()).unwrap();
        assert_eq!(files, vec!["a.rs", "b.rs", "sub/c.rs"]);
    }

    #[test]
    fn tracked_ignores_untracked_files() {
        let f = init_repo();
        fs::write(f.path.join("fresh.rs"), "fn x() {}").unwrap();
        let files = list_files(&f.path, &Scope::Tracked, &LsOptions::default()).unwrap();
        assert_eq!(files, vec!["a.rs", "b.rs", "sub/c.rs"]);
    }

    #[test]
    fn all_respects_gitignore() {
        let f = init_repo();
        fs::write(f.path.join(".gitignore"), "ignored.rs\ntarget/\n").unwrap();
        fs::write(f.path.join("ignored.rs"), "should not appear").unwrap();
        fs::create_dir_all(f.path.join("target")).unwrap();
        fs::write(f.path.join("target/x.rs"), "noise").unwrap();
        fs::write(f.path.join("extra.rs"), "visible").unwrap();

        let files = list_files(&f.path, &Scope::All, &LsOptions::default()).unwrap();
        assert!(files.contains(&"extra.rs".to_string()));
        assert!(files.contains(&"a.rs".to_string()));
        assert!(!files.iter().any(|p| p == "ignored.rs"));
        assert!(!files.iter().any(|p| p.starts_with("target/")));
    }

    #[test]
    fn glob_filter_matches_full_path() {
        let f = init_repo();
        let files = list_files(
            &f.path,
            &Scope::Tracked,
            &LsOptions {
                glob: Some("sub/**".into()),
                ..LsOptions::default()
            },
        )
        .unwrap();
        assert_eq!(files, vec!["sub/c.rs"]);
    }

    #[test]
    fn max_results_caps_output() {
        let f = init_repo();
        let files = list_files(
            &f.path,
            &Scope::Tracked,
            &LsOptions {
                max_results: Some(2),
                ..LsOptions::default()
            },
        )
        .unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn dirty_reports_untracked_and_modified() {
        let f = init_repo();
        // Modify a tracked file.
        fs::write(f.path.join("a.rs"), "fn changed() {}").unwrap();
        // Add an untracked file.
        fs::write(f.path.join("new.rs"), "fn n() {}").unwrap();

        let files = list_files(&f.path, &Scope::Dirty, &LsOptions::default()).unwrap();
        assert!(files.contains(&"a.rs".to_string()));
        assert!(files.contains(&"new.rs".to_string()));
        assert!(!files.contains(&"b.rs".to_string()));
    }

    #[test]
    fn staged_reports_index_vs_head() {
        let f = init_repo();
        let repo = git2::Repository::open(&f.path).unwrap();
        // Change a tracked file and stage it.
        fs::write(f.path.join("b.rs"), "fn b2() {}").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("b.rs")).unwrap();
        index.write().unwrap();

        let files = list_files(&f.path, &Scope::Staged, &LsOptions::default()).unwrap();
        assert_eq!(files, vec!["b.rs"]);
    }
}
