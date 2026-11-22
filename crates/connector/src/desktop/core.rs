//! The desktop app's on-device connection service: one bound session (Metro
//! server + account), the connections paired for it, their workers (the
//! same claim → run → result loop as `metro-connector run`), vendor sign-ins
//! and the local workspace.
//!
//! Invariants:
//! - Workers run only while a session is bound, and only for profiles bound
//!   to exactly that (server, account) that the server confirmed are that
//!   account's own connections of the same kind.
//! - Ending the session (sign-out, account switch, app exit) stops every
//!   worker and sign-in and drops the session token.
//! - The device token is minted by this service with the person's session
//!   token and goes only to the profile on disk; keys and sign-ins stay on
//!   this device and are never sent to Metro.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::local::{LocalState, LocalWorkspaceHost, SessionCtx, ToolsHook};
use super::metro_api::{ApiError, MetroApi};
use super::store::{self, Binding};
use crate::metro::MetroClient;
use crate::netpolicy::validate_base_url;
use crate::openai_oauth::{self, Endpoints, LoginOptions};
use crate::profile::{
    DEFAULT_REQUEST_TIMEOUT_SECS, Kind, Profile, ProfileConfig, validate_models,
    validate_provider_key,
};
use crate::provider::Provider;
use crate::run::{RunOptions, RunState, Stop, run};
use crate::secret::clean_line;
use crate::{SafeError, Secret, claude_code};

/// An error the bridge returns: HTTP status, stable code, safe message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

pub fn err(status: u16, code: &'static str, message: impl AsRef<str>) -> BridgeError {
    BridgeError {
        status,
        code,
        message: clean_line(message.as_ref(), 300),
    }
}

type R<T> = Result<T, BridgeError>;

fn api_err(e: ApiError) -> BridgeError {
    match e {
        ApiError::Rejected => err(
            422,
            "session_rejected",
            "The Metro server did not accept this sign-in; sign in again",
        ),
        ApiError::NotFound => err(
            404,
            "not_your_connection",
            "No such connection for this account",
        ),
        ApiError::Unreachable => err(502, "server_unreachable", "Cannot reach the Metro server"),
        ApiError::Server(m) => err(502, "server_error", m),
    }
}

