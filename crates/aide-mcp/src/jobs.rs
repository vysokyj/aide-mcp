//! Registry of aide-spawned child processes.
//!
//! Every `run_project` / `run_tests` / `install_package` invocation
//! registers its child here between spawn and exit. This unlocks three
//! observability + control MCP tools (`job_list`, `job_info`,
//! `job_kill`) without opening a generic process-control surface:
//! callers refer to jobs by an aide-assigned `job_id`, never by raw
//! PID, so there is no API path through which an agent can signal a
//! process aide did not launch.
//!
//! The scope gate is the point. Generic "kill by PID" is deliberately
//! *not* exposed — see STATUS.md's v0.20.1 row for the won't-do
//! reasoning (PID reuse, agent hallucination, scope creep).
//!
//! ## Concurrency model
//!
//! One [`Registry`] hangs off `AideServer` inside an `Arc`. `exec::run`
//! borrows it through an `Option<&Registry>` argument; every tool that
//! could signal a job (`job_kill`) locks the underlying `AsyncMutex`
//! only long enough to read the stored PID before calling `kill(2)`
//! outside the lock. Register and deregister are point operations so
//! blocking is negligible in practice.
//!
//! ## PID-reuse race
//!
//! There is a small window between `exec::run` returning from `wait()`
//! and the deregister point where the kernel has already freed the
//! PID. A concurrent `job_kill` in that window signals whatever new
//! process now holds that PID — classic PID-reuse. We accept this: on
//! Linux / macOS the window is sub-millisecond, the same-user
//! permission check in `kill(2)` still limits blast radius, and the
//! alternative (per-job synchronisation around reap-then-deregister)
//! would serialise every exec call for vanishing risk.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use thiserror::Error;
use tokio::sync::Mutex as AsyncMutex;

/// Metadata for a single aide-spawned job while (and briefly after) it
/// runs. Every field is cheap to clone; `list` hands out full `Vec<Job>`
/// copies rather than sharing locks with callers.
#[derive(Debug, Clone, Serialize)]
pub struct Job {
    /// Monotonic id assigned by [`Registry::next_id`] — `"job-1"`,
    /// `"job-2"`, …
    pub id: String,
    /// OS process id at spawn time. Populated from
    /// [`tokio::process::Child::id`] immediately after `cmd.spawn()`.
    pub pid: u32,
    /// Which MCP tool kicked off this job — `run_project`,
    /// `run_tests`, or `install_package`. Lets agents filter before
    /// signalling, e.g. "only kill stuck tests, not my running server".
    pub kind: &'static str,
    /// Binary name as invoked (the first arg of `tokio::process::Command`).
    pub executable: String,
    /// Command-line args, each best-effort lossy UTF-8 from the
    /// caller's `OsString`.
    pub args: Vec<String>,
    /// UNIX epoch seconds the job was registered.
    pub started_at_unix: i64,
}

pub struct Registry {
    jobs: AsyncMutex<HashMap<String, Job>>,
    counter: AtomicU64,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            jobs: AsyncMutex::new(HashMap::new()),
            counter: AtomicU64::new(1),
        }
    }

    /// Allocate the next `job-N` id. Increments independently of
    /// registration success so ids stay monotonic even if the caller
    /// decides not to follow through.
    pub fn next_id(&self) -> String {
        format!("job-{}", self.counter.fetch_add(1, Ordering::Relaxed))
    }

    pub async fn register(&self, job: Job) {
        self.jobs.lock().await.insert(job.id.clone(), job);
    }

    pub async fn deregister(&self, id: &str) {
        self.jobs.lock().await.remove(id);
    }

    pub async fn list(&self) -> Vec<Job> {
        let mut out: Vec<Job> = self.jobs.lock().await.values().cloned().collect();
        // Stable, monotonic id order — easier to diff across calls and
        // scan visually in a tool response.
        out.sort_by(|a, b| a.started_at_unix.cmp(&b.started_at_unix));
        out
    }

    pub async fn get(&self, id: &str) -> Option<Job> {
        self.jobs.lock().await.get(id).cloned()
    }

    /// Send `signal` to the job's OS process. Returns a structured
    /// result so the agent gets confirmation of exactly what was sent
    /// to which PID. Does not touch the registry — the `exec::run`
    /// call that owns the child will see the signal propagate via
    /// `wait()` and handle deregistration on its own timeline.
    pub async fn signal(&self, id: &str, signal: Signal) -> Result<SignalResult, SignalError> {
        let job = self.get(id).await.ok_or(SignalError::NoSuchJob)?;
        let pid_i32 = i32::try_from(job.pid).map_err(|_| SignalError::BadPid(job.pid))?;
        // Explicit annotation: `nix::sys::signal::kill` is generic over
        // `Into<Option<Signal>>` so `.into()` alone is ambiguous.
        let nix_signal: nix::sys::signal::Signal = signal.into();
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid_i32), nix_signal)?;
        Ok(SignalResult {
            id: job.id,
            pid: job.pid,
            signal: signal.as_str(),
        })
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Agent-friendly signal names. We expose a narrow set because a
/// broader one (SIGSYS, SIGPIPE, …) adds blast radius for no
/// foreseeable use case; the five here cover the "stop this
/// gracefully", "really stop this now", and "reload" intents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
    Int,
    Hup,
    Quit,
}

