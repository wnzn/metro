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

pub mod client;
pub mod config;
pub mod confine;
pub mod exec;
pub mod mcp;
pub mod runner;
pub mod spaces;
pub mod tools;
pub mod util;

use std::sync::atomic::{AtomicBool, Ordering};

/// Set when the process receives SIGINT/SIGTERM. Long-running waits poll it.
pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}
