use std::path::Path;

use git2::{Status, StatusOptions};
use serde::Serialize;

use crate::{open_repo, GitError};

/// Snapshot of `git status` for a given repository root.
#[derive(Debug, Clone, Serialize)]
pub struct RepoStatus {
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub is_clean: bool,
    pub files: Vec<FileStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileStatus {
    pub path: String,
    pub staged: Option<&'static str>,
    pub working: Option<&'static str>,
    pub is_conflicted: bool,
    pub is_untracked: bool,
    pub is_ignored: bool,
}

pub fn status(root: &Path) -> Result<RepoStatus, GitError> {
    let repo = open_repo(root)?;

    let head = repo.head().ok();
    let branch = head
        .as_ref()
        .and_then(|h| h.shorthand().map(str::to_string));
    let head_sha = head
        .as_ref()
        .and_then(|h| h.target().map(|o| o.to_string()));

    let (upstream, ahead, behind) = upstream_divergence(&repo).unwrap_or((None, 0, 0));

    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let entries = repo.statuses(Some(&mut opts))?;

    let mut files = Vec::with_capacity(entries.len());
    for entry in entries.iter() {
        let bits = entry.status();
        if bits.is_ignored() {
            // Skip ignored files entirely — they're noise for agents.
            continue;
        }
        let path = entry.path().unwrap_or_default().to_string();
        files.push(FileStatus {
            path,
            staged: stage_label(bits),
            working: work_label(bits),
            is_conflicted: bits.is_conflicted(),
            is_untracked: bits.is_wt_new() && !bits.is_index_new(),
            is_ignored: false,
        });
    }

    let is_clean = files.is_empty();
    Ok(RepoStatus {
        branch,
        head_sha,
        upstream,
        ahead,
        behind,
        is_clean,
        files,
    })
}

fn stage_label(s: Status) -> Option<&'static str> {
    if s.is_index_new() {
        Some("added")
    } else if s.is_index_modified() {
        Some("modified")
    } else if s.is_index_deleted() {
        Some("deleted")
    } else if s.is_index_renamed() {
        Some("renamed")
    } else if s.is_index_typechange() {
        Some("typechange")
    } else {
        None
    }
}

fn work_label(s: Status) -> Option<&'static str> {
    if s.is_wt_new() {
        Some("untracked")
    } else if s.is_wt_modified() {
        Some("modified")
    } else if s.is_wt_deleted() {
        Some("deleted")
    } else if s.is_wt_renamed() {
        Some("renamed")
    } else if s.is_wt_typechange() {
        Some("typechange")
    } else {
        None
    }
}

fn upstream_divergence(
    repo: &git2::Repository,
) -> Result<(Option<String>, usize, usize), GitError> {
    let head = repo.head()?;
    if !head.is_branch() {
        return Ok((None, 0, 0));
    }
    let branch = git2::Branch::wrap(head);
    let Ok(upstream) = branch.upstream() else {
        return Ok((None, 0, 0));
    };
    let upstream_name = upstream.name().ok().flatten().map(str::to_string);

    let local = branch.get().target();
    let remote = upstream.get().target();
    let (ahead, behind) = match (local, remote) {
        (Some(l), Some(r)) => repo.graph_ahead_behind(l, r).unwrap_or((0, 0)),
        _ => (0, 0),
    };
    Ok((upstream_name, ahead, behind))
}