/// The text a person sees for a failed local action: the `SafeError` in the
/// chain, else `fallback`. Other error text (it may quote a provider) stays
/// out of the UI.
fn safe_text(e: &anyhow::Error, fallback: &str) -> String {
    e.chain()
        .find_map(|c| c.downcast_ref::<SafeError>())
        .map(|s| s.as_str().to_string())
        .unwrap_or_else(|| fallback.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    #[default]
    Stopped,
    NeedsSetup,
    Starting,
    Online,
    Offline,
    Unauthorized,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub struct LoginView {
    pub state: &'static str,
    pub auth_url: Option<String>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Live {
    worker: WorkerState,
    detail: Option<String>,
    epoch: u64,
    stop: Option<watch::Sender<bool>>,
    task: Option<JoinHandle<()>>,
    login: Option<LoginView>,
    login_task: Option<JoinHandle<()>>,
}

struct Session {
    server_url: String,
    account_id: String,
    token: Secret,
}

#[derive(Default)]
struct State {
    session: Option<Session>,
    live: HashMap<String, Live>,
    epoch: u64,
}

pub type Opener = Arc<dyn Fn(&str) + Send + Sync>;

/// How the service is set up. The launcher uses [`DesktopConfig::new`];
/// tests shorten the poll interval and replace the browser opener.
pub struct DesktopConfig {
    pub root: PathBuf,
    /// False when another app instance already serves connections.
    pub primary: bool,
    pub local: Option<Arc<dyn LocalWorkspaceHost>>,
    /// The desktop tools lane (`/v1/local-tools…`), when built in.
    pub tools: Option<Arc<dyn ToolsHook>>,
    pub poll_interval: Duration,
    /// Opens a sign-in URL in the person's browser.
    pub open_browser: Opener,
    pub oauth_endpoints: Endpoints,
}

impl DesktopConfig {
    pub fn new(root: PathBuf, primary: bool, local: Option<Arc<dyn LocalWorkspaceHost>>) -> Self {
        DesktopConfig {
            root,
            primary,
            local,
            tools: None,
            poll_interval: Duration::from_secs(2),
            open_browser: Arc::new(open_system_browser),
            oauth_endpoints: Endpoints::openai(),
        }
    }
}

/// Opens a URL with the system's opener, by absolute path only (no PATH
/// lookup). Failure is silent: the UI also gets the URL to open itself.
pub fn open_system_browser(url: &str) {
    if !(url.starts_with("http://127.0.0.1:") || url.starts_with("https://")) {
        return;
    }
    #[cfg(windows)]
    let candidates: Vec<PathBuf> = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|r| vec![r.join("explorer.exe")])
        .unwrap_or_default();
    #[cfg(target_os = "macos")]
    let candidates = vec![PathBuf::from("/usr/bin/open")];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates = vec![
        PathBuf::from("/usr/bin/xdg-open"),
        PathBuf::from("/usr/local/bin/xdg-open"),
        PathBuf::from("/bin/xdg-open"),
    ];
    if let Some(bin) = candidates.into_iter().find(|p| p.is_file()) {
        let _ = std::process::Command::new(bin)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

pub struct Desktop {
    cfg: DesktopConfig,
    /// Serializes changes (session, pairing, credentials); status never waits on it.
    ops: tokio::sync::Mutex<()>,
    state: Arc<Mutex<State>>,
    /// Watches a hosted session: stops the tools the moment the local
    /// server no longer runs with the bound URL and bearer.
    monitor: Mutex<Option<JoinHandle<()>>>,
    /// Every URL the local workspace was handed out at (URLs only, never
    /// tokens): a session to one of them is only allowed while that server
    /// runs there now with that bearer.
    local_urls: Mutex<std::collections::HashSet<String>>,
}

fn lock(s: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

/// Record a worker state change unless a newer worker replaced this one.
fn set_worker(state: &Mutex<State>, name: &str, epoch: u64, w: WorkerState, d: Option<String>) {
    let mut st = lock(state);
    if let Some(l) = st.live.get_mut(name)
        && l.epoch == epoch
    {
        l.worker = w;
        l.detail = d;
    }
}

struct Bound {
    profile: Profile,
    binding: Binding,
    config: ProfileConfig,
}

impl Desktop {
    pub fn new(cfg: DesktopConfig) -> Arc<Self> {
        Arc::new(Desktop {
            cfg,
            ops: tokio::sync::Mutex::new(()),
            state: Arc::new(Mutex::new(State::default())),
            monitor: Mutex::new(None),
            local_urls: Mutex::new(Default::default()),
        })
    }

    fn session(&self) -> R<(String, String, Secret)> {
        let st = lock(&self.state);
        match &st.session {
            Some(s) => Ok((s.server_url.clone(), s.account_id.clone(), s.token.clone())),
            None => Err(err(409, "no_session", "Sign in to a Metro server first")),
        }
    }

    /// The local server's URL and bearer, only while its manager reports it
    /// running and owning that address now. Never a remembered value: a
    /// server that stopped (or crashed) has no hosted context, so nothing
    /// sends its bearer to whatever process reuses the port later.
    fn running_local(&self) -> Option<(String, Secret)> {
        let h = self.cfg.local.as_ref()?;
        if h.state() != LocalState::Running {
            return None;
        }
        let w = h.current()?;
        // Re-check after reading: a stop in between must not slip through.
        if h.state() != LocalState::Running {
            return None;
        }
        Some((w.server_url, w.token))
    }

    /// The bound session plus this app's running local workspace, for the
    /// tools hook.
    fn session_ctx(&self) -> Option<SessionCtx> {
        let (server, account, token) = self.session().ok()?;
        let (local_url, local_token) = match self.running_local() {
            Some((u, t)) => (Some(u), Some(t)),
            None => (None, None),
        };
        Some(SessionCtx {
            local_url,
            local_token,
            session_url: server,
            session_token: token,
            account_id: account,
        })
    }

    /// True while the bound session is exactly the running local server's
    /// URL and bearer.
    fn hosted_now(&self) -> bool {
        let Ok((server, _, token)) = self.session() else {
            return false;
        };
        match self.running_local() {
            Some((u, t)) => {
                u == server
                    && t.expose().len() == token.expose().len()
                    && bool::from(subtle::ConstantTimeEq::ct_eq(
                        t.expose().as_bytes(),
                        token.expose().as_bytes(),
                    ))
            }
            None => false,
        }
    }

    /// A URL that was this app's local workspace is only usable while that
    /// server runs there now with exactly this bearer; otherwise another
    /// process may own the port, and nothing is sent to it.
    fn check_not_stale_local(&self, server: &str, token: &Secret) -> R<()> {
        let mut known = self
            .local_urls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(server);
        if let Some(w) = self.cfg.local.as_ref().and_then(|h| h.current()) {
            known |= w.server_url == server;
        }
        if !known {
            return Ok(());
        }
        let live = self.running_local().is_some_and(|(u, t)| {
            u == server
                && t.expose().len() == token.expose().len()
                && bool::from(subtle::ConstantTimeEq::ct_eq(
                    t.expose().as_bytes(),
                    token.expose().as_bytes(),
                ))
        });
        if live {
            Ok(())
        } else {
            Err(err(
                409,
                "local_not_running",
                "The local workspace isn't running with this sign-in; open it again",
            ))
        }
    }

    fn stop_monitor(&self) {
        if let Some(t) = self.monitor.lock().unwrap_or_else(|p| p.into_inner()).take() {
            t.abort();
        }
    }

    /// While a hosted session (this app's own local server) stays bound,
    /// end it as soon as the local server stops being that session's
    /// server: its address may be reused by an unrelated process, which must
    /// get neither the session bearer, a device token nor a tools token.
    /// Workers, sign-ins and tools stop; the UI sees `session: null` and
    /// opens the local workspace again. Remote sessions are never watched.
    fn start_monitor(self: &Arc<Self>) {
        self.stop_monitor();
        if !self.hosted_now() {
            return;
        }
        let me = Arc::downgrade(self);
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let Some(d) = me.upgrade() else { return };
                if !d.hosted_now() {
                    // Detach (not abort) ourselves before closing, which
                    // would otherwise cancel this very task.
                    drop(d.monitor.lock().unwrap_or_else(|p| p.into_inner()).take());
                    d.close_session().await;
                    return;
                }
            }
        });
        *self.monitor.lock().unwrap_or_else(|p| p.into_inner()) = Some(task);
    }

    /// One `/v1/local-tools…` request, after the bridge's checks.
    pub async fn tools_request(&self, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
        match &self.cfg.tools {
            Some(t) => {
                let ctx = self.session_ctx();
                t.handle(method, path, body, ctx.as_ref()).await
            }
            None => (
                404,
                json!({"error": {"code": "not_found", "message": "Local workspace tools are not available"}}),
            ),
        }
    }

    // ---------------------------------------------------------------- status

    pub async fn status(&self) -> Value {
        let (session, list) = {
            let st = lock(&self.state);
            match &st.session {
                Some(s) => (
                    json!({"server_url": s.server_url, "account_id": s.account_id}),
                    Some((s.server_url.clone(), s.account_id.clone())),
                ),
                None => (Value::Null, None),
            }
        };
        let mut connections = Vec::new();
        if let Some((server, account)) = list {
            for (p, b) in store::bound_profiles(&self.cfg.root, &server, &account) {
                connections.push(self.view(&p, &b).await);
            }
        }
        let local = match &self.cfg.local {
            None => "unavailable",
            Some(h) => match h.state() {
                LocalState::Stopped => "stopped",
                LocalState::Starting => "starting",
                LocalState::Running => "running",
                LocalState::Failed => "failed",
            },
        };
        json!({
            "bridge": 1,
            "version": env!("CARGO_PKG_VERSION"),
            "instance": if self.cfg.primary { "primary" } else { "secondary" },
            "kinds": Kind::ALL.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            "session": session,
            "local": local,
            "connections": connections,
        })
    }

    async fn credential(p: &Profile, cfg: Option<&ProfileConfig>, b: &Binding) -> &'static str {
        let vendor = p.vendor_dir();
        match b.kind {
            Kind::OpenAiOAuth => match openai_oauth::status(&vendor).await {
                Ok(s) if s.signed_in && s.plan_usage_enabled => "signed_in",
                _ => "signed_out",
            },
            Kind::ClaudeCode => {
                if claude_code::is_configured(&vendor) {
                    "signed_in"
                } else {
                    "signed_out"
                }
            }
            _ => {
                let base = cfg.and_then(|c| c.base_url.as_deref());
                match (p.provider_key_destination(), base) {
                    (Ok(Some(d)), Some(base)) if d == base => "key_saved",
                    _ if b.keyless => "no_key",
                    _ => "missing",
                }
            }
        }
    }

    /// Why a connection can't serve yet, or `None` when it is ready.
    async fn not_ready(p: &Profile, cfg: Option<&ProfileConfig>, b: &Binding) -> Option<String> {
        let Some(cfg) = cfg else {
            return Some("Pair this connection again".into());
        };
        if p.device_token_for(&cfg.server_url).is_err() {
            return Some("Pair this connection again".into());
        }
        let reason = match (b.kind, Self::credential(p, Some(cfg), b).await) {
            (_, "signed_in" | "key_saved" | "no_key") => None,
            (Kind::OpenAiOAuth, _) => Some("Sign in with ChatGPT on this device"),
            (Kind::ClaudeCode, _) => Some("Add your Anthropic Console API key on this device"),
            _ => Some("Add the endpoint's API key, or choose no key"),
        };
        if let Some(r) = reason {
            return Some(r.into());
        }
        if cfg.models.is_empty() {
            return Some("Approve at least one model".into());
        }
        None
    }

    async fn view(&self, p: &Profile, b: &Binding) -> Value {
        let cfg = p.load().ok();
        let credential = Self::credential(p, cfg.as_ref(), b).await;
        let not_ready = Self::not_ready(p, cfg.as_ref(), b).await;
        let (mut worker, mut detail, login) = {
            let st = lock(&self.state);
            match st.live.get(&p.name) {
                Some(l) => (l.worker, l.detail.clone(), l.login.clone()),
                None => (WorkerState::Stopped, None, None),
            }
        };
        if matches!(worker, WorkerState::Stopped | WorkerState::NeedsSetup)
            && let Some(r) = not_ready
        {
            worker = WorkerState::NeedsSetup;
            detail = Some(r);
        }
        json!({
            "connection_id": b.connection_id,
            "kind": b.kind.as_str(),
            "base_url": cfg.as_ref().and_then(|c| c.base_url.clone()),
            "models": cfg.as_ref().map(|c| c.models.clone()).unwrap_or_default(),
            "credential": credential,
            "worker": worker,
            "detail": detail,
            "login": login,
        })
    }

    // ---------------------------------------------------------------- workers

    fn spawn_worker(&self, p: Profile, cfg: ProfileConfig, token: Secret) {
        let name = p.name.clone();
        let (tx, rx) = watch::channel(false);
        let epoch = {
            let mut st = lock(&self.state);
            st.epoch += 1;
            st.epoch
        };
        let s_state = self.state.clone();
        let s_name = name.clone();
        let on_state = Arc::new(move |rs: RunState| {
            let w = match rs {
                RunState::Online => WorkerState::Online,
                RunState::Offline => WorkerState::Offline,
            };
            let d = (w == WorkerState::Offline)
                .then(|| "Cannot reach the Metro server; retrying".to_string());
            set_worker(&s_state, &s_name, epoch, w, d);
        });
        let opts = RunOptions {
            poll_interval: self.cfg.poll_interval,
            once: false,
            stop: Some(rx),
            on_state: Some(on_state),
        };
        let e_state = self.state.clone();
        let e_name = name.clone();
        let task = tokio::spawn(async move {
            let client = if cfg.kind.embeds_media() {
                MetroClient::new(&cfg.server_url, token, Duration::from_secs(120))
                    .map(|c| c.with_media_claims())
            } else {
                MetroClient::new(&cfg.server_url, token, Duration::from_secs(30))
            };
            let outcome = match client {
                Ok(client) => {
                    let provider = Provider {
                        profile: &p,
                        config: &cfg,
                    };
                    run(&client, &provider, &opts).await
                }
                Err(_) => Stop::Fatal("cannot start the HTTP client".into()),
            };
            let (w, d) = match outcome {
                Stop::Done => (WorkerState::Stopped, None),
                Stop::Unauthorized => (
                    WorkerState::Unauthorized,
                    Some(
                        "Metro no longer accepts this device's token; pair the connection again"
                            .into(),
                    ),
                ),
                Stop::Fatal(m) => (WorkerState::Error, Some(clean_line(&m, 200))),
            };
            set_worker(&e_state, &e_name, epoch, w, d);
        });
        let mut st = lock(&self.state);
        let l = st.live.entry(name).or_default();
        l.epoch = epoch;
        l.worker = WorkerState::Starting;
        l.detail = None;
        l.stop = Some(tx);
        l.task = Some(task);
    }

    /// Stop one worker and wait (bounded) for it to end.
    async fn stop_worker(&self, name: &str) {
        let task = {
            let mut st = lock(&self.state);
            st.epoch += 1;
            let epoch = st.epoch;
            match st.live.get_mut(name) {
                Some(l) => {
                    l.epoch = epoch;
                    if let Some(tx) = l.stop.take() {
                        let _ = tx.send(true);
                    }
                    l.worker = WorkerState::Stopped;
                    l.detail = None;
                    l.task.take()
                }
                None => None,
            }
        };
        if let Some(mut t) = task
            && tokio::time::timeout(Duration::from_secs(5), &mut t)
                .await
                .is_err()
        {
            // Stuck in a provider call: drop it (cancels its requests and,
            // for Claude Code, kills the CLI's process group).
            t.abort();
        }
    }

    fn cancel_login(&self, name: &str) {
        let mut st = lock(&self.state);
        if let Some(l) = st.live.get_mut(name) {
            if let Some(t) = l.login_task.take() {
                t.abort();
            }
            l.login = None;
        }
    }

    /// Stop everything: workers and sign-ins.
    async fn stop_all(&self) {
        let names: Vec<String> = lock(&self.state).live.keys().cloned().collect();
        for n in &names {
            self.cancel_login(n);
        }
        let mut waits = Vec::new();
        for n in &names {
            waits.push(self.stop_worker(n));
        }
        for w in waits {
            w.await;
        }
        lock(&self.state).live.clear();
    }

    /// (Re)start a connection's worker if the session matches and it is
    /// ready; otherwise make sure it is stopped.
    async fn refresh_worker(&self, p: &Profile, b: &Binding) {
        self.stop_worker(&p.name).await;
        let token_ok = {
            let st = lock(&self.state);
            st.session
                .as_ref()
                .is_some_and(|s| s.server_url == b.server_url && s.account_id == b.account_id)
        };
        if !token_ok || !self.cfg.primary {
            return;
        }
        let cfg = p.load().ok();
        if Self::not_ready(p, cfg.as_ref(), b).await.is_some() {
            return;
        }
        let Some(cfg) = cfg else { return };
        let Ok(token) = p.device_token_for(&cfg.server_url) else {
            return;
        };
        self.spawn_worker(p.clone(), cfg, token);
    }

    // ---------------------------------------------------------------- session

    pub async fn open_session(
        self: &Arc<Self>,
        server_url: &str,
        session_token: Secret,
    ) -> R<Value> {
        let _g = self.ops.lock().await;
        if !self.cfg.primary {
            return Err(err(
                409,
                "other_instance",
                "Another Metro window already serves this device's connections",
            ));
        }
        let server = validate_base_url(server_url, "server_url")
            .map_err(|e| err(422, "invalid", e.to_string()))?;
        let t = session_token.expose();
        if t.is_empty() || t.len() > 8192 || t.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(err(422, "invalid", "session_token is not a valid token"));
        }
        self.check_not_stale_local(&server, &session_token)?;
        let api = MetroApi::new(&server, session_token.clone()).map_err(api_err)?;
        let account = api.me().await.map_err(api_err)?;
        let same = {
            let st = lock(&self.state);
            st.session
                .as_ref()
                .is_some_and(|s| s.server_url == server && s.account_id == account)
        };
        self.stop_monitor();
        if !same {
            self.stop_all().await;
            if let Some(t) = &self.cfg.tools {
                t.stop_all().await;
            }
        }
        lock(&self.state).session = Some(Session {
            server_url: server.clone(),
            account_id: account.clone(),
            token: session_token,
        });
        if !same {
            let bound = store::bound_profiles(&self.cfg.root, &server, &account);
            if !bound.is_empty() {
                match api.connections().await {
                    Ok(conns) => {
                        for (p, b) in bound {
                            if conns
                                .iter()
                                .any(|(id, pid)| *id == b.connection_id && pid == b.kind.as_str())
                            {
                                self.refresh_worker(&p, &b).await;
                            } else {
                                let mut st = lock(&self.state);
                                let l = st.live.entry(p.name.clone()).or_default();
                                l.worker = WorkerState::Error;
                                l.detail = Some(
                                    "This connection no longer exists in Metro for this account; forget it"
                                        .into(),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        let msg = api_err(e).message;
                        let mut st = lock(&self.state);
                        for (p, _) in bound {
                            let l = st.live.entry(p.name.clone()).or_default();
                            l.worker = WorkerState::Error;
                            l.detail = Some(format!("Couldn't confirm with Metro: {msg}"));
                        }
                    }
                }
            }
        }
        if let (Some(t), Some(ctx)) = (&self.cfg.tools, self.session_ctx()) {
            t.session_bound(&ctx).await;
        }
        self.start_monitor();
        drop(_g);
        Ok(self.status().await)
    }

    pub async fn close_session(&self) -> Value {
        {
            let _g = self.ops.lock().await;
            self.stop_monitor();
            self.stop_all().await;
            if let Some(t) = &self.cfg.tools {
                t.stop_all().await;
            }
            lock(&self.state).session = None;
        }
        self.status().await
    }

    /// App exit: stop everything, forget the session, stop the local workspace.
    pub async fn shutdown(&self) {
        self.close_session().await;
        if let Some(h) = &self.cfg.local {
            h.shutdown().await;
        }
    }

    // ---------------------------------------------------------------- connections

    fn check_id(id: &str) -> R<()> {
        if store::valid_connection_id(id) {
            Ok(())
        } else {
            Err(err(
                404,
                "not_your_connection",
                "No such connection for this account",
            ))
        }
    }

    fn bound(&self, id: &str) -> R<Bound> {
        Self::check_id(id)?;
        let (server, account, _) = self.session()?;
        let not_paired = || {
            err(
                404,
                "not_paired",
                "This connection is not set up on this device",
            )
        };
        let profile =
            store::profile_for(&self.cfg.root, &server, &account, id).map_err(|_| not_paired())?;
        let binding = match store::load_binding(&profile) {
            Ok(Some(b))
                if b.server_url == server && b.account_id == account && b.connection_id == id =>
            {
                b
            }
            _ => return Err(not_paired()),
        };
        let config = profile.load().map_err(|_| not_paired())?;
        Ok(Bound {
            profile,
            binding,
            config,
        })
    }

    async fn connection(&self, id: &str) -> R<Value> {
        let b = self.bound(id)?;
        Ok(self.view(&b.profile, &b.binding).await)
    }

    pub async fn pair(
        &self,
        id: &str,
        kind: &str,
        base_url: Option<&str>,
        models: Option<Vec<String>>,
    ) -> R<Value> {
        let _g = self.ops.lock().await;
        let (server, account, token) = self.session()?;
        Self::check_id(id)?;
        let kind: Kind = kind
            .parse()
            .map_err(|_| err(422, "invalid", "Unknown connection kind"))?;
        let base_url = match (kind.uses_endpoint(), base_url) {
            (true, Some(u)) => Some(
                validate_base_url(u, "base_url").map_err(|e| err(422, "invalid", e.to_string()))?,
            ),
            (true, None) => None,
            (false, Some(_)) => {
                return Err(err(
                    422,
                    "invalid",
                    "base_url is only for endpoint connections",
                ));
            }
            (false, None) => None,
        };
        if let Some(m) = &models {
            validate_models(m).map_err(|e| err(422, "invalid", e.to_string()))?;
        }
        let api = MetroApi::new(&server, token).map_err(api_err)?;
        let conns = api.connections().await.map_err(api_err)?;
        match conns.iter().find(|(cid, _)| cid == id) {
            None => {
                return Err(err(
                    404,
                    "not_your_connection",
                    "No such connection for this account",
                ));
            }
            Some((_, pid)) if pid != kind.as_str() => {
                return Err(err(
                    422,
                    "kind_mismatch",
                    "That connection in Metro is of another kind",
                ));
            }
            _ => {}
        }
        let profile = store::profile_for(&self.cfg.root, &server, &account, id)
            .map_err(|_| err(500, "internal", "Bad profile"))?;
        let prev_cfg = profile
            .load()
            .ok()
            .filter(|c| c.kind == kind && c.server_url == server);
        let prev_binding = store::load_binding(&profile).ok().flatten();
        let base_url = base_url.or_else(|| prev_cfg.as_ref().and_then(|c| c.base_url.clone()));
        if kind.uses_endpoint() && base_url.is_none() {
            return Err(err(422, "invalid", "base_url is required for this kind"));
        }
        self.stop_worker(&profile.name).await;
        let device_token = api.issue_device_token(id).await.map_err(api_err)?;
        let base_changed = prev_cfg.as_ref().and_then(|c| c.base_url.clone()) != base_url;
        let cfg = ProfileConfig {
            version: 1,
            kind,
            server_url: server.clone(),
            models: models
                .or_else(|| prev_cfg.as_ref().map(|c| c.models.clone()))
                .unwrap_or_default(),
            base_url,
            request_timeout_secs: prev_cfg
                .as_ref()
                .map(|c| c.request_timeout_secs)
                .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS),
        };
        let binding = Binding {
            version: 1,
            server_url: server.clone(),
            account_id: account.clone(),
            connection_id: id.to_string(),
            kind,
            keyless: prev_binding
                .map(|b| b.keyless && !base_changed)
                .unwrap_or(false),
        };
        let write = (|| -> anyhow::Result<()> {
            profile.ensure()?;
            if base_changed {
                profile.forget_provider_key()?;
            }
            profile.save_device_token(&server, &device_token)?;
            profile.save(&cfg)?;
            store::save_binding(&profile, &binding)
        })();
        drop(device_token);
        write.map_err(|_| {
            err(
                500,
                "storage_error",
                "Couldn't save the connection on this device",
            )
        })?;
        self.refresh_worker(&profile, &binding).await;
        drop(_g);
        Ok(self.view(&profile, &binding).await)
    }

    pub async fn rotate(&self, id: &str) -> R<Value> {
        let _g = self.ops.lock().await;
        let b = self.bound(id)?;
        let (server, _, token) = self.session()?;
        let api = MetroApi::new(&server, token).map_err(api_err)?;
        self.stop_worker(&b.profile.name).await;
        let t = api.issue_device_token(id).await.map_err(api_err)?;
        b.profile
            .save_device_token(&server, &t)
            .map_err(|_| err(500, "storage_error", "Couldn't save the new device token"))?;
        drop(t);
        self.refresh_worker(&b.profile, &b.binding).await;
        drop(_g);
        self.connection(id).await
    }

    pub async fn set_credentials(
        self: &Arc<Self>,
        id: &str,
        api_key: Option<Secret>,
        no_api_key: bool,
        open_browser: bool,
    ) -> R<Value> {
        let _g = self.ops.lock().await;
        let mut b = self.bound(id)?;
        b.profile
            .ensure()
            .map_err(|_| err(500, "storage_error", "Couldn't prepare the profile"))?;
        match b.binding.kind {
            k if k.uses_endpoint() => {
                let base = b.config.base_url.clone().unwrap_or_default();
                match (api_key, no_api_key) {
                    (Some(k), false) if !k.is_empty() => {
                        validate_provider_key(&k)
                            .map_err(|e| err(422, "invalid", e.to_string()))?;
                        b.profile
                            .save_provider_key(&base, Some(&k))
                            .map_err(|_| err(500, "storage_error", "Couldn't save the key"))?;
                        b.binding.keyless = false;
                    }
                    (None, true) => {
                        b.profile
                            .save_provider_key(&base, None)
                            .map_err(|_| err(500, "storage_error", "Couldn't save the choice"))?;
                        b.binding.keyless = true;
                    }
                    _ => {
                        return Err(err(
                            422,
                            "invalid",
                            "Send either api_key or no_api_key: true",
                        ));
                    }
                }
                store::save_binding(&b.profile, &b.binding)
                    .map_err(|_| err(500, "storage_error", "Couldn't save the connection"))?;
            }
            Kind::ClaudeCode => {
                let (Some(key), false) = (api_key, no_api_key) else {
                    return Err(err(
                        422,
                        "invalid",
                        "Send api_key (an Anthropic Console API key)",
                    ));
                };
                claude_code::validate_key(&key).map_err(|e| {
                    err(
                        422,
                        "credential_rejected",
                        safe_text(&e, "That key can't be used"),
                    )
                })?;
                self.stop_worker(&b.profile.name).await;
                claude_code::login_with_key(&b.profile.vendor_dir(), key)
                    .await
                    .map_err(|e| {
                        err(
                            422,
                            "credential_rejected",
                            safe_text(&e, "Claude Code couldn't be set up with that key"),
                        )
                    })?;
            }
            Kind::OpenAiOAuth => {
                if api_key.is_some() || no_api_key {
                    return Err(err(
                        422,
                        "invalid",
                        "This connection signs in with ChatGPT; send no key",
                    ));
                }
                drop(_g);
                return self.start_oauth(b, open_browser).await;
            }
            _ => return Err(err(422, "unsupported", "Unsupported connection kind")),
        }
        self.refresh_worker(&b.profile, &b.binding).await;
        drop(_g);
        Ok(self.view(&b.profile, &b.binding).await)
    }

    async fn start_oauth(self: &Arc<Self>, b: Bound, open_browser: bool) -> R<Value> {
        let name = b.profile.name.clone();
        let connection_id = b.binding.connection_id.clone();
        {
            let mut st = lock(&self.state);
            let l = st.live.entry(name.clone()).or_default();
            if l.login_task.as_ref().is_some_and(|t| !t.is_finished()) {
                return Err(err(
                    409,
                    "login_in_progress",
                    "A sign-in is already waiting in the browser",
                ));
            }
            l.login = Some(LoginView {
                state: "waiting",
                auth_url: None,
                error: None,
            });
        }
        let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();
        let opener = self.cfg.open_browser.clone();
        let options = LoginOptions {
            endpoints: self.cfg.oauth_endpoints.clone(),
            open_browser: Box::new(move |url: &str| {
                let _ = url_tx.send(url.to_string());
                if open_browser {
                    opener(url);
                }
                Ok(())
            }),
            ..LoginOptions::default()
        };
        let me = self.clone();
        let vendor = b.profile.vendor_dir();
        let t_name = name.clone();
        let task = tokio::spawn(async move {
            let r = openai_oauth::login_with(&vendor, options).await;
            let view = match &r {
                Ok(()) => LoginView {
                    state: "done",
                    auth_url: None,
                    error: None,
                },
                Err(e) => LoginView {
                    state: "failed",
                    auth_url: None,
                    error: Some(safe_text(e, "Sign-in did not finish; try again")),
                },
            };
            if let Some(l) = lock(&me.state).live.get_mut(&t_name) {
                l.login = Some(view);
            }
            if r.is_ok() {
                let _g = me.ops.lock().await;
                me.refresh_worker(&b.profile, &b.binding).await;
            }
        });
        if let Some(l) = lock(&self.state).live.get_mut(&name) {
            l.login_task = Some(task);
        }
        match tokio::time::timeout(Duration::from_secs(20), url_rx).await {
            Ok(Ok(url)) => {
                let mut st = lock(&self.state);
                if let Some(l) = st.live.get_mut(&name)
                    && let Some(v) = l.login.as_mut()
                    && v.state == "waiting"
                {
                    v.auth_url = Some(url);
                }
            }
            _ => {
                // Failed before a browser was needed: give the task a moment
                // to record why.
                for _ in 0..40 {
                    let done = lock(&self.state)
                        .live
                        .get(&name)
                        .and_then(|l| l.login.as_ref())
                        .is_some_and(|v| v.state != "waiting");
                    if done {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        self.connection(&connection_id).await
    }

    pub async fn delete_credentials(&self, id: &str) -> R<Value> {
        let _g = self.ops.lock().await;
        let mut b = self.bound(id)?;
        self.cancel_login(&b.profile.name);
        self.stop_worker(&b.profile.name).await;
        let outcome = Provider {
            profile: &b.profile,
            config: &b.config,
        }
        .logout()
        .await;
        b.binding.keyless = false;
        let _ = store::save_binding(&b.profile, &b.binding);
        if let Err(e) = outcome {
            let mut st = lock(&self.state);
            let l = st.live.entry(b.profile.name.clone()).or_default();
            l.detail = Some(safe_text(
                &e,
                "Signed out on this device; the provider did not confirm",
            ));
        }
        drop(_g);
        Ok(self.view(&b.profile, &b.binding).await)
    }

    pub async fn available_models(&self, id: &str) -> R<Value> {
        let b = self.bound(id)?;
        let p = Provider {
            profile: &b.profile,
            config: &b.config,
        };
        match tokio::time::timeout(Duration::from_secs(60), p.available_models()).await {
            Ok(Ok(models)) => Ok(json!({"models": models})),
            Ok(Err(e)) => Err(err(
                502,
                "provider_error",
                safe_text(&e, "Couldn't list the provider's models from this device"),
            )),
            Err(_) => Err(err(
                502,
                "provider_error",
                "The provider did not answer in time",
            )),
        }
    }

    pub async fn set_models(&self, id: &str, models: Vec<String>) -> R<Value> {
        let _g = self.ops.lock().await;
        let mut b = self.bound(id)?;
        if models.is_empty() {
            return Err(err(422, "invalid", "Approve at least one model"));
        }
        validate_models(&models).map_err(|e| err(422, "invalid", e.to_string()))?;
        b.config.models = models;
        b.profile
            .save(&b.config)
            .map_err(|_| err(500, "storage_error", "Couldn't save the models"))?;
        self.refresh_worker(&b.profile, &b.binding).await;
        drop(_g);
        Ok(self.view(&b.profile, &b.binding).await)
    }

    pub async fn set_endpoint(&self, id: &str, base_url: &str) -> R<Value> {
        let _g = self.ops.lock().await;
        let mut b = self.bound(id)?;
        if !b.binding.kind.uses_endpoint() {
            return Err(err(422, "unsupported", "This connection has no endpoint"));
        }
        let base = validate_base_url(base_url, "base_url")
            .map_err(|e| err(422, "invalid", e.to_string()))?;
        if b.config.base_url.as_deref() != Some(base.as_str()) {
            self.stop_worker(&b.profile.name).await;
            b.config.base_url = Some(base);
            b.binding.keyless = false;
            let w = (|| -> anyhow::Result<()> {
                b.profile.forget_provider_key()?;
                b.profile.save(&b.config)?;
                store::save_binding(&b.profile, &b.binding)
            })();
            w.map_err(|_| err(500, "storage_error", "Couldn't save the endpoint"))?;
        }
        self.refresh_worker(&b.profile, &b.binding).await;
        drop(_g);
        Ok(self.view(&b.profile, &b.binding).await)
    }

    pub async fn forget(&self, id: &str, revoke: bool) -> R<Value> {
        let _g = self.ops.lock().await;
        let b = self.bound(id)?;
        let (server, _, token) = self.session()?;
        self.cancel_login(&b.profile.name);
        self.stop_worker(&b.profile.name).await;
        let revoked = if revoke {
            let api = MetroApi::new(&server, token).map_err(api_err)?;
            match api.revoke_device_token(id).await {
                Ok(()) | Err(ApiError::NotFound) => true,
                Err(ApiError::Rejected) => {
                    return Err(api_err(ApiError::Rejected));
                }
                Err(_) => false,
            }
        } else {
            false
        };
        let _ = Provider {
            profile: &b.profile,
            config: &b.config,
        }
        .logout()
        .await;
        b.profile.forget_all().map_err(|_| {
            err(
                500,
                "storage_error",
                "Couldn't delete the connection's files",
            )
        })?;
        lock(&self.state).live.remove(&b.profile.name);
        Ok(json!({"forgotten": true, "revoked": revoked}))
    }

    // ---------------------------------------------------------------- local workspace

    pub async fn open_local(&self) -> R<(String, Secret)> {
        let Some(h) = &self.cfg.local else {
            return Err(err(
                503,
                "local_unavailable",
                "This Metro app has no local workspace (metro-server is not installed beside it)",
            ));
        };
        let w = h
            .open()
            .await
            .map_err(|e| err(502, "local_failed", e.as_str()))?;
        let url = validate_base_url(&w.server_url, "local server")
            .ok()
            .filter(|u| *u == w.server_url && crate::netpolicy::url_is_loopback(u))
            .ok_or_else(|| {
                err(
                    502,
                    "local_failed",
                    "The local workspace reported an invalid address",
                )
            })?;
        self.local_urls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(url.clone());
        Ok((url, w.token))
    }
}
