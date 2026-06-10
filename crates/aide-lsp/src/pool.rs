use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;
use tokio::sync::Mutex;

use crate::client::{LspClient, LspClientError};

#[derive(Debug, Error)]
pub enum LspPoolError {
    #[error(transparent)]
    Client(#[from] LspClientError),
    #[error("no LSP server binary configured for language `{0}`")]
    NoServer(String),
    #[error("LSP server binary not found at {0} — run project_setup first")]
    ServerMissing(PathBuf),
}

/// How long a cached client may sit unused before the idle reaper
/// shuts it down. Language servers hold 100–500 MB each; an agent that
/// hopped to another repo should not pin that memory forever.
pub const IDLE_TTL: Duration = Duration::from_secs(30 * 60);

/// How often [`spawn_idle_reaper`] sweeps the pool.
pub const REAP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Caches per-workspace [`LspClient`] instances keyed by `(language, root)`.
///
/// The first call for a given workspace spawns the server; subsequent calls
/// reuse the same client so rust-analyzer keeps its index hot. Clients
/// untouched for [`IDLE_TTL`] are evicted by the reaper task; the next
/// `get_or_spawn` for that workspace simply spawns a fresh server.
pub struct LspPool {
    clients: Mutex<HashMap<Key, Entry>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    language: String,
    root: PathBuf,
}

struct Entry {
    client: Arc<LspClient>,
    last_used: Instant,
}

impl LspPool {
    pub fn new() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
        }
    }

    /// Return a handle to the LSP client for `(language, root)`, spawning
    /// one at `server_binary` if none is cached yet. `server_args` are
    /// plugin-supplied launch flags (see `LanguagePlugin::lsp_spawn_args`).
    pub async fn get_or_spawn(
        &self,
        language: &str,
        root: &Path,
        server_binary: &Path,
        server_args: &[std::ffi::OsString],
    ) -> Result<Arc<LspClient>, LspPoolError> {
        let key = Key {
            language: language.to_string(),
            root: root.to_path_buf(),
        };

        {
            let mut clients = self.clients.lock().await;
            if let Some(existing) = clients.get_mut(&key) {
                if existing.client.is_alive() {
                    existing.last_used = Instant::now();
                    return Ok(existing.client.clone());
                }
                // The server died behind our back — without this check
                // every call would burn the full request timeout on a
                // dead process. Drop the corpse and spawn fresh below.
                tracing::warn!(
                    language,
                    root = %root.display(),
                    "cached LSP client is dead — respawning"
                );
                clients.remove(&key);
            }
        }

        if !server_binary.exists() {
            return Err(LspPoolError::ServerMissing(server_binary.to_path_buf()));
        }

        let client = Arc::new(LspClient::spawn(server_binary, server_args, root).await?);
        self.clients.lock().await.insert(
            key,
            Entry {
                client: client.clone(),
                last_used: Instant::now(),
            },
        );
        Ok(client)
    }

    /// Remove every client idle for longer than `ttl` and return the
    /// handles so the caller can run the shutdown handshake outside the
    /// pool lock. Outstanding `Arc` holders keep an evicted client alive
    /// until their in-flight request finishes.
    pub async fn take_idle(&self, ttl: Duration) -> Vec<Arc<LspClient>> {
        let now = Instant::now();
        let mut evicted = Vec::new();
        self.clients.lock().await.retain(|key, entry| {
            if now.duration_since(entry.last_used) > ttl {
                tracing::info!(
                    language = %key.language,
                    root = %key.root.display(),
                    "evicting idle LSP client"
                );
                evicted.push(entry.client.clone());
                false
            } else {
                true
            }
        });
        evicted
    }
}

/// Periodically evict and shut down clients idle for longer than
/// [`IDLE_TTL`]. Must be called from within a tokio runtime; the task
/// runs for the life of the process.
pub fn spawn_idle_reaper(pool: Arc<LspPool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(REAP_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires immediately; skip it so a server started
        // and immediately suspended does not sweep on resume.
        tick.tick().await;
        loop {
            tick.tick().await;
            for client in pool.take_idle(IDLE_TTL).await {
                let _ = client.shutdown().await;
            }
        }
    })
}

impl Default for LspPool {
    fn default() -> Self {
        Self::new()
    }
}
