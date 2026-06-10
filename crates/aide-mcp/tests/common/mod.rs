//! Shared harness for the e2e test crates: spawns the real `mcp-aide`
//! binary and speaks newline-delimited JSON-RPC over stdio.
#![allow(
    dead_code,
    reason = "shared test harness — not every test crate uses every helper"
)]

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

pub const BIN: &str = env!("CARGO_BIN_EXE_mcp-aide");
pub const RECV_TIMEOUT: Duration = Duration::from_secs(30);

pub struct StdioServer {
    pub child: Child,
    stdin: ChildStdin,
    pub stdout: Lines<BufReader<ChildStdout>>,
}

impl StdioServer {
    pub fn spawn(aide_home: &Path, cwd: &Path) -> Self {
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

    pub async fn send_raw(&mut self, line: &str) {
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write to server stdin");
        self.stdin.flush().await.expect("flush server stdin");
    }

    pub async fn send(&mut self, msg: &Value) {
        self.send_raw(&msg.to_string()).await;
    }

    /// Read messages until one carries the given request id —
    /// notifications and unrelated traffic are skipped.
    pub async fn recv_id(&mut self, id: u64) -> Value {
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
    pub async fn handshake(&mut self) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "e2e-test", "version": "0"}
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

    pub async fn tools_list(&mut self, id: u64) -> Vec<Value> {
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

    pub async fn tools_call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
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
pub fn tool_json(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result has no text content: {resp}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("tool result text is not JSON ({e}): {text}"))
}

pub fn git(cwd: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed in {}", cwd.display());
}

pub fn git_commit_all(cwd: &Path, message: &str) {
    git(cwd, &["add", "-A"]);
    git(
        cwd,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            message,
        ],
    );
}

pub fn git_head_sha(cwd: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .expect("git rev-parse HEAD");
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A minimal committed Rust crate for deterministic tool output.
pub fn init_repo(dir: &Path) {
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
    git(dir, &["init", "-q"]);
    git_commit_all(dir, "init");
}
