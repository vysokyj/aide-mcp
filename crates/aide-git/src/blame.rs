use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{open_repo, GitError};

#[derive(Debug, Clone, Serialize)]
pub struct BlameLine {
    pub line: u32,
    pub sha: String,
    pub short: String,
    pub author: String,
    pub time: i64,
    pub summary: String,
}

/// Line-by-line blame of `file` (relative to the repository root).
pub fn blame(root: &Path, file: &Path) -> Result<Vec<BlameLine>, GitError> {
    let repo = open_repo(root)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::Decode("bare repository has no workdir".to_string()))?;

    let relative: PathBuf = if file.is_absolute() {
        file.strip_prefix(workdir)
            .map_err(|_| GitError::Decode(format!("{} is outside workdir", file.display())))?
            .to_path_buf()
    } else {
        file.to_path_buf()
    };

    let blame = repo.blame_file(&relative, None)?;
    let mut out = Vec::new();
    for hunk in blame.iter() {
        let sha = hunk.final_commit_id().to_string();
        let commit = repo.find_commit(hunk.final_commit_id()).ok();
        let (author, time, summary) = match &commit {
            Some(c) => (
                c.author().name().unwrap_or("").to_string(),
                c.time().seconds(),
                c.summary().unwrap_or("").to_string(),
            ),
            None => (String::new(), 0, String::new()),
        };
        let start = u32::try_from(hunk.final_start_line()).unwrap_or(u32::MAX);
        let hunk_len = u32::try_from(hunk.lines_in_hunk()).unwrap_or(u32::MAX);
        for offset in 0..hunk_len {
            out.push(BlameLine {
                line: start.saturating_add(offset),
                sha: sha.clone(),
                short: sha.chars().take(7).collect(),
                author: author.clone(),
                time,
                summary: summary.clone(),
            });
        }
    }
    Ok(out)
}
