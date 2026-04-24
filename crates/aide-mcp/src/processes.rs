//! Read-only `process_list` implementation (v0.20.1).
//!
//! Thin wrapper over the `sysinfo` crate that enumerates processes
//! owned by the current user, with optional case-insensitive name
//! filtering and a result cap. This is **purely observational** —
//! no signal, no kill. Signalling arbitrary PIDs is explicitly off
//! the roadmap (see STATUS.md v0.20.1 note); the `job_*` family
//! handles the "abort a thing I launched" case without the risk of
//! hitting the user's editor / terminal / browser.
//!
//! The primary use case is "which PID is the running aide-mcp?" and
//! similar diagnostic questions — answering them today requires a
//! shell-out to `ps`, which defeats the dogfood preference.
//!
//! Scoped to same-user processes: the kernel already enforces this
//! for anything that would read `/proc/<pid>/*` on Linux or call
//! `proc_pidinfo` on macOS, so our filter is also a display nicety
//! — it removes noise from system daemons rather than enforcing a
//! security boundary.

use serde::Serialize;
use sysinfo::{ProcessStatus, ProcessesToUpdate, System};

/// One process entry as returned by [`list`]. Fields are cheap to
/// produce; expensive ones (env, open FDs, threads) are deliberately
/// omitted — agents that need those reach for `Bash(ps)` or an MCP
/// extension we haven't written.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    pub cmd: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// UNIX epoch seconds the process started.
    pub started_at_unix: u64,
    /// Resident memory in bytes.
    pub memory_bytes: u64,
    pub cpu_percent: f32,
    /// `running` / `sleeping` / `stopped` / `zombie` / …
    pub status: String,
}

/// Snapshot current-user processes matching `name_filter` (case-
/// insensitive substring on the process name; `None` returns everything
/// for the current user). Sorted by PID ascending for deterministic
/// output. Capped at `limit` entries.
pub fn list(name_filter: Option<&str>, limit: usize) -> Vec<ProcessInfo> {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);

    let current_uid = sysinfo::get_current_pid()
        .ok()
        .and_then(|pid| system.process(pid))
        .and_then(|p| p.user_id().cloned());

    let needle = name_filter.map(str::to_ascii_lowercase);

    let mut out: Vec<ProcessInfo> = system
        .processes()
        .values()
        .filter(|p| {
            // Current-user scope gate. Skip the filter when we
            // couldn't resolve our own UID — still safer than
            // returning a blank list and leaving the agent guessing.
            if let Some(uid) = &current_uid {
                if p.user_id() != Some(uid) {
                    return false;
                }
            }
            if let Some(needle) = &needle {
                let name = p.name().to_string_lossy();
                if !name.to_ascii_lowercase().contains(needle) {
                    return false;
                }
            }
            true
        })
        .map(to_info)
        .collect();

    out.sort_by_key(|p| p.pid);
    out.truncate(limit);
    out
}

fn to_info(p: &sysinfo::Process) -> ProcessInfo {
    ProcessInfo {
        pid: p.pid().as_u32(),
        name: p.name().to_string_lossy().into_owned(),
        exe: p.exe().map(|p| p.display().to_string()),
        cmd: p
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
        cwd: p.cwd().map(|p| p.display().to_string()),
        started_at_unix: p.start_time(),
        memory_bytes: p.memory(),
        cpu_percent: p.cpu_usage(),
        status: status_label(p.status()).to_string(),
    }
}

fn status_label(s: ProcessStatus) -> &'static str {
    match s {
        ProcessStatus::Run => "running",
        ProcessStatus::Sleep => "sleeping",
        ProcessStatus::Idle => "idle",
        ProcessStatus::Stop => "stopped",
        ProcessStatus::Zombie => "zombie",
        ProcessStatus::Tracing => "tracing",
        ProcessStatus::Dead => "dead",
        ProcessStatus::Wakekill => "wakekill",
        ProcessStatus::Waking => "waking",
        ProcessStatus::Parked => "parked",
        ProcessStatus::LockBlocked => "lock_blocked",
        ProcessStatus::UninterruptibleDiskSleep => "disk_sleep",
        ProcessStatus::Unknown(_) => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_includes_self() {
        // The test process itself must show up when we list without
        // a filter — smoke check that the sysinfo pipeline works at
        // all on the current platform (macOS / Linux).
        let all = list(None, 10_000);
        let my_pid = std::process::id();
        assert!(
            all.iter().any(|p| p.pid == my_pid),
            "listing should include our own pid {my_pid}"
        );
    }

    #[test]
    fn name_filter_is_case_insensitive() {
        // Filter for a substring of whatever our binary is called.
        // `cargo test` runs under a `<crate>-<hash>` name, so "test"
        // or a fragment of the crate name is a reliable needle.
        let my_pid = std::process::id();
        let all = list(None, 10_000);
        let me = all
            .iter()
            .find(|p| p.pid == my_pid)
            .expect("self present in full list");

        // Use the first 4 chars of our own name as the needle, in
        // upper-case — the filter should match regardless of case.
        let needle_lower: String = me.name.chars().take(4).collect();
        let needle: String = needle_lower.to_ascii_uppercase();
        let filtered = list(Some(&needle), 10_000);
        assert!(
            filtered.iter().any(|p| p.pid == my_pid),
            "case-insensitive filter {needle:?} should match own name {:?}",
            me.name
        );
    }

    #[test]
    fn limit_caps_results() {
        let all = list(None, 10_000);
        if all.len() < 2 {
            // Hermetic CI with only 1 visible process — skip.
            return;
        }
        let capped = list(None, 1);
        assert_eq!(capped.len(), 1);
    }

    #[test]
    fn sorted_by_pid_ascending() {
        let all = list(None, 10_000);
        let mut last: u32 = 0;
        for p in &all {
            assert!(p.pid >= last, "not ascending at pid {}", p.pid);
            last = p.pid;
        }
    }
}
