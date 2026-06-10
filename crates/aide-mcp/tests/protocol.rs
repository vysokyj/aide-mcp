//! End-to-end MCP protocol tests.
//!
//! Each test spawns the real `mcp-aide` binary (built by cargo for this
//! package) and speaks the actual wire protocol — newline-delimited
//! JSON-RPC over stdio, or Streamable HTTP via `--http`. No language
//! servers are involved: `project_detect`, `git_status`, and the error
//! paths are enough to exercise the protocol layer end to end.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

const BIN: &str = env!("CARGO_BIN_EXE_mcp-aide");
const RECV_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------
// stdio harness
// ---------------------------------------------------------------------

struct StdioServer {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl StdioServer {
    fn spawn(aide_home: &Path, cwd: &Path) -> Self {
        let mut child = tokio::process::Command::new(BIN)
            .env("AIDE_HOME", aide_home)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn mcp-aide");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout")).lines();
        Self {
            child,
            stdin,
            stdout,
        }
    }

    async fn send_raw(&mut self, line: &str) {
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write to server stdin");
        self.stdin.flush().await.expect("flush server stdin");
    }

    async fn send(&mut self, msg: &Value) {
        self.send_raw(&msg.to_string()).await;
    }

    /// Read messages until one carries the given request id —
    /// notifications and unrelated traffic are skipped.
    async fn recv_id(&mut self, id: u64) -> Value {
        loop {
            let line = timeout(RECV_TIMEOUT, self.stdout.next_line())
                .await
                .expect("timed out waiting for server response")
                .expect("read from server stdout")
                .expect("server closed stdout");
            let msg: Value = serde_json::from_str(&line).expect("server emitted non-JSON line");
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                return msg;
            }
        }
    }

    /// Run the initialize → initialized handshake (request id 1).
    async fn handshake(&mut self) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "protocol-test", "version": "0"}
            }
        }))
        .await;
        let resp = self.recv_id(1).await;
        assert!(
            resp.get("result").is_some(),
            "initialize must succeed, got: {resp}"
        );
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .await;
    }

    async fn tools_list(&mut self, id: u64) -> Vec<Value> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
            "params": {}
        }))
        .await;
        let resp = self.recv_id(id).await;
        resp["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools/list missing tools array: {resp}"))
            .clone()
    }

    async fn tools_call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .await;
        self.recv_id(id).await
    }
}

/// Extract the first text content block of a tools/call result and
/// parse it as JSON — every aide tool returns a JSON string.
fn tool_json(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result has no text content: {resp}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("tool result text is not JSON ({e}): {text}"))
}

fn git(cwd: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed in {}", cwd.display());
}

/// A minimal committed Rust-looking repo for deterministic tool output.
fn init_repo(dir: &Path) {
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "init",
        ],
    );
}

// ---------------------------------------------------------------------
// stdio tests
// ---------------------------------------------------------------------

#[tokio::test]
async fn stdio_tools_list_advertises_complete_schemas() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let mut server = StdioServer::spawn(home.path(), repo.path());
    server.handshake().await;

    let tools = server.tools_list(2).await;
    assert!(
        tools.len() >= 70,
        "expected the full tool surface, got {} tools",
        tools.len()
    );
    for tool in &tools {
        let name = tool["name"].as_str().expect("tool has a name");
        assert!(
            !tool["description"].as_str().unwrap_or("").is_empty(),
            "tool `{name}` has an empty description"
        );
        assert!(
            tool["inputSchema"].is_object(),
            "tool `{name}` has no input schema"
        );
    }
}

#[tokio::test]
async fn stdio_project_detect_and_git_status_roundtrip() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let mut server = StdioServer::spawn(home.path(), repo.path());
    server.handshake().await;

    let resp = server
        .tools_call(
            2,
            "project_detect",
            json!({"path": repo.path().to_str().unwrap()}),
        )
        .await;
    let detect = tool_json(&resp);
    let langs: Vec<&str> = detect["languages"]
        .as_array()
        .expect("languages array")
        .iter()
        .filter_map(|l| l["id"].as_str())
        .collect();
    assert!(langs.contains(&"rust"), "expected rust, got {langs:?}");

    let resp = server
        .tools_call(
            3,
            "git_status",
            json!({"path": repo.path().to_str().unwrap()}),
        )
        .await;
    let status = tool_json(&resp);
    assert!(
        status.get("branch").is_some(),
        "git_status response missing branch: {status}"
    );
}

#[tokio::test]
async fn stdio_malformed_line_terminates_the_session_cleanly() {
    // Documented rmcp behavior: a line that fails JSON parsing tears
    // the stdio transport down rather than answering with a JSON-RPC
    // parse error. What we guarantee here is that the teardown is
    // clean — stdout reaches EOF and the process exits instead of
    // hanging or spinning.
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let mut server = StdioServer::spawn(home.path(), repo.path());
    server.handshake().await;

    server.send_raw("this is not json {{{").await;

    let eof = timeout(RECV_TIMEOUT, async {
        loop {
            match server.stdout.next_line().await {
                Ok(Some(_)) => {}  // drain any in-flight responses
                Ok(None) => break, // clean EOF
                Err(e) => panic!("stdout read error instead of clean EOF: {e}"),
            }
        }
    })
    .await;
    assert!(
        eof.is_ok(),
        "server kept the session open after malformed input"
    );

    let status = timeout(RECV_TIMEOUT, server.child.wait())
        .await
        .expect("server did not exit after transport teardown")
        .expect("wait on child");
    assert!(status.success(), "server exited non-zero: {status}");
}

