//! Persistent state for the indexer daemon.
//!
//! Tracks every repo → every enqueued commit, its state, timestamps, and
//! the on-disk path of the produced `.scip` index. Flushed to disk
//! atomically on every mutation — calls are infrequent and durability
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

/// Result of a single `enqueue` call — tells the worker whether there is
/// fresh work to do or whether the commit was already known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// Commit is new or was previously in a terminal failed state — the
    /// worker should (re)index it.
    NeedsIndexing,
    /// Commit is already `Pending` or `InProgress` — no extra work to schedule.
    AlreadyQueued,
    /// Commit was already indexed; caller does not need to touch it.
    AlreadyReady,
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
    index_path: Option<String>,
}

impl CommitEntry {
    fn to_info(&self, sha: String) -> CommitInfo {
        CommitInfo {
            sha,
            state: self.state.clone(),
            enqueued_at_unix: self.enqueued_at_unix,
            indexed_at_unix: self.indexed_at_unix,
            index_path: self.index_path.clone(),
        }
    }
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

    /// Record a commit as needing to be indexed. The commit lands in
    /// [`IndexState::Pending`] unless it was already Ready. Any prior
    /// `Failed` state is cleared so the commit will be retried.
    ///
    /// Returns an [`EnqueueOutcome`] so the caller knows whether to
    /// schedule fresh work.
    pub async fn enqueue(&self, repo_root: &str, sha: &str) -> Result<EnqueueOutcome, StateError> {
        let now = now_unix();
        let mut guard = self.inner.lock().await;
        let repo = guard.state.repos.entry(repo_root.to_string()).or_default();
        repo.last_sha = Some(sha.to_string());

        let outcome = if let Some(existing) = repo.commits.get_mut(sha) {
            match &existing.state {
                IndexState::Ready => EnqueueOutcome::AlreadyReady,
                IndexState::Pending | IndexState::InProgress => EnqueueOutcome::AlreadyQueued,
                IndexState::Failed(_) => {
                    existing.state = IndexState::Pending;
                    existing.enqueued_at_unix = now;
                    EnqueueOutcome::NeedsIndexing
                }
            }
        } else {
            repo.commits.insert(
                sha.to_string(),
                CommitEntry {
                    state: IndexState::Pending,
                    enqueued_at_unix: now,
                    indexed_at_unix: None,
                    index_path: None,
                },
            );
            EnqueueOutcome::NeedsIndexing
        };

        let path = guard.path.clone();
        flush(&guard.state, &path)?;
        Ok(outcome)
    }

    pub async fn mark_in_progress(&self, repo_root: &str, sha: &str) -> Result<(), StateError> {
        self.mutate(repo_root, sha, |entry| {
            entry.state = IndexState::InProgress;
        })
        .await
    }

    pub async fn mark_ready(
        &self,
        repo_root: &str,
        sha: &str,
        index_path: PathBuf,
    ) -> Result<(), StateError> {
        let now = now_unix();
        let path_str = index_path.display().to_string();
        self.mutate(repo_root, sha, |entry| {
            entry.state = IndexState::Ready;
            entry.indexed_at_unix = Some(now);
            entry.index_path = Some(path_str.clone());
        })
        .await
    }

    pub async fn mark_failed(
        &self,
        repo_root: &str,
        sha: &str,
        reason: String,
    ) -> Result<(), StateError> {
        self.mutate(repo_root, sha, |entry| {
            entry.state = IndexState::Failed(reason.clone());
            entry.indexed_at_unix = None;
        })
        .await
    }

    async fn mutate<F>(&self, repo_root: &str, sha: &str, mut f: F) -> Result<(), StateError>
    where
        F: FnMut(&mut CommitEntry),
    {
        let mut guard = self.inner.lock().await;
        if let Some(repo) = guard.state.repos.get_mut(repo_root) {
            if let Some(entry) = repo.commits.get_mut(sha) {
                f(entry);
            }
        }
        let path = guard.path.clone();
        flush(&guard.state, &path)
    }

    pub async fn status(&self, repo_root: &str, sha: Option<&str>) -> Option<CommitInfo> {
        let guard = self.inner.lock().await;
        let repo = guard.state.repos.get(repo_root)?;
        let target_sha = match sha {
            Some(s) => s.to_string(),
            None => repo.last_sha.clone()?,
        };
        let entry = repo.commits.get(&target_sha)?;
        Some(entry.to_info(target_sha))
    }

