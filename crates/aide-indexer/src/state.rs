//! Persistent state for the indexer daemon.
//!
//! Tracks every repo → every enqueued commit, its state, timestamps. Flushed
//! to disk atomically on every mutation — calls are infrequent and durability
//! matters more than throughput.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aide_proto::{CommitInfo, IndexState};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexerState {
    repos: HashMap<String, RepoState>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RepoState {
    last_sha: Option<String>,
    commits: HashMap<String, CommitEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CommitEntry {
    state: IndexState,
    enqueued_at_unix: i64,
    indexed_at_unix: Option<i64>,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Mutex<StoreInner>>,
}

struct StoreInner {
    state: IndexerState,
    path: PathBuf,
}

impl Store {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, StateError> {
        let path = path.into();
        let state = if path.exists() {
            let bytes = std::fs::read(&path)?;
            serde_json::from_slice(&bytes)?
        } else {
            IndexerState::default()
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(StoreInner { state, path })),
        })
    }

    /// Record a new (or repeated) enqueue for a commit. In v0.3 we immediately
    /// mark the commit as `Ready` because actual SCIP indexing lands in v0.4 —
    /// the daemon still owns the bookkeeping; future versions flip the
    /// transition to `Pending` → `InProgress` → `Ready` driven by the real indexer.
    pub async fn enqueue(&self, repo_root: &str, sha: &str) -> Result<(), StateError> {
        let now = now_unix();
        let mut guard = self.inner.lock().await;
        let repo = guard.state.repos.entry(repo_root.to_string()).or_default();
        repo.last_sha = Some(sha.to_string());
        let entry = repo
            .commits
            .entry(sha.to_string())
            .or_insert_with(|| CommitEntry {
                state: IndexState::Pending,
                enqueued_at_unix: now,
                indexed_at_unix: None,
            });
        // v0.3 shortcut: pretend the commit is indexed immediately so the
        // rest of the plumbing (status, last-known queries) can be exercised
        // end-to-end. Replace when real SCIP builds land in v0.4.
        entry.state = IndexState::Ready;
        entry.indexed_at_unix.get_or_insert(now);
        let path = guard.path.clone();
        flush(&guard.state, &path)
    }

    pub async fn status(&self, repo_root: &str, sha: Option<&str>) -> Option<(String, CommitInfo)> {
        let guard = self.inner.lock().await;
        let repo = guard.state.repos.get(repo_root)?;
        let target_sha = match sha {
            Some(s) => s.to_string(),
            None => repo.last_sha.clone()?,
        };
        let entry = repo.commits.get(&target_sha)?;
        Some((
            target_sha.clone(),
            CommitInfo {
                sha: target_sha,
                state: entry.state.clone(),
                enqueued_at_unix: entry.enqueued_at_unix,
                indexed_at_unix: entry.indexed_at_unix,
            },
        ))
    }

    pub async fn last_known(&self, repo_root: &str) -> Option<CommitInfo> {
        let guard = self.inner.lock().await;
        let repo = guard.state.repos.get(repo_root)?;
        let sha = repo.last_sha.clone()?;
        let entry = repo.commits.get(&sha)?;
        Some(CommitInfo {
            sha,
            state: entry.state.clone(),
            enqueued_at_unix: entry.enqueued_at_unix,
            indexed_at_unix: entry.indexed_at_unix,
        })
    }
}

fn flush(state: &IndexerState, path: &Path) -> Result<(), StateError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(state)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn enqueue_then_status() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let store = Store::load(&path).unwrap();

        store.enqueue("/repo", "abc").await.unwrap();
        let (sha, info) = store.status("/repo", None).await.unwrap();
        assert_eq!(sha, "abc");
        assert_eq!(info.state, IndexState::Ready);

        let (sha2, info2) = store.status("/repo", Some("abc")).await.unwrap();
        assert_eq!(sha2, "abc");
        assert_eq!(info2.state, IndexState::Ready);

        assert!(store.status("/repo", Some("missing")).await.is_none());
        assert!(store.status("/other", None).await.is_none());
    }

    #[tokio::test]
    async fn state_persists_across_reload() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        {
            let store = Store::load(&path).unwrap();
            store.enqueue("/repo", "abc").await.unwrap();
        }
        let store = Store::load(&path).unwrap();
        let last = store.last_known("/repo").await.unwrap();
        assert_eq!(last.sha, "abc");
    }

    #[tokio::test]
    async fn last_known_follows_latest_enqueue() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let store = Store::load(&path).unwrap();
        store.enqueue("/repo", "first").await.unwrap();
        store.enqueue("/repo", "second").await.unwrap();
        let last = store.last_known("/repo").await.unwrap();
        assert_eq!(last.sha, "second");
    }
}
