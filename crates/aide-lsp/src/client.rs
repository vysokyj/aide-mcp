use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lsp_types::notification::{Notification as LspNotification, PublishDiagnostics};
use lsp_types::request::{Initialize, Request as LspRequest, Shutdown};
use lsp_types::{
    ClientCapabilities, Diagnostic, InitializeParams, InitializedParams, PublishDiagnosticsParams,
    Uri, WorkspaceFolder,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

use crate::framing::{read_message, write_message, FramingError};

#[derive(Debug, Error)]
pub enum LspClientError {
    #[error("spawn failed: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("server exited before initialize")]
    EarlyExit,
    #[error("transport: {0}")]
    Framing(#[from] FramingError),
    #[error("serialize/deserialize: {0}")]
    Json(#[from] serde_json::Error),
    #[error("lsp error ({code}): {message}")]
    LspError { code: i64, message: String },
    #[error("response for unknown id {0}")]
    OrphanResponse(i64),
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    #[error("server dropped response channel")]
    Cancelled,
    #[error("server URI invalid: {0}")]
    Uri(String),
    #[error("I/O: {0}")]
    Io(#[source] std::io::Error),
}

impl LspClientError {
    pub fn from_io(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, LspClientError>>>>>;
type Diagnostics = Arc<Mutex<HashMap<Uri, Vec<Diagnostic>>>>;
type OpenedDocs = Arc<Mutex<HashMap<Uri, OpenedDocument>>>;

/// Tracks a single text document the server currently has open.
#[derive(Debug, Clone)]
pub struct OpenedDocument {
    pub version: i32,
    pub text: String,
}

/// A running LSP server connected over stdio.
pub struct LspClient {
    next_id: AtomicI64,
    writer: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    diagnostics: Diagnostics,
    opened: OpenedDocs,
    child: Mutex<Child>,
    request_timeout: Duration,
}

impl LspClient {
    /// Spawn `server_path` (e.g. a rust-analyzer binary) with optional
    /// extra launch arguments and run the LSP initialize handshake
    /// against `workspace_root`. The plugin-provided `server_args` let
    /// servers that need per-workspace flags (e.g. JDT-LS `-data`) be
    /// launched correctly.
    pub async fn spawn(
        server_path: &Path,
        server_args: &[std::ffi::OsString],
        workspace_root: &Path,
    ) -> Result<Self, LspClientError> {
        let mut child = Command::new(server_path)
            .args(server_args)
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(LspClientError::Spawn)?;

        let stdin = child.stdin.take().ok_or(LspClientError::EarlyExit)?;
        let stdout = child.stdout.take().ok_or(LspClientError::EarlyExit)?;
        let stderr = child.stderr.take();

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics: Diagnostics = Arc::new(Mutex::new(HashMap::new()));

        spawn_reader(stdout, pending.clone(), diagnostics.clone());
        if let Some(stderr) = stderr {
            spawn_stderr_drain(stderr);
        }

        let client = Self {
            next_id: AtomicI64::new(1),
            writer: Arc::new(Mutex::new(stdin)),
            pending,
            diagnostics,
            opened: Arc::new(Mutex::new(HashMap::new())),
            child: Mutex::new(child),
            request_timeout: Duration::from_secs(30),
        };

        client.initialize(workspace_root).await?;
        Ok(client)
    }

    async fn initialize(&self, root: &Path) -> Result<(), LspClientError> {
        let root_uri = path_to_uri(root)?;
        let folder = WorkspaceFolder {
            uri: root_uri.clone(),
            name: root.file_name().map_or_else(
                || "workspace".to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
        };

        #[allow(
            deprecated,
            reason = "root_uri is deprecated but still expected by many servers"
        )]
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(root_uri),
            capabilities: ClientCapabilities::default(),
            workspace_folders: Some(vec![folder]),
            client_info: Some(lsp_types::ClientInfo {
                name: "aide-mcp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            ..Default::default()
        };

        let _: <Initialize as LspRequest>::Result = self.request::<Initialize>(params).await?;
        self.notify_raw("initialized", InitializedParams {}).await?;
        Ok(())
    }

    /// Send a typed LSP request and await its typed response.
    pub async fn request<R>(&self, params: R::Params) -> Result<R::Result, LspClientError>
    where
        R: LspRequest,
        R::Params: Serialize,
        R::Result: DeserializeOwned,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": R::METHOD,
            "params": params,
        });
        let bytes = serde_json::to_vec(&envelope)?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        tracing::debug!(id, method = R::METHOD, "lsp tx request");
        {
            let mut w = self.writer.lock().await;
            write_message(&mut *w, &bytes).await?;
        }

        let value = match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(Ok(v))) => v,
            Ok(Ok(Err(e))) => return Err(e),
            Ok(Err(_)) => return Err(LspClientError::Cancelled),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                return Err(LspClientError::Timeout(self.request_timeout));
            }
        };

        // `null` results are valid for several LSP methods (e.g. hover on a
        // non-token position). Deserialize through Value to allow that.
        let result: R::Result = serde_json::from_value(value)?;
        Ok(result)
    }

    /// Send a typed LSP notification (no response).
    pub async fn notify<N>(&self, params: N::Params) -> Result<(), LspClientError>
    where
        N: LspNotification,
        N::Params: Serialize,
    {
        self.notify_raw(N::METHOD, params).await
    }

    async fn notify_raw<P>(&self, method: &str, params: P) -> Result<(), LspClientError>
    where
        P: Serialize,
    {
        let envelope = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let bytes = serde_json::to_vec(&envelope)?;
        tracing::debug!(method, "lsp tx notification");
        let mut w = self.writer.lock().await;
        write_message(&mut *w, &bytes).await?;
        Ok(())
    }

    /// Access the shared open-document tracker used by [`crate::ops`] to emit
    /// `textDocument/didOpen`/`didChange` correctly.
    pub fn opened_documents(&self) -> &Mutex<HashMap<Uri, OpenedDocument>> {
        &self.opened
    }

    /// Snapshot of the most recently published diagnostics for `uri`.
    pub async fn diagnostics_for(&self, uri: &Uri) -> Vec<Diagnostic> {
        self.diagnostics
            .lock()
            .await
            .get(uri)
            .cloned()
            .unwrap_or_default()
    }

    /// Attempt a graceful `shutdown` → `exit` handshake, then kill the child.
    pub async fn shutdown(&self) -> Result<(), LspClientError> {
        let _ = self.request::<Shutdown>(()).await;
        let _ = self.notify_raw("exit", Value::Null).await;
        let _ = self.child.lock().await.kill().await;
        Ok(())
    }
}

