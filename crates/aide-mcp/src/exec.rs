//! Shared process-runner for `run_project`, `run_tests`, and
//! `install_package`. Each tool builds a command line from its language
//! plugin and hands it off to [`run`], which:
//!
//! - spawns the child in `cwd` with a null stdin and piped stdout/stderr,
//! - captures both streams concurrently up to [`MAX_STREAM_BYTES`]
//!   (truncating the tail — never blocking the child),
//! - tees the *full* streams to log files under `~/.aide/logs/` when
//!   the caller supplies a log dir (so oversized output survives the
//!   1 MB cap),
//! - kills the child if it exceeds `timeout`,
//! - returns a structured [`ExecResult`] the agent can parse.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aide_proto::Diagnostic;
use rmcp::model::{ProgressNotificationParam, ProgressToken};
use rmcp::{Peer, RoleServer};
use serde::Serialize;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// Max bytes kept in memory per stream. Anything beyond is dropped from
/// the response (but still written to the log file when one is in use).
pub const MAX_STREAM_BYTES: usize = 1024 * 1024;

/// Per-second heartbeat for MCP `notifications/progress` so the client
/// knows a long-running `cargo test` / `mvn test` is still alive.
const PROGRESS_TICK: Duration = Duration::from_secs(1);

/// Optional side channel for emitting MCP progress notifications while
/// a command runs. Callers build this from the `progressToken` on the
/// tool-call meta and the server-side [`Peer`]; no token = `None` =
/// no heartbeat.
#[derive(Clone)]
pub struct Progress {
    pub token: ProgressToken,
    pub peer: Peer<RoleServer>,
    /// Human-readable bin name surfaced in each notification's message.
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecResult {
    /// The fully-rendered command line (best-effort lossy UTF-8).
    pub command: String,
    /// Exit code. `None` when the process was killed by a signal or by
    /// our timeout.
    pub exit_code: Option<i32>,
    /// True when we killed the child because it exceeded `timeout`.
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// Full-output stdout log file (populated when `log_dir` was given).
    /// Useful when `stdout_truncated` is true — the log has everything.
    pub stdout_log: Option<String>,
    /// Full-output stderr log file (populated when `log_dir` was given).
    pub stderr_log: Option<String>,
    /// Parsed compiler / test diagnostics. Filled in by the caller
    /// (e.g. `run_project` / `run_tests` in the server) when the
    /// language plugin has a structured-output parser and the tool
    /// was invoked with structured-output args; empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

/// Opt-in binding that registers the spawned child in a
/// [`crate::jobs::Registry`] for the duration of the run, so
/// `job_list` / `job_info` / `job_kill` MCP tools can observe and
/// signal it. The `kind` string becomes the job's `kind` field —
/// one of `"run_project"`, `"run_tests"`, `"install_package"`.
pub struct JobBinding<'a> {
    pub registry: &'a crate::jobs::Registry,
    pub kind: &'static str,
}

pub async fn run(
    bin: &str,
    args: &[OsString],
    cwd: &Path,
    duration: Duration,
    log_dir: Option<&Path>,
    progress: Option<Progress>,
    job_binding: Option<JobBinding<'_>>,
) -> std::io::Result<ExecResult> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let (stdout_path, stderr_path) = resolve_log_paths(log_dir, bin)?;

    let mut child = cmd.spawn()?;

    // Register in the jobs registry the moment we have a PID. Kept
    // as `Option<(Registry, id)>` so every exit path can deregister
    // with one helper at the bottom of the function.
    let job_ticket: Option<(&crate::jobs::Registry, String)> = match (&job_binding, child.id()) {
        (Some(binding), Some(pid)) => {
            let job = crate::jobs::build_job(binding.registry, pid, binding.kind, bin, args);
            let id = job.id.clone();
            binding.registry.register(job).await;
            Some((binding.registry, id))
        }
        _ => None,
    };

    let stdout = child
        .stdout
        .take()
        .expect("stdout was piped; .take() must return Some");
    let stderr = child
        .stderr
        .take()
        .expect("stderr was piped; .take() must return Some");

    let stdout_file = open_log(stdout_path.as_deref()).await;
    let stderr_file = open_log(stderr_path.as_deref()).await;

    let stdout_task = tokio::spawn(read_capped_tee(stdout, MAX_STREAM_BYTES, stdout_file));
    let stderr_task = tokio::spawn(read_capped_tee(stderr, MAX_STREAM_BYTES, stderr_file));

    let heartbeat = progress.map(spawn_progress_heartbeat);

    let (timed_out, status) = match timeout(duration, child.wait()).await {
        Ok(Ok(status)) => (false, Some(status)),
        Ok(Err(e)) => {
            if let Some(h) = heartbeat {
                h.abort();
            }
            if let Some((registry, id)) = &job_ticket {
                registry.deregister(id).await;
            }
            return Err(e);
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (true, None)
        }
    };

    if let Some(h) = heartbeat {
        h.abort();
    }

    let (stdout, stdout_truncated) = stdout_task.await.unwrap_or_else(|_| (String::new(), false));
    let (stderr, stderr_truncated) = stderr_task.await.unwrap_or_else(|_| (String::new(), false));

    let command = format_command(bin, args);
    let exit_code = status.and_then(|s| s.code());

    if let Some((registry, id)) = job_ticket {
        registry.deregister(&id).await;
    }

    Ok(ExecResult {
        command,
        exit_code,
        timed_out,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        stdout_log: stdout_path.map(|p| p.display().to_string()),
        stderr_log: stderr_path.map(|p| p.display().to_string()),
        diagnostics: Vec::new(),
    })
}

