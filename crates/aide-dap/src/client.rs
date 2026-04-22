use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aide_proto::framing::{read_message, write_message, FramingError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex, Notify};
use tokio::time::timeout;

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<ResponseResult>>>>;
type ResponseResult = Result<Option<Value>, DapClientError>;

#[derive(Debug, Error)]
pub enum DapClientError {
    #[error("spawn failed: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("adapter exited before initialize")]
    EarlyExit,
    #[error("transport: {0}")]
    Framing(#[from] FramingError),
    #[error("serialize/deserialize: {0}")]
    Json(#[from] serde_json::Error),
    #[error("dap error on `{command}`: {message}")]
    DapError { command: String, message: String },
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    #[error("adapter closed response channel")]
    Cancelled,
    #[error("I/O: {0}")]
    Io(#[source] std::io::Error),
}

/// Condensed `Capabilities` from the `initialize` response. Only the
/// bits MCP tools currently care about are carried through.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors DAP protocol capability flags"
)]
pub struct DapCapabilities {
    #[serde(default)]
    pub supports_configuration_done_request: bool,
    #[serde(default)]
    pub supports_evaluate_for_hovers: bool,
    #[serde(default)]
    pub supports_set_variable: bool,
    #[serde(default)]
    pub supports_conditional_breakpoints: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoppedInfo {
    pub thread_id: Option<i64>,
    pub reason: String,
    pub description: Option<String>,
    pub text: Option<String>,
    pub all_threads_stopped: bool,
    pub hit_breakpoint_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub line: i64,
    #[serde(default)]
    pub column: i64,
    pub source_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scope {
    pub name: String,
    #[serde(rename = "variablesReference")]
    pub variables_reference: i64,
    #[serde(default)]
    pub expensive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    pub value: String,
    #[serde(rename = "type", default)]
    pub type_name: Option<String>,
    #[serde(rename = "variablesReference", default)]
    pub variables_reference: i64,
}

#[derive(Debug, Clone, Default)]
struct EventState {
    /// `Some` while the debuggee is paused; `None` while running.
    stopped: Option<StoppedInfo>,
    /// Adapter has emitted `initialized` — safe to send `setBreakpoints`
    /// + `configurationDone`.
    initialized: bool,
    /// Adapter has emitted `terminated` / `exited`.
    terminated: bool,
}

/// A running DAP adapter connected over stdio.
pub struct DapClient {
    next_seq: AtomicI64,
    writer: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    child: Mutex<Child>,
    state: Arc<Mutex<EventState>>,
    state_notify: Arc<Notify>,
    closed: Arc<AtomicBool>,
    request_timeout: Duration,
}

impl DapClient {
    /// Spawn `adapter_path` with `adapter_args`. `cwd` becomes the
    /// adapter's working directory. Call [`initialize`](Self::initialize)
    /// right after to complete the DAP handshake.
    #[allow(clippy::unused_async, reason = "kept async for symmetry with LspClient::spawn")]
    pub async fn spawn(
        adapter_path: &Path,
        adapter_args: &[OsString],
        cwd: &Path,
    ) -> Result<Self, DapClientError> {
        let mut child = Command::new(adapter_path)
            .args(adapter_args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(DapClientError::Spawn)?;

        let stdin = child.stdin.take().ok_or(DapClientError::EarlyExit)?;
        let stdout = child.stdout.take().ok_or(DapClientError::EarlyExit)?;
        let stderr = child.stderr.take();

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let state = Arc::new(Mutex::new(EventState::default()));
        let state_notify = Arc::new(Notify::new());
        let closed = Arc::new(AtomicBool::new(false));

        spawn_reader(
            stdout,
            pending.clone(),
            state.clone(),
            state_notify.clone(),
            closed.clone(),
        );
        if let Some(stderr) = stderr {
            spawn_stderr_drain(stderr);
        }

        Ok(Self {
            next_seq: AtomicI64::new(1),
            writer: Arc::new(Mutex::new(stdin)),
            pending,
            child: Mutex::new(child),
            state,
            state_notify,
            closed,
            request_timeout: Duration::from_secs(30),
        })
    }

    /// Send a DAP request but return a receiver for the response
    /// instead of awaiting it. Useful for requests whose response does
    /// not come back until after another request has completed — the
    /// `launch` / `configurationDone` dance being the canonical case.
    pub async fn request_rx(
        &self,
        command: &str,
        arguments: Value,
    ) -> Result<oneshot::Receiver<ResponseResult>, DapClientError> {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let msg = json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        let bytes = serde_json::to_vec(&msg)?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(seq, tx);

        let mut writer = self.writer.lock().await;
        write_message(&mut *writer, &bytes).await?;
        Ok(rx)
    }

    /// Send a DAP request and await its response. Returns the `body`
    /// field (`None` for empty-body responses).
    pub async fn request(
        &self,
        command: &str,
        arguments: Value,
    ) -> Result<Option<Value>, DapClientError> {
        let rx = self.request_rx(command, arguments).await?;
        match timeout(self.request_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(DapClientError::Cancelled),
            Err(_) => Err(DapClientError::Timeout(self.request_timeout)),
        }
    }

    /// Await a pending response receiver (returned by
    /// [`request_rx`](Self::request_rx)) with the client's default
    /// request timeout.
    pub async fn await_response(
        &self,
        rx: oneshot::Receiver<ResponseResult>,
    ) -> Result<Option<Value>, DapClientError> {
        match timeout(self.request_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(DapClientError::Cancelled),
            Err(_) => Err(DapClientError::Timeout(self.request_timeout)),
        }
    }

    pub async fn initialize(&self, client_id: &str) -> Result<DapCapabilities, DapClientError> {
        let body = self
            .request(
                "initialize",
                json!({
                    "clientID": client_id,
                    "clientName": "aide-mcp",
                    "adapterID": "aide",
                    "linesStartAt1": true,
                    "columnsStartAt1": true,
                    "pathFormat": "path",
                    "supportsRunInTerminalRequest": false,
                    "supportsStartDebuggingRequest": false,
                }),
            )
            .await?;
        let caps = match body {
            Some(v) => serde_json::from_value(v).unwrap_or_default(),
            None => DapCapabilities::default(),
        };
        Ok(caps)
    }

    /// Send the `launch` request and return its response receiver
    /// without awaiting. Per the DAP spec the launch response only
    /// arrives after the client has sent `configurationDone`, so
    /// callers must drive the rest of the launch dance before awaiting
    /// this receiver.
    pub async fn launch_start(
        &self,
        arguments: Value,
    ) -> Result<oneshot::Receiver<ResponseResult>, DapClientError> {
        self.request_rx("launch", arguments).await
    }

    pub async fn set_breakpoints(
        &self,
        source_path: &str,
        lines: &[i64],
    ) -> Result<Value, DapClientError> {
        let breakpoints: Vec<Value> = lines.iter().map(|l| json!({ "line": l })).collect();
        let body = self
            .request(
                "setBreakpoints",
                json!({
                    "source": { "path": source_path },
                    "lines": lines,
                    "breakpoints": breakpoints,
                }),
            )
            .await?;
        Ok(body.unwrap_or(Value::Null))
    }

    pub async fn configuration_done(&self) -> Result<(), DapClientError> {
        self.request("configurationDone", json!({})).await?;
        Ok(())
    }

    pub async fn continue_thread(&self, thread_id: i64) -> Result<(), DapClientError> {
        self.request("continue", json!({ "threadId": thread_id }))
            .await?;
        // Flip to running so the next wait_for_stopped actually blocks.
        let mut guard = self.state.lock().await;
        guard.stopped = None;
        drop(guard);
        Ok(())
    }

    pub async fn stack_trace(&self, thread_id: i64) -> Result<Vec<StackFrame>, DapClientError> {
        let body = self
            .request(
                "stackTrace",
                json!({ "threadId": thread_id, "startFrame": 0, "levels": 50 }),
            )
            .await?;
        let raw_frames = body
            .as_ref()
            .and_then(|v| v.get("stackFrames"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(raw_frames.iter().map(to_frame).collect())
    }

    pub async fn scopes(&self, frame_id: i64) -> Result<Vec<Scope>, DapClientError> {
        let body = self
            .request("scopes", json!({ "frameId": frame_id }))
            .await?;
        let raw = body
            .as_ref()
            .and_then(|v| v.get("scopes"))
            .cloned()
            .unwrap_or(Value::Array(vec![]));
        Ok(serde_json::from_value(raw).unwrap_or_default())
    }

    pub async fn variables(
        &self,
        variables_reference: i64,
    ) -> Result<Vec<Variable>, DapClientError> {
        let body = self
            .request(
                "variables",
                json!({ "variablesReference": variables_reference }),
            )
            .await?;
        let raw = body
            .as_ref()
            .and_then(|v| v.get("variables"))
            .cloned()
            .unwrap_or(Value::Array(vec![]));
        Ok(serde_json::from_value(raw).unwrap_or_default())
    }

    pub async fn evaluate(
        &self,
        expression: &str,
        frame_id: Option<i64>,
    ) -> Result<Value, DapClientError> {
        let mut args = json!({
            "expression": expression,
            "context": "repl",
        });
        if let Some(f) = frame_id {
            args["frameId"] = json!(f);
        }
        Ok(self.request("evaluate", args).await?.unwrap_or(Value::Null))
    }

    pub async fn disconnect(&self) -> Result<(), DapClientError> {
        let _ = self
            .request(
                "disconnect",
                json!({ "terminateDebuggee": true, "restart": false }),
            )
            .await;
        // Give the adapter a moment to shut down cleanly.
        let mut child = self.child.lock().await;
        let _ = timeout(Duration::from_secs(2), child.wait()).await;
        let _ = child.kill().await;
        Ok(())
    }

    /// Wait until the adapter emits `initialized`. Called right after
    /// `launch` and before `setBreakpoints` / `configurationDone`.
    pub async fn wait_for_initialized(&self, dur: Duration) -> Result<(), DapClientError> {
        let start = std::time::Instant::now();
        loop {
            if self.state.lock().await.initialized {
                return Ok(());
            }
            if self.closed.load(Ordering::SeqCst) {
                return Err(DapClientError::Cancelled);
            }
            let remaining = dur.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(DapClientError::Timeout(dur));
            }
            let notified = self.state_notify.notified();
            if timeout(remaining, notified).await.is_err() {
                return Err(DapClientError::Timeout(dur));
            }
        }
    }

    /// Wait until the debuggee is paused. Returns the most recent
    /// [`StoppedInfo`] or errors out on timeout / adapter close.
    pub async fn wait_for_stopped(&self, dur: Duration) -> Result<StoppedInfo, DapClientError> {
        let start = std::time::Instant::now();
        loop {
            {
                let guard = self.state.lock().await;
                if let Some(info) = guard.stopped.clone() {
                    return Ok(info);
                }
                if guard.terminated {
                    return Err(DapClientError::Cancelled);
                }
            }
            let remaining = dur.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(DapClientError::Timeout(dur));
            }
            let notified = self.state_notify.notified();
            if timeout(remaining, notified).await.is_err() {
                return Err(DapClientError::Timeout(dur));
            }
        }
    }

    /// Current stopped state (non-blocking).
    pub async fn current_stopped(&self) -> Option<StoppedInfo> {
        self.state.lock().await.stopped.clone()
    }
}

fn to_frame(raw: &Value) -> StackFrame {
    let source_path = raw
        .get("source")
        .and_then(|s| s.get("path"))
        .and_then(Value::as_str)
        .map(ToString::to_string);
    StackFrame {
        id: raw.get("id").and_then(Value::as_i64).unwrap_or(0),
        name: raw
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        line: raw.get("line").and_then(Value::as_i64).unwrap_or(0),
        column: raw.get("column").and_then(Value::as_i64).unwrap_or(0),
        source_path,
    }
}

fn spawn_reader(
    stdout: tokio::process::ChildStdout,
    pending: Pending,
    state: Arc<Mutex<EventState>>,
    state_notify: Arc<Notify>,
    closed: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        loop {
            let Ok(bytes) = read_message(&mut reader).await else {
                break;
            };
            let msg: Value = match serde_json::from_slice(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "dap: bad JSON");
                    continue;
                }
            };
            match msg.get("type").and_then(Value::as_str) {
                Some("response") => handle_response(msg, &pending).await,
                Some("event") => handle_event(msg, &state, &state_notify).await,
                Some("request") => {
                    // Reverse requests (e.g. runInTerminal). We declined them
                    // in the initialize args, so we should not see them; if
                    // we do, reply with a polite failure.
                    let seq = msg.get("seq").and_then(Value::as_i64).unwrap_or(0);
                    let command = msg
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    tracing::debug!(%command, "dap: ignoring reverse request");
                    let _ = (seq, command);
                }
                _ => {}
            }
        }
        closed.store(true, Ordering::SeqCst);
        state_notify.notify_waiters();
    });
}

