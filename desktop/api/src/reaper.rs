//! Orphaned-subprocess reaping across restarts (PRD FR1, P4).
//!
//! A live session's `codex` child is normally reaped by RAII: the driver sets
//! `kill_on_drop`, so dropping the [`SessionHandle`](core_provider::SessionHandle)
//! — or the whole process exiting cleanly — kills it. The one case that escapes
//! that is a *hard* parent crash (SIGKILL, power loss): the child is reparented
//! to init and keeps running. [`SessionRegistry`] closes that gap by persisting
//! every spawned child's pid to a small file and sweeping survivors on the next
//! startup.
//!
//! Reaping is deliberately conservative. PIDs get reused by the OS, so before
//! killing anything the sweep verifies (on Unix, via `ps`) that the pid still
//! belongs to a `codex` process. A pid that has been reused by an unrelated
//! program is left alone.

use std::path::PathBuf;
use std::sync::Mutex;

/// Persists the pids of live provider subprocesses so orphans can be reaped on
/// the next startup. A registry with no path is a no-op (used in tests).
pub struct SessionRegistry {
    /// Where the pid list is stored. `None` disables persistence and sweeping.
    path: Option<PathBuf>,
    /// Serializes the read-modify-write cycle on the pid file.
    lock: Mutex<()>,
}

impl SessionRegistry {
    /// A registry backed by `path` (created lazily on first write).
    pub fn new(path: PathBuf) -> Self {
        SessionRegistry {
            path: Some(path),
            lock: Mutex::new(()),
        }
    }

    /// A no-op registry: never records, never sweeps. For tests and headless use.
    pub fn disabled() -> Self {
        SessionRegistry {
            path: None,
            lock: Mutex::new(()),
        }
    }

    /// Record a newly spawned subprocess so it can be reaped if we crash.
    pub fn record(&self, pid: u32) {
        self.mutate(|pids| {
            if !pids.contains(&pid) {
                pids.push(pid);
            }
        });
    }

    /// Drop a subprocess we have already stopped/reaped ourselves.
    pub fn forget(&self, pid: u32) {
        self.mutate(|pids| pids.retain(|&p| p != pid));
    }

    /// Kill any recorded process that is still alive *and* still a `codex`
    /// process (orphans from a prior crash), then clear the file. Returns the
    /// pids actually reaped. A no-op for a disabled registry.
    pub fn sweep(&self) -> Vec<u32> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let _guard = self.lock.lock().unwrap();
        let recorded = read_pids(path);
        let mut reaped = Vec::new();
        for pid in recorded {
            if is_codex_process(pid) {
                if kill_pid(pid) {
                    reaped.push(pid);
                }
            }
        }
        // Whether or not each kill succeeded, the slate is wiped: stale entries
        // (already-dead, or pid-reused) must not accumulate across restarts.
        let _ = std::fs::write(path, "[]");
        reaped
    }

    /// Run `f` over the current pid list under the file lock and persist it.
    fn mutate(&self, f: impl FnOnce(&mut Vec<u32>)) {
        let Some(path) = &self.path else {
            return;
        };
        let _guard = self.lock.lock().unwrap();
        let mut pids = read_pids(path);
        f(&mut pids);
        if let Ok(raw) = serde_json::to_string(&pids) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, raw);
        }
    }
}

/// Read the persisted pid list, treating any read/parse failure as empty.
fn read_pids(path: &PathBuf) -> Vec<u32> {
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// True if `pid` names a live process whose command looks like `codex`.
///
/// On Unix this shells out to `ps -p <pid> -o comm=` (portable across macOS and
/// Linux). On other platforms we cannot cheaply verify identity, so we report
/// `false` and decline to reap — never risk killing the wrong process.
#[cfg(unix)]
fn is_codex_process(pid: u32) -> bool {
    use std::process::Command;
    let Ok(output) = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false; // no such process
    }
    let comm = String::from_utf8_lossy(&output.stdout);
    // `comm` is the executable path/name; match the basename loosely.
    comm.rsplit('/')
        .next()
        .unwrap_or("")
        .trim()
        .contains("codex")
}

#[cfg(not(unix))]
fn is_codex_process(_pid: u32) -> bool {
    false
}

/// Send a hard kill to `pid`. Returns whether the kill command reported success.
#[cfg(unix)]
fn kill_pid(pid: u32) -> bool {
    use std::process::Command;
    Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn kill_pid(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_forget_roundtrip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.pids");
        let reg = SessionRegistry::new(path.clone());

        reg.record(101);
        reg.record(202);
        reg.record(101); // duplicate ignored
        assert_eq!(read_pids(&path), vec![101, 202]);

        reg.forget(101);
        assert_eq!(read_pids(&path), vec![202]);
    }

    #[test]
    fn sweep_never_kills_a_non_codex_process_and_clears_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.pids");
        let reg = SessionRegistry::new(path.clone());

        // Our own test process is alive but is not `codex`, so the identity
        // guard must spare it — proving the pid-reuse safeguard works.
        let me = std::process::id();
        reg.record(me);

        let reaped = reg.sweep();
        assert!(!reaped.contains(&me), "must not reap a non-codex pid");
        assert!(std::process::id() == me, "we are obviously still alive");
        // The file is wiped regardless so stale entries don't accumulate.
        assert_eq!(read_pids(&path), Vec::<u32>::new());
    }

    #[test]
    fn disabled_registry_is_inert() {
        let reg = SessionRegistry::disabled();
        reg.record(1);
        reg.forget(1);
        assert!(reg.sweep().is_empty());
    }
}
