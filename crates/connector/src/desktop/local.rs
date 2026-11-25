//! The desktop-hosted solo workspace: a packaged `metro-server` the launcher
//! starts on loopback on demand and stops at exit.
//!
//! This file is the interface. The process manager implements
//! [`LocalWorkspaceHost`] in `crate::desktop_local` (a separate lane); the
//! launcher owns its lifetime: it builds one host, hands it to the bridge,
//! and calls [`LocalWorkspaceHost::shutdown`] when the app exits.

use std::future::Future;
use std::pin::Pin;

use serde::Serialize;

use crate::{SafeError, Secret};

/// A running local workspace: where it is and a bearer for it.
pub struct LocalWorkspace {
    /// `http://127.0.0.1:<port>`, no trailing slash; must pass
    /// `netpolicy::validate_base_url` unchanged.
    pub server_url: String,
    /// The UI's session token for that server. Kept in memory only.
    pub token: Secret,
}

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalState {
    Stopped,
    Starting,
    Running,
    Failed,
}

pub trait LocalWorkspaceHost: Send + Sync + 'static {
    /// Start the local server if it isn't running (or reuse it), wait until
    /// it is ready, and return its URL and a bearer. Safe to call repeatedly
    /// and concurrently. Error text is short and user-facing, never secret.
    fn open(&self) -> BoxFut<'_, Result<LocalWorkspace, SafeError>>;
    /// Current state, without starting anything.
    fn state(&self) -> LocalState;
    /// Stop the server and its children, wipe in-memory credentials.
    /// Idempotent.
    fn shutdown(&self) -> BoxFut<'_, ()>;
    /// Only true when no server can still be using the local database.
    fn shutdown_confirmed(&self) -> BoxFut<'_, bool> {
        Box::pin(async {
            self.shutdown().await;
            self.state() == LocalState::Stopped
        })
    }
    /// The running server's URL and bearer, without starting anything; used
    /// to tell whether a bound session is this app's own hosted workspace.
    /// In-process only.
    fn current(&self) -> Option<LocalWorkspace> {
        None
    }
}

/// The desktop tools lane's hook (`crate::desktop_tools`), adapted in
/// `desktop::tools_glue`. Called only after the bridge's auth, Host and
/// Origin checks.
pub trait ToolsHook: Send + Sync + 'static {
    /// One `/v1/local-tools…` request.
    fn handle<'a>(
        &'a self,
        method: &'a str,
        path: &'a str,
        body: &'a [u8],
        session: Option<&'a SessionCtx>,
    ) -> BoxFut<'a, (u16, serde_json::Value)>;
    /// A session was bound (sign-in, token refresh or account switch).
    fn session_bound<'a>(&'a self, session: &'a SessionCtx) -> BoxFut<'a, ()>;
    /// Sign-out, account switch or app exit.
    fn stop_all(&self) -> BoxFut<'_, ()>;
}

/// The bound session, as the tools hook needs it to decide whether it is
/// this app's own hosted workspace.
pub struct SessionCtx {
    pub local_url: Option<String>,
    pub local_token: Option<Secret>,
    pub session_url: String,
    pub session_token: Secret,
    pub account_id: String,
}