#[tokio::test]
async fn stdio_unknown_tool_is_an_error_not_a_crash() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let mut server = StdioServer::spawn(home.path(), repo.path());
    server.handshake().await;

    let resp = server.tools_call(2, "no_such_tool", json!({})).await;
    let is_error =
        resp.get("error").is_some() || resp["result"]["isError"].as_bool().unwrap_or(false);
    assert!(is_error, "expected an error response, got: {resp}");

    // Server still alive.
    let tools = server.tools_list(3).await;
    assert!(!tools.is_empty());
}

#[tokio::test]
async fn stdio_missing_required_param_is_an_error_not_a_crash() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let mut server = StdioServer::spawn(home.path(), repo.path());
    server.handshake().await;

    // project_grep requires `pattern`.
    let resp = server.tools_call(2, "project_grep", json!({})).await;
    let is_error =
        resp.get("error").is_some() || resp["result"]["isError"].as_bool().unwrap_or(false);
    assert!(is_error, "expected an error response, got: {resp}");

    let tools = server.tools_list(3).await;
    assert!(!tools.is_empty());
}

#[tokio::test]
async fn stdio_memory_write_read_roundtrip() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let mut server = StdioServer::spawn(home.path(), repo.path());
    server.handshake().await;

    let path = repo.path().to_str().unwrap();
    let resp = server
        .tools_call(
            2,
            "memory_write",
            json!({
                "path": path,
                "name": "protocol-test",
                "description": "e2e fixture",
                "content": "remember this"
            }),
        )
        .await;
    assert!(
        resp["result"]["isError"].as_bool() != Some(true) && resp.get("error").is_none(),
        "memory_write failed: {resp}"
    );

    let resp = server
        .tools_call(
            3,
            "memory_read",
            json!({"path": path, "name": "protocol-test"}),
        )
        .await;
    let read = tool_json(&resp);
    assert!(
        read.to_string().contains("remember this"),
        "memory_read did not return the stored content: {read}"
    );
}

// ---------------------------------------------------------------------
// HTTP transport
// ---------------------------------------------------------------------

struct HttpServer {
    _child: Child,
    url: String,
}

impl HttpServer {
    /// Spawn `mcp-aide --http :0` and scrape the bound address from the
    /// startup log line on stderr.
    async fn spawn(aide_home: &Path, cwd: &Path) -> Self {
        let mut child = tokio::process::Command::new(BIN)
            .args(["--http", ":0"])
            .env("AIDE_HOME", aide_home)
            .env("RUST_LOG", "mcp_aide=info")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn mcp-aide --http");
        let mut stderr = BufReader::new(child.stderr.take().expect("child stderr")).lines();
        let url = timeout(RECV_TIMEOUT, async {
            while let Ok(Some(line)) = stderr.next_line().await {
                if let Some(idx) = line.find("listening on http://") {
                    let tail = &line[idx + "listening on ".len()..];
                    // The log line continues with structured fields
                    // after the URL — keep the first token only.
                    return tail.split_whitespace().next().unwrap().to_string();
                }
            }
            panic!("server exited before logging its listen address");
        })
        .await
        .expect("timed out waiting for the listen address");
        // Keep draining stderr so the child never blocks on a full pipe.
        tokio::spawn(async move { while stderr.next_line().await.ok().flatten().is_some() {} });
        Self { _child: child, url }
    }
}

/// Parse a Streamable HTTP response body — plain JSON or an SSE stream
/// of `data:` lines — and return the JSON message carrying `id`.
fn http_message_with_id(body: &str, content_type: &str, id: u64) -> Value {
    let candidates: Vec<Value> = if content_type.starts_with("text/event-stream") {
        body.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str(d.trim()).ok())
            .collect()
    } else {
        serde_json::from_str::<Value>(body).into_iter().collect()
    };
    candidates
        .into_iter()
        .find(|m| m.get("id").and_then(Value::as_u64) == Some(id))
        .unwrap_or_else(|| panic!("no message with id {id} in body: {body}"))
}

#[tokio::test]
async fn http_initialize_session_and_call_tool() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let server = HttpServer::spawn(home.path(), repo.path()).await;
    let client = reqwest::Client::new();

    // initialize — must hand back a session id.
    let resp = client
        .post(&server.url)
        .header("accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "protocol-test-http", "version": "0"}
            }
        }))
        .send()
        .await
        .expect("initialize over http");
    assert!(
        resp.status().is_success(),
        "initialize returned {}",
        resp.status()
    );
    let session = resp
        .headers()
        .get("mcp-session-id")
        .expect("initialize response carries mcp-session-id")
        .to_str()
        .unwrap()
        .to_string();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp.text().await.unwrap();
    let init = http_message_with_id(&body, &ct, 1);
    assert!(init.get("result").is_some(), "initialize failed: {init}");

    // initialized notification.
    let resp = client
        .post(&server.url)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "initialized notification returned {}",
        resp.status()
    );

    // tools/call project_detect — exercises a full tool roundtrip.
    let resp = client
        .post(&server.url)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "project_detect",
                "arguments": {"path": repo.path().to_str().unwrap()}
            }
        }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "tools/call returned {}",
        resp.status()
    );
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp.text().await.unwrap();
    let msg = http_message_with_id(&body, &ct, 2);
    let text = msg["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content: {msg}"));
    assert!(text.contains("rust"), "project_detect over http: {text}");
}

#[tokio::test]
async fn http_request_without_session_is_rejected() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let server = HttpServer::spawn(home.path(), repo.path()).await;
    let client = reqwest::Client::new();

    // tools/list without ever initializing a session.
    let resp = client
        .post(&server.url)
        .header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}))
        .send()
        .await
        .unwrap();
    assert!(
        !resp.status().is_success(),
        "expected a rejection without mcp-session-id, got {}",
        resp.status()
    );
}
