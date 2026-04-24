//! Git operations for aide-mcp, backed by libgit2 via the `git2` crate.
//!
//! Most operations are read-only: open the repo at `root`, compute the
//! answer, close. The one mutation-like helper is [`export::export_commit`],
//! which materialises a commit's tree into a destination dir without
//! touching the source repo's workdir or index.

pub mod blame;
pub mod diff;
pub mod export;
pub mod log;
pub mod status;

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("no git repository at {0}")]
    NotARepo(String),
    #[error("HEAD does not point at a commit (unborn branch?)")]
    NoHead,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("git2: {0}")]
    Git2(#[from] git2::Error),
    #[error("decode error: {0}")]
    Decode(String),
}

pub(crate) fn open_repo(root: &Path) -> Result<git2::Repository, GitError> {
    git2::Repository::discover(root).map_err(|e| {
        if e.code() == git2::ErrorCode::NotFound {
            GitError::NotARepo(root.display().to_string())
        } else {
            GitError::Git2(e)
        }
    })
}

/// Resolve the repository working-directory root (the dir containing `.git/`)
/// and the current `HEAD` commit SHA for the repo that `path` lives inside.
pub fn resolve_head(path: &Path) -> Result<(PathBuf, String), GitError> {
    let repo = open_repo(path)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::NotARepo(path.display().to_string()))?
        .to_path_buf();
    let head = repo.head().map_err(|e| {
        if e.code() == git2::ErrorCode::UnbornBranch || e.code() == git2::ErrorCode::NotFound {
            GitError::NoHead
        } else {
            GitError::Git2(e)
        }
    })?;
    let oid = head.target().ok_or(GitError::NoHead)?;
    Ok((workdir, oid.to_string()))
}

/// Short name of the currently checked-out branch — e.g. `"master"`,
/// `"feature/foo"`. Returns `None` on detached HEAD (libgit2 reports
/// no shorthand in that case); the caller typically treats detached
/// HEAD as "no branch" rather than an error. Used by `gh_pr_create`
/// to default `head` to the working-tree branch.
pub fn current_branch(path: &Path) -> Result<Option<String>, GitError> {
    let repo = open_repo(path)?;
    let head = match repo.head() {
        Ok(h) => h,
        Err(e)
            if e.code() == git2::ErrorCode::UnbornBranch
                || e.code() == git2::ErrorCode::NotFound =>
        {
            return Err(GitError::NoHead);
        }
        Err(e) => return Err(GitError::Git2(e)),
    };
    Ok(head.shorthand().map(String::from))
}
