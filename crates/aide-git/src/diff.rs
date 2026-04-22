use std::path::Path;

use git2::{Diff, DiffFormat, DiffOptions, Repository};
use serde::Serialize;

use crate::{open_repo, GitError};

/// Which two trees to diff.
#[derive(Debug, Clone, Copy, Default)]
pub enum DiffMode {
    /// HEAD → working tree (staged + unstaged). The default `git diff HEAD`.
    #[default]
    HeadToWorktree,
    /// Index → working tree. Unstaged changes only.
    IndexToWorktree,
    /// HEAD → index. Staged changes only.
    HeadToIndex,
}

/// Full diff payload.
#[derive(Debug, Clone, Serialize)]
pub struct DiffResult {
    pub mode: &'static str,
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub patch: String,
}

/// Produce a unified diff as a single string, plus summary stats.
pub fn diff(root: &Path, mode: DiffMode, pathspec: Option<&str>) -> Result<DiffResult, GitError> {
    let repo = open_repo(root)?;
    let mut opts = DiffOptions::new();
    if let Some(p) = pathspec {
        opts.pathspec(p);
    }
    let diff = build_diff(&repo, mode, &mut opts)?;

    let stats = diff.stats()?;
    let files_changed = stats.files_changed();
    let insertions = stats.insertions();
    let deletions = stats.deletions();

    let mut patch = String::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            patch.push(line.origin());
        }
        if let Ok(s) = std::str::from_utf8(line.content()) {
            patch.push_str(s);
        }
        true
    })?;

    Ok(DiffResult {
        mode: mode_label(mode),
        files_changed,
        insertions,
        deletions,
        patch,
    })
}

fn build_diff<'r>(
    repo: &'r Repository,
    mode: DiffMode,
    opts: &mut DiffOptions,
) -> Result<Diff<'r>, GitError> {
    match mode {
        DiffMode::HeadToWorktree => {
            let head_tree = repo.head()?.peel_to_tree()?;
            Ok(repo.diff_tree_to_workdir_with_index(Some(&head_tree), Some(opts))?)
        }
        DiffMode::IndexToWorktree => Ok(repo.diff_index_to_workdir(None, Some(opts))?),
        DiffMode::HeadToIndex => {
            let head_tree = repo.head()?.peel_to_tree()?;
            Ok(repo.diff_tree_to_index(Some(&head_tree), None, Some(opts))?)
        }
    }
}

fn mode_label(mode: DiffMode) -> &'static str {
    match mode {
        DiffMode::HeadToWorktree => "head-to-worktree",
        DiffMode::IndexToWorktree => "index-to-worktree",
        DiffMode::HeadToIndex => "head-to-index",
    }
}
