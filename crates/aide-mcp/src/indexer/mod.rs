//! In-process SCIP indexer.
//!
//! The MCP server owns a single [`Indexer`] that holds a persistent
//! [`Store`] and a background tokio task processing jobs off an mpsc
//! channel. Tool handlers just call [`Indexer::enqueue_head`] or
//! [`Indexer::enqueue`] and return — indexing runs asynchronously in
//! the background.

mod state;
mod worker;

use std::path::{Path, PathBuf};

use aide_core::AidePaths;
use aide_git::resolve_head;
use aide_proto::CommitInfo;
use anyhow::Result;
use tokio::sync::mpsc;

use self::state::{EnqueueOutcome, Store};
use self::worker::Job;

#[derive(Clone)]
pub struct Indexer {
    store: Store,
    jobs: mpsc::UnboundedSender<Job>,
}

impl Indexer {
    /// Spin up the indexer: load (or create) the state file, spawn the
    /// background worker, and re-enqueue anything that was Pending or
    /// `InProgress` when the previous MCP instance exited.
    pub fn start(paths: &AidePaths) -> Result<Self> {
        std::fs::create_dir_all(paths.queue())?;
        std::fs::create_dir_all(paths.scip())?;
        let state_path: PathBuf = paths.queue().join("indexer_state.json");
        let store = Store::load(&state_path)?;
        let jobs = worker::spawn(paths.clone(), store.clone());

        let resume_store = store.clone();
        let resume_jobs = jobs.clone();
        tokio::spawn(async move {
            for (repo_root, sha) in resume_store.recoverable_jobs().await {
                tracing::info!(repo = %repo_root, sha = %sha, "recovering interrupted job");
                let _ = resume_jobs.send(Job { repo_root, sha });
            }
        });

        Ok(Self { store, jobs })
    }

    /// Resolve the `HEAD` of the repo at `path` and enqueue it if we
    /// don't already have a Ready index for that SHA. Errors (non-git
    /// path, unborn branch, I/O) are swallowed — this is a best-effort
    /// trigger meant to live inside hot tool paths.
    pub async fn enqueue_head(&self, path: &Path) {
        let (repo_root, sha) = match resolve_head(path) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "skip enqueue_head");
                return;
            }
        };
        if let Err(e) = self
            .enqueue_inner(repo_root.display().to_string(), sha)
            .await
        {
            tracing::debug!(error = %e, "enqueue_head failed");
        }
    }

    /// Enqueue `sha` in `repo_root`. The worker picks it up asynchronously
    /// if it is new or previously failed; already-queued / already-ready
    /// commits produce no extra work.
    pub async fn enqueue(&self, repo_root: String, sha: String) -> Result<CommitInfo> {
        self.enqueue_inner(repo_root.clone(), sha.clone()).await?;
        // status is guaranteed Some because enqueue just wrote an entry.
        self.store
            .status(&repo_root, Some(&sha))
            .await
            .ok_or_else(|| anyhow::anyhow!("state for {sha} missing immediately after enqueue"))
    }

    async fn enqueue_inner(&self, repo_root: String, sha: String) -> Result<EnqueueOutcome> {
        let outcome = self.store.enqueue(&repo_root, &sha).await?;
        if outcome == EnqueueOutcome::NeedsIndexing {
            self.jobs
                .send(Job { repo_root, sha })
                .map_err(|e| anyhow::anyhow!("indexer worker channel closed: {e}"))?;
        }
        Ok(outcome)
    }

    pub async fn status(&self, repo_root: &str, sha: Option<&str>) -> Option<CommitInfo> {
        self.store.status(repo_root, sha).await
    }

    pub async fn last_known(&self, repo_root: &str) -> Option<CommitInfo> {
        self.store.last_known(repo_root).await
    }
}
