//! End-to-end tests of the in-process SCIP indexer, driven through the
//! real MCP protocol: enqueue via `index_commit`, poll `index_status`
//! to `Ready`, and inspect the `.scip` files under `AIDE_HOME`.
//!
//! Requires a `rust-analyzer` binary (the Rust plugin's SCIP indexer).
//! When none is found on `$PATH` the tests log a skip and pass — CI
//! installs the rustup component explicitly so the pipeline always
//! runs there.

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{git_commit_all, git_head_sha, init_repo, tool_json, StdioServer};
use serde_json::{json, Value};

/// Find rust-analyzer on `$PATH`.
fn find_rust_analyzer() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("rust-analyzer"))
        .find(|candidate| candidate.is_file())
}

/// Prepare an `AIDE_HOME` whose `bin/` holds the SCIP indexer the
/// worker expects (`~/.aide/bin/rust-analyzer`).
fn seed_aide_home(home: &Path, rust_analyzer: &Path) {
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(rust_analyzer, bin.join("rust-analyzer")).unwrap();
}

/// Poll `index_status` for (path, sha) until Ready, panicking on
/// `Failed` or after `deadline`.
async fn wait_ready(server: &mut StdioServer, id_base: u64, path: &str, sha: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let mut id = id_base;
    loop {
        let resp = server
            .tools_call(id, "index_status", json!({"path": path, "sha": sha}))
            .await;
        id += 1;
        let info = tool_json(&resp);
        // IndexState serializes snake_case: "ready", {"failed": "..."}.
        match &info["state"] {
            Value::String(s) if s == "ready" => return,
            Value::Object(o) if o.contains_key("failed") => {
                panic!("indexing {sha} failed: {info}");
            }
            _ => {}
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {sha} to reach Ready; last status: {info}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Locate `<sha>.scip` anywhere under `AIDE_HOME/scip/`.
fn scip_file(home: &Path, sha: &str) -> Option<PathBuf> {
    let root = home.join("scip");
    let repos = std::fs::read_dir(&root).ok()?;
    for repo_dir in repos.flatten() {
        let candidate = repo_dir.path().join(format!("{sha}.scip"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[tokio::test]
async fn full_pipeline_with_retention_and_grace_period() {
    let Some(rust_analyzer) = find_rust_analyzer() else {
        eprintln!("SKIP: rust-analyzer not found on PATH");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo_path = std::fs::canonicalize(repo.path()).unwrap();
    seed_aide_home(home.path(), &rust_analyzer);
    init_repo(&repo_path);

    let mut server = StdioServer::spawn(home.path(), &repo_path);
    server.handshake().await;
    let path = repo_path.to_str().unwrap();

    // --- commit 1: full pipeline to Ready -----------------------------
    let sha1 = git_head_sha(&repo_path);
    let resp = server
        .tools_call(100, "index_commit", json!({"path": path, "sha": sha1}))
        .await;
    let info = tool_json(&resp);
    assert!(info.get("state").is_some(), "index_commit reply: {info}");
    wait_ready(&mut server, 200, path, &sha1).await;

    let file1 = scip_file(home.path(), &sha1).expect("commit 1 produced a .scip file");
    let index = aide_scip::load(&file1).expect("commit 1 index loads");
    let docs = aide_scip::documents(&index);
    assert!(
        docs.iter().any(|d| d.ends_with("src/lib.rs")),
        "index should cover src/lib.rs, got {docs:?}"
    );

    // --- commit 2: retention (default 1) evicts commit 1 from state,
    // but the grace period keeps its file on disk for one more round ---
    std::fs::write(
        repo_path.join("src/lib.rs"),
        "pub fn answer() -> u32 { 43 }\n",
    )
    .unwrap();
    git_commit_all(&repo_path, "second");
    let sha2 = git_head_sha(&repo_path);
    server
        .tools_call(300, "index_commit", json!({"path": path, "sha": sha2}))
        .await;
    wait_ready(&mut server, 400, path, &sha2).await;

    assert!(
        scip_file(home.path(), &sha2).is_some(),
        "commit 2 index file exists"
    );
    assert!(
        scip_file(home.path(), &sha1).is_some(),
        "commit 1 file must survive one round (eviction grace period)"
    );
    let resp = server
        .tools_call(500, "index_status", json!({"path": path, "sha": sha1}))
        .await;
    let info = tool_json(&resp);
    assert!(
        info.get("state").is_none() || info["state"].is_null(),
        "commit 1 must be evicted from state: {info}"
    );

    // --- commit 3: releases commit 1's file for deletion ---------------
    std::fs::write(
        repo_path.join("src/lib.rs"),
        "pub fn answer() -> u32 { 44 }\n",
    )
    .unwrap();
    git_commit_all(&repo_path, "third");
    let sha3 = git_head_sha(&repo_path);
    server
        .tools_call(600, "index_commit", json!({"path": path, "sha": sha3}))
        .await;
    wait_ready(&mut server, 700, path, &sha3).await;

    assert!(
        scip_file(home.path(), &sha3).is_some(),
        "commit 3 index file exists"
    );
    assert!(
        scip_file(home.path(), &sha1).is_none(),
        "commit 1 file must be deleted after the grace round"
    );
    assert!(
        scip_file(home.path(), &sha2).is_some(),
        "commit 2 file is stashed (grace), not yet deleted"
    );
}

#[tokio::test]
async fn crash_recovery_resumes_pending_commit() {
    let Some(rust_analyzer) = find_rust_analyzer() else {
        eprintln!("SKIP: rust-analyzer not found on PATH");
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo_path = std::fs::canonicalize(repo.path()).unwrap();
    seed_aide_home(home.path(), &rust_analyzer);
    init_repo(&repo_path);
    let sha = git_head_sha(&repo_path);

    // Pre-seed the state file as a crashed previous instance would have
    // left it: the commit enqueued but never processed.
    let queue = home.path().join("queue");
    std::fs::create_dir_all(&queue).unwrap();
    let state = json!({
        "repos": {
            format!("{}/", repo_path.display()): {
                "last_sha": sha,
                "commits": {
                    sha.clone(): {
                        "state": "pending",
                        "enqueued_at_unix": 1,
                        "indexed_at_unix": null,
                        "index_path": null
                    }
                }
            }
        }
    });
    std::fs::write(
        queue.join("indexer_state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();

    // A fresh server must pick the job up on startup without any
    // explicit index_commit call.
    let mut server = StdioServer::spawn(home.path(), &repo_path);
    server.handshake().await;
    let path = repo_path.to_str().unwrap();
    wait_ready(&mut server, 100, path, &sha).await;
    assert!(
        scip_file(home.path(), &sha).is_some(),
        "recovered commit produced a .scip file"
    );
}