async fn handle_response(msg: Value, pending: &Pending) {
    let Some(req_seq) = msg.get("request_seq").and_then(Value::as_i64) else {
        return;
    };
    let command = msg
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let success = msg.get("success").and_then(Value::as_bool).unwrap_or(false);
    let message = msg
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let body = msg.get("body").cloned();

    let tx = pending.lock().await.remove(&req_seq);
    if let Some(tx) = tx {
        let res = if success {
            Ok(body)
        } else {
            Err(DapClientError::DapError {
                command,
                message: if message.is_empty() {
                    "adapter reported failure".into()
                } else {
                    message
                },
            })
        };
        let _ = tx.send(res);
    }
}

async fn handle_event(msg: Value, state: &Arc<Mutex<EventState>>, notify: &Arc<Notify>) {
    let Some(event) = msg.get("event").and_then(Value::as_str) else {
        return;
    };
    let body = msg.get("body").cloned();
    let mut guard = state.lock().await;
    match event {
        "initialized" => guard.initialized = true,
        "stopped" => {
            if let Some(body) = body {
                guard.stopped = Some(parse_stopped(&body));
            }
        }
        "continued" => {
            guard.stopped = None;
        }
        "exited" | "terminated" => {
            guard.terminated = true;
            guard.stopped = None;
        }
        "output" => {
            // Log adapter output at debug level; tools can fetch via a
            // future helper if needed.
            if let Some(body) = body {
                let category = body
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("console");
                let output = body.get("output").and_then(Value::as_str).unwrap_or("");
                tracing::debug!(%category, output = %output.trim_end(), "dap output");
            }
        }
        _ => {}
    }
    drop(guard);
    notify.notify_waiters();
}

