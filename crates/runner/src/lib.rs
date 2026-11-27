//! metro-runner: claims tool jobs from a Metro server and executes them
//! against a confined workspace directory (one per Space for reusable
//! credentials).
//!
//! Module overview:
//! - [`config`]: environment parsing and validation (fail closed).
//! - [`client`]: outbound JSON HTTP to the server (bounded, no redirects).
//! - [`confine`]: workspace-confined file access via `openat2(RESOLVE_BENEATH)`.
//! - [`exec`]: subprocess execution on the host or inside an opt-in docker sandbox.
//! - [`mcp`]: minimal stdio MCP client (initialize, tools/list, tools/call).
//! - [`tools`]: tool trait, registry and the built-in adapters.
//! - [`spaces`]: per-Space roots under the base for reusable credentials.
//! - [`runner`]: the claim/execute/report loop with backoff.
//! - [`update`]: version reporting and the opt-in self-update.

pub mod client;
pub mod config;
pub mod confine;
pub mod exec;
pub mod mcp;
pub mod runner;
pub mod spaces;
pub mod tools;
pub mod update;
pub mod util;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Set when the process receives SIGINT/SIGTERM. Long-running waits poll it.
pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

/// Which job runs now and whether the server still authorizes it. Each job
/// gets a new serial, so a late heartbeat answer for an earlier job never
/// cancels a later one. The runner runs one job at a time.
pub struct JobCancel {
    /// Serial of the last job started; serials only grow.
    last: AtomicU64,
    /// Serial of the job running now, 0 between jobs.
    current: AtomicU64,
    /// Highest serial the server no longer authorizes.
    cancelled: AtomicU64,
}

impl JobCancel {
    pub const fn new() -> Self {
        JobCancel {
            last: AtomicU64::new(0),
            current: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
        }
    }

    /// Marks the start of one job and returns its serial.
    pub fn begin(&self) -> u64 {
        let serial = self.last.fetch_add(1, Ordering::SeqCst) + 1;
        self.current.store(serial, Ordering::SeqCst);
        serial
    }

    /// Marks the end of the job `serial`.
    pub fn end(&self, serial: u64) {
        let _ = self
            .current
            .compare_exchange(serial, 0, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// The server no longer authorizes the job `serial` (heartbeat
    /// 401/404/409/410).
    pub fn cancel(&self, serial: u64) {
        self.cancelled.fetch_max(serial, Ordering::SeqCst);
    }

    /// True while the running job is cancelled.
    pub fn is_cancelled(&self) -> bool {
        let current = self.current.load(Ordering::SeqCst);
        current != 0 && self.cancelled.load(Ordering::SeqCst) == current
    }
}

impl Default for JobCancel {
    fn default() -> Self {
        Self::new()
    }
}

/// The runner's job state; shell waits poll [`job_cancelled`].
pub static JOBS: JobCancel = JobCancel::new();

pub fn job_cancelled() -> bool {
    JOBS.is_cancelled()
}

#[cfg(test)]
mod job_cancel_tests {
    use super::JobCancel;

    #[test]
    fn a_late_cancel_for_an_earlier_job_does_not_stop_the_next() {
        let jobs = JobCancel::new();
        assert!(!jobs.is_cancelled());
        let first = jobs.begin();
        jobs.cancel(first);
        assert!(jobs.is_cancelled());
        jobs.end(first);
        assert!(!jobs.is_cancelled());
        let second = jobs.begin();
        assert!(!jobs.is_cancelled());
        // The first job's heartbeat answers late.
        jobs.cancel(first);
        assert!(!jobs.is_cancelled());
        jobs.cancel(second);
        assert!(jobs.is_cancelled());
        // An even later answer for the first job can't undo it.
        jobs.cancel(first);
        assert!(jobs.is_cancelled());
        jobs.end(second);
        assert!(!jobs.is_cancelled());
        // Ending an older serial leaves the running job alone.
        let third = jobs.begin();
        jobs.end(second);
        jobs.cancel(third);
        assert!(jobs.is_cancelled());
    }
}

/// Unit tests that start processes run apart from tests that check the
/// install lock. That lock is a `flock` on the install directory, which
/// belongs to its open file description: a child forked by another test
/// thread holds a copy of every descriptor until its `exec`, so dropping the
/// lock does not release it in that window and the next take sees
/// `installer_busy`. Spawning tests hold [`test_gate::spawning`]; lock tests
/// hold [`test_gate::exclusive`].
#[cfg(test)]
pub(crate) mod test_gate {
    use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

    static GATE: RwLock<()> = RwLock::new(());

    pub fn spawning() -> RwLockReadGuard<'static, ()> {
        GATE.read().unwrap_or_else(|p| p.into_inner())
    }

    pub fn exclusive() -> RwLockWriteGuard<'static, ()> {
        GATE.write().unwrap_or_else(|p| p.into_inner())
    }
}