impl Signal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Term => "term",
            Self::Kill => "kill",
            Self::Int => "int",
            Self::Hup => "hup",
            Self::Quit => "quit",
        }
    }

    /// Accept the canonical name, the `SIG`-prefixed alias, and the
    /// POSIX number. Case-insensitive on the string forms.
    pub fn parse(raw: &str) -> Option<Self> {
        let lower = raw.trim().to_ascii_lowercase();
        match lower.as_str() {
            "term" | "sigterm" | "15" => Some(Self::Term),
            "kill" | "sigkill" | "9" => Some(Self::Kill),
            "int" | "sigint" | "2" => Some(Self::Int),
            "hup" | "sighup" | "1" => Some(Self::Hup),
            "quit" | "sigquit" | "3" => Some(Self::Quit),
            _ => None,
        }
    }
}

impl From<Signal> for nix::sys::signal::Signal {
    fn from(s: Signal) -> Self {
        use nix::sys::signal::Signal as N;
        match s {
            Signal::Term => N::SIGTERM,
            Signal::Kill => N::SIGKILL,
            Signal::Int => N::SIGINT,
            Signal::Hup => N::SIGHUP,
            Signal::Quit => N::SIGQUIT,
        }
    }
}

/// Success payload for `job_kill`. Echoes back exactly what aide sent
/// where; on success the OS guarantees the signal was delivered (though
/// the target may handle or ignore it depending on its signal mask).
#[derive(Debug, Clone, Serialize)]
pub struct SignalResult {
    pub id: String,
    pub pid: u32,
    pub signal: &'static str,
}

#[derive(Debug, Error)]
pub enum SignalError {
    #[error("no such job in registry (did it already exit?)")]
    NoSuchJob,
    #[error("pid {0} is larger than i32::MAX; refusing to cast")]
    BadPid(u32),
    #[error("kill(2): {0}")]
    Nix(#[from] nix::errno::Errno),
}

fn current_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}

/// Helper for `exec::run` — build a [`Job`] at spawn time without
/// having to reconstruct the args/pid plumbing in two places.
pub fn build_job(
    registry: &Registry,
    pid: u32,
    kind: &'static str,
    executable: &str,
    args: &[std::ffi::OsString],
) -> Job {
    Job {
        id: registry.next_id(),
        pid,
        kind,
        executable: executable.to_string(),
        args: args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        started_at_unix: current_unix(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_list_deregister_round_trip() {
        let r = Registry::new();
        let id = r.next_id();
        r.register(Job {
            id: id.clone(),
            pid: 1234,
            kind: "run_tests",
            executable: "cargo".into(),
            args: vec!["test".into()],
            started_at_unix: 1_777_000_000,
        })
        .await;
        assert_eq!(r.list().await.len(), 1);
        assert_eq!(r.get(&id).await.unwrap().pid, 1234);
        r.deregister(&id).await;
        assert!(r.list().await.is_empty());
        assert!(r.get(&id).await.is_none());
    }

    #[tokio::test]
    async fn list_sorted_by_started_at() {
        let r = Registry::new();
        r.register(Job {
            id: "job-2".into(),
            pid: 1,
            kind: "run_tests",
            executable: "cargo".into(),
            args: vec![],
            started_at_unix: 200,
        })
        .await;
        r.register(Job {
            id: "job-1".into(),
            pid: 2,
            kind: "run_project",
            executable: "cargo".into(),
            args: vec![],
            started_at_unix: 100,
        })
        .await;
        let jobs = r.list().await;
        assert_eq!(jobs[0].id, "job-1");
        assert_eq!(jobs[1].id, "job-2");
    }

    #[tokio::test]
    async fn signal_nonexistent_job_errors() {
        let r = Registry::new();
        let err = r.signal("job-999", Signal::Term).await.unwrap_err();
        matches!(err, SignalError::NoSuchJob);
    }

    #[tokio::test]
    async fn signal_parses_canonical_and_sig_aliases_and_numbers() {
        for input in ["term", "TERM", "sigterm", "SIGTERM", "15"] {
            assert_eq!(Signal::parse(input).unwrap(), Signal::Term, "{input}");
        }
        for input in ["kill", "SIGKILL", "9"] {
            assert_eq!(Signal::parse(input).unwrap(), Signal::Kill);
        }
        for input in ["int", "hup", "quit"] {
            assert!(Signal::parse(input).is_some());
        }
        assert!(Signal::parse("nope").is_none());
        assert!(Signal::parse("42").is_none());
    }

    #[tokio::test]
    async fn signal_delivers_to_real_child() {
        // Spawn a sleeping child, register its PID, send SIGTERM, and
        // verify the child actually dies. This is the end-to-end
        // smoke test that the nix integration is wired correctly.
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep should be on PATH in CI/dev");
        let pid = child.id().expect("just-spawned child has a pid");

        let r = Registry::new();
        let id = r.next_id();
        r.register(Job {
            id: id.clone(),
            pid,
            kind: "run_tests",
            executable: "sleep".into(),
            args: vec!["30".into()],
            started_at_unix: current_unix(),
        })
        .await;

        r.signal(&id, Signal::Term).await.expect("kill succeeds");

        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .expect("child exited within timeout")
            .expect("wait returns");
        assert!(!status.success(), "SIGTERM-killed child is not success");

        // Registry is unchanged by signal — deregister is exec::run's
        // responsibility in production, so manual cleanup here.
        r.deregister(&id).await;
    }
}
