//! Project-scoped file listing and text search for aide-mcp.
//!
//! Replaces shell equivalents (`ls`, `find`, `grep`, `rg`) with
//! MCP-callable primitives that are bounded to the project root,
//! respect `.gitignore`, and default to git-tracked files only.
//!
//! The design rationale — "where aide can be better than bash":
//! - `Scope::Tracked` reads the libgit2 index directly, avoiding a full
//!   filesystem walk (orders of magnitude fewer syscalls on large repos).
//! - `Scope::All` walks via the `ignore` crate (ripgrep's walker), so
//!   `.gitignore` / `.ignore` / global excludes are honoured for free.
//! - The `grep` pipeline is `grep-regex` + `grep-searcher` (the engine
//!   ripgrep is built on), with `smart_case` and binary-file skipping by
//!   default, so agents get ripgrep-quality results through MCP.
//!
//! This crate is intentionally decoupled from MCP wiring — the server
//! crate wraps these primitives into JSON tools.

pub mod grep;
pub mod ls;
pub mod scope;

use std::path::PathBuf;

use thiserror::Error;

pub use crate::grep::{grep, GrepHit, GrepOptions, LineMatch};
pub use crate::ls::{list_files, LsOptions};
pub use crate::scope::Scope;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("no git repository at {0}")]
    NotARepo(PathBuf),
    #[error("invalid glob {pattern:?}: {source}")]
    InvalidGlob {
        pattern: String,
        #[source]
        source: globset::Error,
    },
    #[error("invalid regex {pattern:?}: {source}")]
    InvalidRegex {
        pattern: String,
        #[source]
        source: grep_regex::Error,
    },
    #[error("git2: {0}")]
    Git(#[from] git2::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("ignore walker: {0}")]
    Ignore(#[from] ignore::Error),
    #[error("aide-git: {0}")]
    AideGit(#[from] aide_git::GitError),
}
