//! Shared process-runner for `run_project`, `run_tests`, and
//! `install_package`. Each tool builds a command line from its language
//! plugin and hands it off to [`run`], which:
//!
//! - spawns the child in `cwd` with a null stdin and piped stdout/stderr,
//! - captures both streams concurrently up to [`MAX_STREAM_BYTES`]
//!   (truncating the tail, never blocking the child),
//! - kills the child if it exceeds `timeout`,
//! - returns a structured [`ExecResult`] the agent can parse.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

/// Max bytes kept per stream. Anything beyond is dropped and the
/// `*_truncated` flag flips to true.
pub const MAX_STREAM_BYTES: usize = 1024 * 1024;

/// Default wall-clock budget for a tool invocation (5 minutes). Callers
/// can override via the tool's `timeout_secs` arg.
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

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
}

pub async fn run(
    bin: &str,
    args: &[OsString],
    cwd: &Path,
    duration: Duration,
) -> std::io::Result<ExecResult> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .expect("stdout was piped; .take() must return Some");
    let stderr = child
        .stderr
        .take()
        .expect("stderr was piped; .take() must return Some");

    let stdout_task = tokio::spawn(read_capped(stdout, MAX_STREAM_BYTES));
    let stderr_task = tokio::spawn(read_capped(stderr, MAX_STREAM_BYTES));

    let (timed_out, status) = match timeout(duration, child.wait()).await {
        Ok(Ok(status)) => (false, Some(status)),
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            (true, None)
        }
    };

    let (stdout, stdout_truncated) = stdout_task.await.unwrap_or_else(|_| (String::new(), false));
    let (stderr, stderr_truncated) = stderr_task.await.unwrap_or_else(|_| (String::new(), false));

    let command = format_command(bin, args);
    let exit_code = status.and_then(|s| s.code());

    Ok(ExecResult {
        command,
        exit_code,
        timed_out,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
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

async fn read_capped<R>(mut reader: R, cap: usize) -> (String, bool)
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
                    // pipe; we just throw the bytes away.
                    truncated = true;
                }
            }
        }
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
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);
        assert_eq!(result.stdout, "hello\nworld\n");
        assert!(result.stderr.is_empty());
        assert!(!result.stdout_truncated);
    }

    #[tokio::test]
    async fn captures_stderr_and_nonzero_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run(
            "sh",
            &os_args(&["-c", "echo oops >&2; exit 7"]),
            tmp.path(),
            Duration::from_secs(5),
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
        )
        .await
        .unwrap();
        assert!(result.timed_out);
        assert!(result.exit_code.is_none());
    }

    #[tokio::test]
    async fn stdout_is_truncated_past_cap() {
        let tmp = tempfile::tempdir().unwrap();
        // Emit ~2 MB of 'a' so we exceed the 1 MB cap.
        let result = run(
            "sh",
            &os_args(&["-c", "yes a | head -c 2097152; printf '\\nend\\n' >&2"]),
            tmp.path(),
            Duration::from_secs(10),
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
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
