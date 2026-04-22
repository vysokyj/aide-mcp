use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// Caches per-workspace [`LspClient`] instances keyed by `(language, root)`.
///
/// The first call for a given workspace spawns the server; subsequent calls
/// reuse the same client so rust-analyzer keeps its index hot.
pub struct LspPool {
    clients: Mutex<HashMap<Key, Arc<LspClient>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    language: String,
    root: PathBuf,
}

impl LspPool {
    pub fn new() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
        }
    }

    /// Return a handle to the LSP client for `(language, root)`, spawning one
    /// at `server_binary` if none is cached yet.
    pub async fn get_or_spawn(
        &self,
        language: &str,
        root: &Path,
        server_binary: &Path,
    ) -> Result<Arc<LspClient>, LspPoolError> {
        let key = Key {
            language: language.to_string(),
            root: root.to_path_buf(),
        };

        if let Some(existing) = self.clients.lock().await.get(&key) {
            return Ok(existing.clone());
        }

        if !server_binary.exists() {
            return Err(LspPoolError::ServerMissing(server_binary.to_path_buf()));
        }

        let client = Arc::new(LspClient::spawn(server_binary, root).await?);
        self.clients.lock().await.insert(key, client.clone());
        Ok(client)
    }
}

impl Default for LspPool {
    fn default() -> Self {
        Self::new()
    }
}