fn spawn_progress_heartbeat(progress: Progress) -> JoinHandle<()> {
    tokio::spawn(async move {
        let start = Instant::now();
        let mut tick: f64 = 0.0;
        loop {
            tokio::time::sleep(PROGRESS_TICK).await;
            tick += 1.0;
            let elapsed = start.elapsed().as_secs();
            let message = format!("{} running: {elapsed}s", progress.label);
            let _ = progress
                .peer
                .notify_progress(ProgressNotificationParam {
                    progress_token: progress.token.clone(),
                    progress: tick,
                    total: None,
                    message: Some(message),
                })
                .await;
        }
    })
}

fn format_command(bin: &str, args: &[OsString]) -> String {
    let mut s = bin.to_string();
    for a in args {
        s.push(' ');
        s.push_str(&a.to_string_lossy());
    }
    s
}

fn resolve_log_paths(
    log_dir: Option<&Path>,
    bin: &str,
) -> std::io::Result<(Option<PathBuf>, Option<PathBuf>)> {
    let Some(dir) = log_dir else {
        return Ok((None, None));
    };
    std::fs::create_dir_all(dir)?;
    let prefix = log_prefix(bin);
    Ok((
        Some(dir.join(format!("{prefix}.stdout.log"))),
        Some(dir.join(format!("{prefix}.stderr.log"))),
    ))
}

fn log_prefix(bin: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let nanos = now.subsec_nanos();
    let safe_bin: String = Path::new(bin)
        .file_name()
        .map_or_else(|| bin.to_string(), |s| s.to_string_lossy().into_owned())
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{secs}_{nanos:09}-{safe_bin}")
}

async fn open_log(path: Option<&Path>) -> Option<File> {
    match path {
        None => None,
        Some(p) => match File::create(p).await {
            Ok(f) => Some(f),
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e, "could not open exec log");
                None
            }
        },
    }
}

async fn read_capped_tee<R>(mut reader: R, cap: usize, mut tee: Option<File>) -> (String, bool)
where
    R: AsyncReadExt + Unpin + Send + 'static,
{
    let mut buf = Vec::with_capacity(cap.min(4096));
    let mut chunk = [0u8; 4096];
    let mut truncated = false;
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if buf.len() < cap {
                    let space = cap - buf.len();
                    let take = n.min(space);
                    buf.extend_from_slice(&chunk[..take]);
                    if take < n {
                        truncated = true;
                    }
                } else {
                    // Keep draining so the child never blocks on a full
                    // pipe; we just throw the bytes away from memory —
                    // the tee keeps them on disk.
                    truncated = true;
                }
                if let Some(f) = tee.as_mut() {
                    if let Err(e) = f.write_all(&chunk[..n]).await {
                        tracing::warn!(error = %e, "exec log tee failed; closing log");
                        tee = None;
                    }
                }
            }
        }
    }
    if let Some(mut f) = tee {
        let _ = f.flush().await;
    }
    (String::from_utf8_lossy(&buf).to_string(), truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os_args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(|s| OsString::from(*s)).collect()
    }

    #[tokio::test]
    async fn captures_stdout_and_exit_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run(
            "sh",
            &os_args(&["-c", "echo hello && echo world"]),
            tmp.path(),
            Duration::from_secs(5),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);
        assert_eq!(result.stdout, "hello\nworld\n");
        assert!(result.stderr.is_empty());
        assert!(!result.stdout_truncated);
        assert!(result.stdout_log.is_none());
    }

    #[tokio::test]
    async fn captures_stderr_and_nonzero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run(
            "sh",
            &os_args(&["-c", "echo oops >&2; exit 7"]),
            tmp.path(),
            Duration::from_secs(5),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(7));
        assert_eq!(result.stderr, "oops\n");
        assert!(result.stdout.is_empty());
    }

    #[tokio::test]
    async fn timeout_kills_long_running_child() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run(
            "sh",
            &os_args(&["-c", "sleep 30"]),
            tmp.path(),
            Duration::from_millis(200),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert!(result.timed_out);
        assert!(result.exit_code.is_none());
    }

    #[tokio::test]
    async fn stdout_is_truncated_past_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run(
            "sh",
            &os_args(&["-c", "yes a | head -c 2097152; printf '\\nend\\n' >&2"]),
            tmp.path(),
            Duration::from_secs(10),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout_truncated);
        assert_eq!(result.stdout.len(), MAX_STREAM_BYTES);
    }

    #[tokio::test]
    async fn missing_binary_returns_io_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = run(
            "definitely-not-a-real-binary-aidemcp",
            &[],
            tmp.path(),
            Duration::from_secs(1),
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[tokio::test]
    async fn log_dir_captures_full_output_even_when_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        let logs = tmp.path().join("logs");
        let result = run(
            "sh",
            &os_args(&["-c", "yes x | head -c 2097152"]),
            tmp.path(),
            Duration::from_secs(10),
            Some(&logs),
            None,
            None,
        )
        .await
        .unwrap();
        assert!(result.stdout_truncated);
        let stdout_log = result.stdout_log.expect("stdout log path");
        let stderr_log = result.stderr_log.expect("stderr log path");
        let stdout_bytes = std::fs::metadata(&stdout_log).unwrap().len();
        // Log has the full 2 MB, not just the 1 MB we held in memory.
        assert_eq!(stdout_bytes, 2 * 1024 * 1024);
        // stderr log exists but empty.
        let stderr_bytes = std::fs::metadata(&stderr_log).unwrap().len();
        assert_eq!(stderr_bytes, 0);
    }
}
