//! File-set scopes for [`list_files`](crate::list_files) and
//! [`grep`](crate::grep).
//!
//! A scope answers "which files count as 'inside the project right now'?".
//! The default is [`Scope::Tracked`] — everything committed to the git
//! index. Other variants let callers expand or narrow the set without
//! shelling out to `git ls-files` / `find`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Files in the git index (i.e. `git ls-files`). Fastest — reads
    /// libgit2's memory-mapped index, no filesystem walk.
    #[default]
    Tracked,

    /// Tracked files + untracked files that pass through `.gitignore`
    /// (same set ripgrep walks by default). Uses the `ignore` crate.
    All,

    /// Files with a non-clean working-tree status (modified, added,
    /// deleted, renamed, or untracked-and-not-ignored). Equivalent to
    /// the union of `git diff --name-only` and untracked files from
    /// `git status`.
    Dirty,

    /// Files with a staged change (index differs from HEAD).
    Staged,
}
