//! Pages: dashboards and small agentic apps people build inside Metro by
//! prompting, written in Page TSX.
//!
//! Page TSX looks like TSX but no page code ever runs: the server parses it
//! with a strict grammar ([`tsx`]), checks it against the block catalogue
//! and resolves room and agent names ([`compile`]), and stores the checked
//! JSON tree ([`spec`]) that the desktop app draws with Metro's own
//! components.

pub mod access;
pub mod actions;
pub(crate) mod agent_tools;
pub mod api;
pub mod compile;
pub mod data;
pub mod generate;
pub mod spec;
pub mod store;
pub mod tsx;

pub use api::router;
pub use store::{hooks, init};

#[cfg(test)]
mod tests;

/// How often expired drafts and pages past their restore window are purged.
pub const CLEANUP_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Purge expired drafts and pages deleted more than 30 days ago, now and
/// then every [`CLEANUP_EVERY`], independent of anyone listing pages.
/// (Direct access already refuses expired drafts and pages past the
/// window; this only frees the space.) Call once, after the state is built.
pub fn spawn_cleanup(state: crate::AppState) {
    tokio::spawn(async move {
        loop {
            let result = state
                .db
                .call(|db| store::purge_expired(db, crate::db::now_ms()))
                .await;
            if let Err(e) = result {
                tracing::error!(error = %format!("{e:#}"), "purging expired pages and drafts");
            }
            tokio::time::sleep(CLEANUP_EVERY).await;
        }
    });
}