fn parse_stopped(body: &Value) -> StoppedInfo {
    StoppedInfo {
        thread_id: body.get("threadId").and_then(Value::as_i64),
        reason: body
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        description: body
            .get("description")
            .and_then(Value::as_str)
            .map(String::from),
        text: body.get("text").and_then(Value::as_str).map(String::from),
        all_threads_stopped: body
            .get("allThreadsStopped")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        hit_breakpoint_ids: body
            .get("hitBreakpointIds")
            .and_then(Value::as_array)
            .map(|ids| ids.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default(),
    }
}

fn spawn_stderr_drain(stderr: tokio::process::ChildStderr) {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 4096];
        let mut reader = stderr;
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&buf[..n]);
                    for line in text.lines() {
                        if !line.is_empty() {
                            tracing::debug!(line = %line, "dap stderr");
                        }
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stopped_event_body() {
        let body = json!({
            "threadId": 7,
            "reason": "breakpoint",
            "description": "Paused on breakpoint",
            "allThreadsStopped": true,
            "hitBreakpointIds": [1, 2],
        });
        let info = parse_stopped(&body);
        assert_eq!(info.thread_id, Some(7));
        assert_eq!(info.reason, "breakpoint");
        assert!(info.all_threads_stopped);
        assert_eq!(info.hit_breakpoint_ids, vec![1, 2]);
    }

    #[test]
    fn to_frame_extracts_source_path() {
        let raw = json!({
            "id": 42,
            "name": "main",
            "line": 10,
            "column": 4,
            "source": {"path": "/p/src/main.rs"},
        });
        let frame = to_frame(&raw);
        assert_eq!(frame.id, 42);
        assert_eq!(frame.name, "main");
        assert_eq!(frame.source_path.as_deref(), Some("/p/src/main.rs"));
    }
}
