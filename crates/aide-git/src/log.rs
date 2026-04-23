use std::path::Path;

use serde::Serialize;

use crate::{open_repo, GitError};

/// A single commit in the log.
#[derive(Debug, Clone, Serialize)]
pub struct CommitEntry {
    pub sha: String,
    pub short: String,
    pub author_name: String,
    pub author_email: String,
    /// Author time (unix epoch seconds).
    pub time: i64,
    pub summary: String,
    pub parents: Vec<String>,
}

/// Return up to `limit` commits reachable from `HEAD`, most recent first.
pub fn log(root: &Path, limit: usize) -> Result<Vec<CommitEntry>, GitError> {
    let repo = open_repo(root)?;
    let mut walker = repo.revwalk()?;
    walker.push_head()?;
    walker.set_sorting(git2::Sort::TIME)?;

    let mut out = Vec::with_capacity(limit.min(128));
    for (i, oid) in walker.enumerate() {
        if i >= limit {
            break;
        }
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let author = commit.author();
        out.push(CommitEntry {
            sha: oid.to_string(),
            short: oid.to_string().chars().take(7).collect(),
            author_name: author.name().unwrap_or("").to_string(),
            author_email: author.email().unwrap_or("").to_string(),
            time: commit.time().seconds(),
            summary: commit.summary().unwrap_or("").to_string(),
            parents: commit.parent_ids().map(|id| id.to_string()).collect(),
        });
    }
    Ok(out)
}

/// Same as [`log`], but keeps only commits that touched `relative_path`
/// (comparison is against each commit's tree vs its first parent, or
/// the root tree for the initial commit). Useful for answering
/// "recent activity on this file" without walking the whole history
/// by hand.
pub fn log_for_path(
    root: &Path,
    relative_path: &str,
    limit: usize,
) -> Result<Vec<CommitEntry>, GitError> {
    let repo = open_repo(root)?;
    let mut walker = repo.revwalk()?;
    walker.push_head()?;
    walker.set_sorting(git2::Sort::TIME)?;

    let target = Path::new(relative_path);
    let mut out = Vec::with_capacity(limit.min(128));
    for oid in walker {
        if out.len() >= limit {
            break;
        }
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        if !commit_touches_path(&repo, &commit, target)? {
            continue;
        }
        let author = commit.author();
        out.push(CommitEntry {
            sha: oid.to_string(),
            short: oid.to_string().chars().take(7).collect(),
            author_name: author.name().unwrap_or("").to_string(),
            author_email: author.email().unwrap_or("").to_string(),
            time: commit.time().seconds(),
            summary: commit.summary().unwrap_or("").to_string(),
            parents: commit.parent_ids().map(|id| id.to_string()).collect(),
        });
    }
    Ok(out)
}

fn commit_touches_path(
    repo: &git2::Repository,
    commit: &git2::Commit<'_>,
    path: &Path,
) -> Result<bool, GitError> {
    let tree = commit.tree()?;
    let parent_tree = if commit.parent_count() == 0 {
        None
    } else {
        Some(commit.parent(0)?.tree()?)
    };
    let mut opts = git2::DiffOptions::new();
    opts.pathspec(path);
    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
    Ok(diff.deltas().len() > 0)
}
