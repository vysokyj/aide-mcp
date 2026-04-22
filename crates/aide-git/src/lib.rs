//! Git read operations for aide-mcp, backed by libgit2 via the `git2` crate.
//!
//! All operations are read-only and synchronous — they open the repository
//! at the given root, compute the answer, and close.

pub mod blame;
pub mod diff;
pub mod log;
pub mod status;

use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("no git repository at {0}")]
    NotARepo(String),
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