fn spawn_reader(stdout: tokio::process::ChildStdout, pending: Pending, diagnostics: Diagnostics) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let body = match read_message(&mut reader).await {
                Ok(b) => b,
                Err(FramingError::Eof) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "lsp read failure");
                    break;
                }
            };
            let msg: Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "lsp malformed json");
                    continue;
                }
            };
            let method = msg
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("<response>");
            let id = msg.get("id").and_then(Value::as_i64);
            if tracing::enabled!(tracing::Level::TRACE) {
                tracing::trace!(?id, method, body = %msg, "lsp rx");
            } else {
                tracing::debug!(?id, method, "lsp rx");
            }
            dispatch(msg, &pending, &diagnostics).await;
        }
        tracing::debug!("lsp reader exited");
    });
}

async fn dispatch(msg: Value, pending: &Pending, diagnostics: &Diagnostics) {
    if let Some(id) = msg.get("id").and_then(Value::as_i64) {
        let tx = pending.lock().await.remove(&id);
        if let Some(tx) = tx {
            let resolved = if let Some(err) = msg.get("error") {
                Err(LspClientError::LspError {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
            } else {
                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
            };
            let _ = tx.send(resolved);
        }
        return;
    }

    // notification
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method == PublishDiagnostics::METHOD {
        if let Some(params) = msg.get("params") {
            if let Ok(params) = serde_json::from_value::<PublishDiagnosticsParams>(params.clone()) {
                diagnostics
                    .lock()
                    .await
                    .insert(params.uri, params.diagnostics);
            }
        }
    }
}

fn spawn_stderr_drain(stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let trimmed = line.trim_end();
                    if !trimmed.is_empty() {
                        tracing::debug!("lsp stderr: {}", trimmed);
                    }
                }
            }
        }
    });
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // `kill_on_drop(true)` on the Command handles this, but explicit is
        // clearer in logs.
        tracing::debug!("LspClient dropped");
    }
}

/// Build an `lsp_types::Uri` from a filesystem path by routing through
/// `url::Url::from_file_path` to get correct percent-encoding.
pub(crate) fn path_to_uri(path: &Path) -> Result<Uri, LspClientError> {
    let url = url::Url::from_file_path(path)
        .map_err(|()| LspClientError::Uri(path.display().to_string()))?;
    Uri::from_str(url.as_str()).map_err(|_| LspClientError::Uri(url.to_string()))
}
