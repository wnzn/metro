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

use std::sync::atomic::{AtomicBool, Ordering};

/// Set when the process receives SIGINT/SIGTERM. Long-running waits poll it.
pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

/// Set while the current job is no longer authorized by the server (its
/// heartbeat answered 401/404/409/410). The runner runs one job at a time and
/// clears it before each job; shell waits poll it.
pub static JOB_CANCELLED: AtomicBool = AtomicBool::new(false);

pub fn job_cancelled() -> bool {
    JOB_CANCELLED.load(Ordering::SeqCst)
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
