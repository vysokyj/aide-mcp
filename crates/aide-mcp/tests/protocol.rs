//! End-to-end MCP protocol tests.
//!
//! Each test spawns the real `mcp-aide` binary (built by cargo for this
//! package) and speaks the actual wire protocol — newline-delimited
//! JSON-RPC over stdio, or Streamable HTTP via `--http`. No language
//! servers are involved: `project_detect`, `git_status`, and the error
//! paths are enough to exercise the protocol layer end to end.

mod common;

use std::path::Path;
use std::process::Stdio;

use common::{init_repo, tool_json, StdioServer, BIN, RECV_TIMEOUT};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::time::timeout;

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