    pub async fn last_known(&self, repo_root: &str) -> Option<CommitInfo> {
        let guard = self.inner.lock().await;
        let repo = guard.state.repos.get(repo_root)?;
        let sha = repo.last_sha.clone()?;
        let entry = repo.commits.get(&sha)?;
        Some(entry.to_info(sha))
    }

    /// Return every (`repo_root`, sha) pair that is still `Pending` or
    /// `InProgress`. The daemon calls this on start-up so that commits
    /// interrupted by an earlier crash get retried.
    pub async fn recoverable_jobs(&self) -> Vec<(String, String)> {
        let guard = self.inner.lock().await;
        let mut out = Vec::new();
        for (repo_root, repo) in &guard.state.repos {
            for (sha, entry) in &repo.commits {
                if matches!(entry.state, IndexState::Pending | IndexState::InProgress) {
                    out.push((repo_root.clone(), sha.clone()));
                }
            }
        }
        out
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
    async fn new_enqueue_lands_as_pending() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let store = Store::load(&path).unwrap();

        let outcome = store.enqueue("/repo", "abc").await.unwrap();
        assert_eq!(outcome, EnqueueOutcome::NeedsIndexing);

        let info = store.status("/repo", None).await.unwrap();
        assert_eq!(info.sha, "abc");
        assert_eq!(info.state, IndexState::Pending);
    }

    #[tokio::test]
    async fn mark_ready_sets_index_path_and_timestamp() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let store = Store::load(&path).unwrap();
        store.enqueue("/repo", "abc").await.unwrap();
        store.mark_in_progress("/repo", "abc").await.unwrap();
        store
            .mark_ready("/repo", "abc", PathBuf::from("/out.scip"))
            .await
            .unwrap();

        let info = store.status("/repo", None).await.unwrap();
        assert_eq!(info.state, IndexState::Ready);
        assert_eq!(info.index_path.as_deref(), Some("/out.scip"));
        assert!(info.indexed_at_unix.is_some());
    }

    #[tokio::test]
    async fn re_enqueue_of_ready_commit_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let store = Store::load(&path).unwrap();
        store.enqueue("/repo", "abc").await.unwrap();
        store
            .mark_ready("/repo", "abc", PathBuf::from("/out.scip"))
            .await
            .unwrap();

        let outcome = store.enqueue("/repo", "abc").await.unwrap();
        assert_eq!(outcome, EnqueueOutcome::AlreadyReady);
    }

    #[tokio::test]
    async fn re_enqueue_of_failed_commit_retries() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let store = Store::load(&path).unwrap();
        store.enqueue("/repo", "abc").await.unwrap();
        store
            .mark_failed("/repo", "abc", "boom".into())
            .await
            .unwrap();

        let outcome = store.enqueue("/repo", "abc").await.unwrap();
        assert_eq!(outcome, EnqueueOutcome::NeedsIndexing);
        let info = store.status("/repo", None).await.unwrap();
        assert_eq!(info.state, IndexState::Pending);
    }

    #[tokio::test]
    async fn state_persists_across_reload() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        {
            let store = Store::load(&path).unwrap();
            store.enqueue("/repo", "abc").await.unwrap();
            store
                .mark_ready("/repo", "abc", PathBuf::from("/x.scip"))
                .await
                .unwrap();
        }
        let store = Store::load(&path).unwrap();
        let last = store.last_known("/repo").await.unwrap();
        assert_eq!(last.sha, "abc");
        assert_eq!(last.state, IndexState::Ready);
        assert_eq!(last.index_path.as_deref(), Some("/x.scip"));
    }

    #[tokio::test]
    async fn recoverable_jobs_reports_pending_and_in_progress() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let store = Store::load(&path).unwrap();
        store.enqueue("/repo", "pending").await.unwrap();
        store.enqueue("/repo", "running").await.unwrap();
        store.mark_in_progress("/repo", "running").await.unwrap();
        store.enqueue("/repo", "done").await.unwrap();
        store
            .mark_ready("/repo", "done", PathBuf::from("/d.scip"))
            .await
            .unwrap();

        let mut jobs = store.recoverable_jobs().await;
        jobs.sort();
        assert_eq!(
            jobs,
            vec![
                ("/repo".into(), "pending".into()),
                ("/repo".into(), "running".into()),
            ]
        );
    }
}
