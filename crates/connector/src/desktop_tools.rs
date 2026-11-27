//! Local workspace tools for a Space hosted by the desktop app's own Metro
//! server: read-only `workspace.list` / `workspace.read` over a private
//! folder per Space, without a separate runner install.
//!
//! What this is and isn't:
//!
//! - Only for the desktop's **own** hosted server. The bridge builds a
//!   [`BoundSession`] with [`BoundSession::verify`], which marks it hosted
//!   only when the bound session's URL and bearer are exactly the ones the
//!   desktop's local server manager handed out for the server it is running
//!   now. Any other session ([`BoundSession::signed_in`]: a remote server,
//!   another localhost process) gets no per-Space local tools: there is no
//!   fallback and no global token.
//! - Explicit, per Space, owner only. The Space owner turns it on for one
//!   Space. The real server checks ownership; this module only asks it.
//!   Pairing uses `POST /v1/spaces/{sid}/runner-token/issue-if-empty`,
//!   which the server applies atomically only
//!   while the Space has no runner at all, so an existing dedicated token or
//!   attached registered runner is never replaced. Turning it off uses
//!   the confirmed local pairing revision and the server's dedicated-only
//!   `revoke-if-current` route, so it can't revoke a newer token.
//! - A dedicated token per Space (shown to nobody, kept mode 600 under the
//!   desktop root). A token the server refuses (401) marks the pairing
//!   revoked and is never re-issued automatically.
//! - Read-only and confined: see [`folder`]. No check, MCP, commands or
//!   programs; those stay with the separately installed Linux runner.
//! - Never model auth, and never anything for personal Chat: a dedicated
//!   runner token serves only its Space's jobs, and personal Chat has no
//!   runner tools on the server.
//! - Pairing workers run only while the bridge has the desktop's own server
//!   bound, and stop on sign-out, server or account switch, and app exit
//!   ([`DesktopTools::stop_all`]).
//!
//! Bridge integration (launcher lane):
//!
//! ```text
//! let tools = DesktopTools::new(desktop_root.join("local-tools"));
//! // GET /v1/local-tools, POST /v1/local-tools/spaces/{sid}/enable|disable-if-current:
//! let (status, json) = tools.handle(method, path, body, bound_session_or_none).await;
//! tools.resume(session).await;  // after POST /v1/session bound a server
//! tools.stop_all().await;       // DELETE /v1/session, switch, shutdown
//! ```
//!
//! Separately, [`runner`] keeps one **personal registered runner** for
//! this computer, built into the app, on whichever server the person is
//! signed in to: set up by an explicit action in Settings (or, on the own
//! hosted server, automatically when the installer's marker asked for it),
//! then resumed with every session of that server and account. It is a
//! different thing from the per-Space dedicated pairings above: neither
//! stands in for the other, and Metro never attaches it to a Space.

pub mod folder;
pub mod hosted;
pub mod runner;
pub mod store;
pub mod worker;

use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use subtle::ConstantTimeEq;

use crate::Secret;
use hosted::{CallError, Hosted, SpaceRunner};
use store::{Pairing, PairingState};
use worker::WorkerState;

/// The bridge's bound session (server, bearer, account), and whether it is
/// the desktop's own hosted server as it runs now ([`BoundSession::verify`]).
#[derive(Clone)]
pub struct BoundSession {
    server_url: String,
    token: Secret,
    account_id: String,
    hosted: bool,
}

impl std::fmt::Debug for BoundSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundSession")
            .field("server_url", &self.server_url)
            .field("account_id", &self.account_id)
            .field("hosted", &self.hosted)
            .finish_non_exhaustive()
    }
}

fn valid_account(account_id: &str) -> bool {
    !account_id.is_empty() && account_id.len() <= 256 && !account_id.chars().any(char::is_control)
}

/// `http://127.0.0.1:<port>`, nothing else (no path, query or user info).
fn is_local_server_url(raw: &str) -> bool {
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    u.scheme() == "http"
        && u.host() == Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST))
        && u.port().is_some()
        && u.username().is_empty()
        && u.password().is_none()
        && u.query().is_none()
        && u.fragment().is_none()
        && u.path() == "/"
        && !raw.ends_with('/')
}

impl BoundSession {
    /// `Some` only when the local server manager reports a running server
    /// (`local_url`, `local_token` from its `LocalWorkspace`) and the bound
    /// session uses exactly that URL and that bearer. `account_id` is the
    /// id the bridge learned from that server's `GET /v1/me`.
    pub fn verify(
        local_url: Option<&str>,
        local_token: Option<&Secret>,
        session_url: &str,
        session_token: &Secret,
        account_id: &str,
    ) -> Option<Self> {
        let (local_url, local_token) = (local_url?, local_token?);
        if local_url != session_url || !is_local_server_url(session_url) {
            return None;
        }
        let (a, b) = (
            local_token.expose().as_bytes(),
            session_token.expose().as_bytes(),
        );
        if a.is_empty() || a.len() != b.len() || !bool::from(a.ct_eq(b)) {
            return None;
        }
        if !valid_account(account_id) {
            return None;
        }
        Some(BoundSession {
            server_url: session_url.to_owned(),
            token: session_token.clone(),
            account_id: account_id.to_owned(),
            hosted: true,
        })
    }

    /// Any other bound session: the bridge already validated the server URL
    /// (and the person's consent for plain HTTP) and learned `account_id`
    /// from that server's `GET /v1/me`. Serves this computer's runner only,
    /// never per-Space local tools.
    pub fn signed_in(session_url: &str, session_token: &Secret, account_id: &str) -> Option<Self> {
        let t = session_token.expose();
        if session_url.is_empty() || t.is_empty() || !valid_account(account_id) {
            return None;
        }
        Some(BoundSession {
            server_url: session_url.trim_end_matches('/').to_owned(),
            token: session_token.clone(),
            account_id: account_id.to_owned(),
            hosted: false,
        })
    }

    /// The desktop's own hosted server.
    pub fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }
}

/// A refusal, as the bridge's `{error:{code,message}}`.
#[derive(Debug, Clone)]
struct ToolsError {
    status: u16,
    code: &'static str,
    message: String,
}

impl ToolsError {
    fn new(status: u16, code: &'static str, message: impl Into<String>) -> Self {
        ToolsError {
            status,
            code,
            message: crate::secret::clean_line(&message.into(), 300),
        }
    }

    fn body(&self) -> Value {
        json!({"error": {"code": self.code, "message": self.message}})
    }
}

const NO_SESSION: &str = "Sign in to a Metro server to use this computer's runner.";

const NOT_LOCAL: &str = "Local tools work only in this computer's own Metro workspace. \
     Open the local workspace to use them; other servers use their own runners.";

fn from_call(e: CallError) -> ToolsError {
    match e {
        CallError::Unauthorized => ToolsError::new(
            422,
            "session_rejected",
            "The Metro server didn't accept this sign-in. Sign in again.",
        ),
        CallError::Unreachable(m) => ToolsError::new(502, "server_unreachable", m),
        CallError::Forbidden(m) => ToolsError::new(403, "not_owner", m),
        CallError::NotFound(m) => ToolsError::new(404, "not_found", m),
        CallError::Precondition(m) => ToolsError::new(409, "runner_present", m),
        CallError::Other(m) => ToolsError::new(502, "server_error", m),
    }
}

fn fs_error(what: &str) -> ToolsError {
    ToolsError::new(
        500,
        "local_tools_failed",
        format!("Metro couldn't {what} in its local tools folder on this computer."),
    )
}

fn pairing_changed() -> ToolsError {
    ToolsError::new(
        409,
        "pairing_changed",
        "This local pairing or account changed. Review the current pairing before turning it off.",
    )
}

struct Inner {
    root: Option<Arc<File>>,
    /// (server URL, account id) the workers belong to.
    bound: Option<(String, String)>,
    workers: HashMap<String, worker::Handle>,
    /// This computer's personal runner (see [`runner`]).
    runner: runner::Slot,
    /// Why the last start didn't serve every pairing (`None`: it did).
    /// Shown in the status; the next session bind tries again.
    problem: Option<String>,
    /// `problem` is only that the folder couldn't be opened: a later
    /// successful open clears it.
    problem_is_root: bool,
}

/// The desktop's local tools manager. One per launcher; `Send + Sync`.
pub struct DesktopTools {
    root_path: PathBuf,
    inner: tokio::sync::Mutex<Inner>,
    store_lock: Arc<Mutex<()>>,
    marker: runner::Marker,
}

impl DesktopTools {
    /// `root` is `<desktop root>/local-tools`; created private on first use.
    pub fn new(root: PathBuf) -> Self {
        DesktopTools {
            root_path: root,
            inner: tokio::sync::Mutex::new(Inner {
                root: None,
                bound: None,
                workers: HashMap::new(),
                runner: runner::Slot::default(),
                problem: None,
                problem_is_root: false,
            }),
            store_lock: Arc::new(Mutex::new(())),
            marker: runner::Marker::Absent,
        }
    }

    /// What the installer asked for ([`runner::installer_marker`]): with
    /// [`runner::Marker::Accepted`], this computer's personal registered
    /// runner is also set up without a press on the own hosted server.
    pub fn with_local_runner(mut self, marker: runner::Marker) -> Self {
        self.marker = marker;
        self
    }

    fn root(&self, inner: &mut Inner) -> Result<Arc<File>, ToolsError> {
        if let Some(r) = &inner.root {
            return Ok(r.clone());
        }
        let f = store::open_root(&self.root_path).map_err(|_| fs_error("open its folder"))?;
        let f = Arc::new(f);
        inner.root = Some(f.clone());
        Ok(f)
    }

    /// The root, or the person-facing reason it can't be opened (then
    /// also recorded as the problem, for this computer's runner too). A
    /// successful open clears a problem that was only this.
    fn root_or_problem(&self, inner: &mut Inner) -> Result<Arc<File>, String> {
        if let Some(r) = &inner.root {
            return Ok(r.clone());
        }
        let opened = store::open_root(&self.root_path).map_err(|e| {
            crate::secret::clean_line(
                &format!(
                    "Metro couldn't open its local tools folder {}: {e}",
                    self.root_path.display()
                ),
                300,
            )
        });
        let f = match opened {
            Ok(f) => Arc::new(f),
            Err(why) => {
                runner::start_failed(&mut inner.runner, &why);
                inner.problem = Some(why.clone());
                inner.problem_is_root = true;
                return Err(why);
            }
        };
        inner.root = Some(f.clone());
        if inner.problem_is_root {
            if let Some(why) = inner.problem.take() {
                runner::clear_start_failed(&mut inner.runner, &why);
            }
            inner.problem_is_root = false;
        }
        Ok(f)
    }

    /// Stop workers that belong to another server or account than `session`.
    async fn rebind(inner: &mut Inner, session: Option<&BoundSession>) {
        let key = session.map(|h| (h.server_url.clone(), h.account_id.clone()));
        if inner.bound != key {
            for (_, w) in inner.workers.drain() {
                w.stop().await;
            }
            inner.runner.stop().await;
            inner.bound = key;
            inner.problem = None;
            inner.problem_is_root = false;
        }
    }

    /// Route a bridge request under `/v1/local-tools`. The bridge has
    /// already checked its own bearer, Host and Origin. `session` is the
    /// bound session, if any; per-Space local tools need it to be hosted.
    pub async fn handle(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        session: Option<BoundSession>,
    ) -> (u16, Value) {
        let r = self.route(method, path, body, session).await;
        match r {
            Ok(v) => (200, v),
            Err(e) => (e.status, e.body()),
        }
    }

    async fn route(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        session: Option<BoundSession>,
    ) -> Result<Value, ToolsError> {
        let rest = path
            .strip_prefix("/v1/local-tools")
            .ok_or_else(|| ToolsError::new(404, "not_found", "unknown path"))?;
        if rest.is_empty() {
            if method != "GET" {
                return Err(ToolsError::new(405, "method_not_allowed", "use GET"));
            }
            return self.status(session.as_ref()).await;
        }
        if let Some(r) = rest.strip_prefix("/runner") {
            return self.runner_route(method, r, body, session).await;
        }
        let hosted = session.clone().filter(BoundSession::is_hosted);
        let Some((sid, action)) = rest
            .strip_prefix("/spaces/")
            .and_then(|r| r.split_once('/'))
        else {
            return Err(ToolsError::new(404, "not_found", "unknown path"));
        };
        if !matches!(action, "enable" | "disable" | "disable-if-current") {
            return Err(ToolsError::new(404, "not_found", "unknown path"));
        }
        if method != "POST" {
            return Err(ToolsError::new(405, "method_not_allowed", "use POST"));
        }
        if !store::valid_space_id(sid) {
            return Err(ToolsError::new(404, "not_found", "no such Space"));
        }
        if action == "disable-if-current" {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Confirmation {
                account_id: String,
                pairing_revision: String,
            }
            let confirmation: Confirmation = serde_json::from_slice(body).map_err(|_| {
                ToolsError::new(
                    422,
                    "invalid",
                    "a pairing revision and account are required",
                )
            })?;
            let Some(h) = hosted.filter(|h| h.account_id == confirmation.account_id) else {
                return Err(pairing_changed());
            };
            return self
                .disable(&h, sid, Some(&confirmation.pairing_revision))
                .await;
        }
        if !body.iter().all(u8::is_ascii_whitespace)
            && serde_json::from_slice::<serde_json::Map<String, Value>>(body)
                .map(|m| !m.is_empty())
                .unwrap_or(true)
        {
            return Err(ToolsError::new(422, "invalid", "the body must be {}"));
        }
        let Some(hosted) = hosted else {
            // Not our own server: make sure no pairing keeps running.
            match &session {
                None => self.stop_all().await,
                Some(s) => Self::rebind(&mut *self.inner.lock().await, Some(s)).await,
            }
            return Err(ToolsError::new(409, "not_local", NOT_LOCAL));
        };
        if action == "enable" {
            self.enable(&hosted, sid).await
        } else {
            self.disable(&hosted, sid, None).await
        }
    }

    fn entry(&self, inner: &Inner, account: &str, sid: &str, p: Option<&Pairing>) -> Value {
        let folder = store::workspace_path(&self.root_path, account, sid);
        let w = inner.workers.get(sid).map(|w| (w.etag.clone(), w.status()));
        let (enabled, state, worker, detail) = match p {
            None => (false, "off", WorkerState::Stopped, None),
            Some(p) if p.state == PairingState::Revoked => (
                true,
                "revoked",
                WorkerState::Revoked,
                Some(
                    "The local Metro server no longer accepts this pairing (revoked or \
                     replaced). Turn local tools on again to pair anew."
                        .to_string(),
                ),
            ),
            Some(p) => match w {
                Some((etag, s)) if etag == p.etag => (true, "on", s.state, s.detail),
                _ => match &inner.problem {
                    Some(why) => (true, "on", WorkerState::Failed, Some(why.clone())),
                    None => (true, "on", WorkerState::Stopped, None),
                },
            },
        };
        json!({
            "space_id": sid,
            "enabled": enabled,
            "state": state,
            "worker": worker,
            "detail": detail,
            "folder": folder.to_string_lossy(),
            "pairing_revision": p.and_then(|p| hosted::pairing_revision(&p.etag)),
            "tools": ["workspace.list", "workspace.read"],
        })
    }

    fn runner_status(&self, inner: &Inner, session: Option<&BoundSession>) -> Value {
        let Some(h) = session else {
            return runner::status(&inner.runner, &self.marker, None);
        };
        let rec = inner.root.as_ref().and_then(|root| {
            let _g = self.store_lock.lock();
            runner::load(root, h).ok().flatten()
        });
        runner::status(&inner.runner, &self.marker, Some((&self.root_path, h, rec)))
    }

    /// `/v1/local-tools/runner…`: this computer's personal runner.
    async fn runner_route(
        &self,
        method: &str,
        rest: &str,
        body: &[u8],
        session: Option<BoundSession>,
    ) -> Result<Value, ToolsError> {
        if rest.is_empty() {
            if method != "GET" {
                return Err(ToolsError::new(405, "method_not_allowed", "use GET"));
            }
            let mut inner = self.inner.lock().await;
            Self::rebind(&mut inner, session.as_ref()).await;
            if session.is_some() {
                // On failure the reason is recorded: say why instead of
                // failing the status read.
                let _ = self.root_or_problem(&mut inner);
            }
            return Ok(self.runner_status(&inner, session.as_ref()));
        }
        let folder_sid = rest
            .strip_prefix("/spaces/")
            .and_then(|r| r.strip_suffix("/folder"))
            .filter(|s| !s.contains('/'));
        let action = rest.strip_prefix('/').unwrap_or_default();
        if !matches!(action, "setup" | "reconnect" | "forget") && folder_sid.is_none() {
            return Err(ToolsError::new(404, "not_found", "unknown path"));
        }
        if method != "POST" {
            return Err(ToolsError::new(405, "method_not_allowed", "use POST"));
        }
        // `{}`, or `{"runner_id": "<id>"}`: for reconnect, a runner the
        // person chose from their own list; for forget, the one they just
        // deleted. Always by id.
        let fields = if body.iter().all(u8::is_ascii_whitespace) {
            serde_json::Map::new()
        } else {
            serde_json::from_slice::<serde_json::Map<String, Value>>(body)
                .map_err(|_| ToolsError::new(422, "invalid", "the body must be a JSON object"))?
        };
        let choice = match fields.get("runner_id") {
            None => None,
            Some(Value::String(id))
                if matches!(action, "reconnect" | "forget") && hosted::valid_job_id(id) =>
            {
                Some(id.clone())
            }
            Some(_) => return Err(ToolsError::new(422, "invalid", "invalid runner_id")),
        };
        if fields.keys().any(|k| k != "runner_id") {
            return Err(ToolsError::new(422, "invalid", "unknown field"));
        }
        let Some(h) = session else {
            self.stop_all().await;
            return Err(ToolsError::new(409, "no_session", NO_SESSION));
        };
        let mut inner = self.inner.lock().await;
        Self::rebind(&mut inner, Some(&h)).await;
        let root = self.root(&mut inner)?;
        let api = Hosted::new(&h.server_url).map_err(|_| fs_error("start"))?;
        Self::check_account(&api, &h).await?;
        let cx = runner::Ctx {
            marker: &self.marker,
            root: &root,
            root_path: &self.root_path,
            session: &h,
            api: &api,
            store_lock: &self.store_lock,
        };
        if let Some(sid) = folder_sid {
            // Asked right after attaching: a runner that didn't start (or
            // stopped) gets another go now, not only at the next sign-in.
            runner::ensure(&mut inner.runner, &cx).await;
            return runner::folder_for(&cx, sid).await;
        }
        match action {
            "setup" => runner::setup(&mut inner.runner, &cx).await?,
            "forget" => {
                let id = choice
                    .ok_or_else(|| ToolsError::new(422, "invalid", "runner_id is required"))?;
                runner::forget(&mut inner.runner, &cx, &id).await?
            }
            _ => runner::reconnect(&mut inner.runner, &cx, choice).await?,
        }
        Ok(self.runner_status(&inner, Some(&h)))
    }

    async fn status(&self, session: Option<&BoundSession>) -> Result<Value, ToolsError> {
        let mut inner = self.inner.lock().await;
        Self::rebind(&mut inner, session).await;
        let Some(h) = session.filter(|s| s.is_hosted()) else {
            if session.is_some() {
                let _ = self.root_or_problem(&mut inner);
            }
            return Ok(json!({
                "available": false,
                "reason": NOT_LOCAL,
                "spaces": [],
                "local_runner": self.runner_status(&inner, session),
            }));
        };
        let root = match self.root_or_problem(&mut inner) {
            Ok(r) => r,
            Err(_) => {
                return Ok(json!({
                    "available": true,
                    "reason": null,
                    "problem": inner.problem,
                    "spaces": [],
                    "local_runner": self.runner_status(&inner, Some(h)),
                }));
            }
        };
        let records = {
            let _g = self.store_lock.lock();
            store::list(&root, &h.account_id).map_err(|_| fs_error("read its settings"))?
        };
        let spaces: Vec<Value> = records
            .iter()
            .map(|p| self.entry(&inner, &h.account_id, &p.space_id, Some(p)))
            .collect();
        let local_runner = self.runner_status(&inner, Some(h));
        Ok(json!({
            "available": true,
            "reason": null,
            "problem": inner.problem,
            "spaces": spaces,
            "local_runner": local_runner,
        }))
    }

    /// The session must still be this account on its server.
    async fn check_account(api: &Hosted, h: &BoundSession) -> Result<(), ToolsError> {
        let id = api.me(&h.token).await.map_err(from_call)?;
        if id != h.account_id {
            return Err(ToolsError::new(
                422,
                "session_rejected",
                "The local Metro server reports a different account than this session.",
            ));
        }
        Ok(())
    }

    fn spawn_worker(
        &self,
        inner: &mut Inner,
        api: &Hosted,
        root: &Arc<File>,
        p: &Pairing,
    ) -> Result<(), ToolsError> {
        let spec = worker::Spec {
            hosted: api.clone(),
            root: root.clone(),
            store_lock: self.store_lock.clone(),
            account_id: p.account_id.clone(),
            space_id: p.space_id.clone(),
            etag: p.etag.clone(),
            token: p.token.clone(),
        };
        inner
            .workers
            .insert(p.space_id.clone(), worker::spawn(spec));
        Ok(())
    }

    async fn enable(&self, h: &BoundSession, sid: &str) -> Result<Value, ToolsError> {
        let mut inner = self.inner.lock().await;
        Self::rebind(&mut inner, Some(h)).await;
        let root = self.root(&mut inner)?;
        let api = Hosted::new(&h.server_url).map_err(|_| fs_error("start"))?;
        Self::check_account(&api, h).await?;
        match api.role(&h.token, sid).await.map_err(from_call)? {
            None => return Err(ToolsError::new(404, "not_found", "no such Space")),
            Some(hosted::SpaceRole::Member) => {
                return Err(ToolsError::new(
                    403,
                    "not_owner",
                    "Only the Space owner can turn on local tools.",
                ));
            }
            Some(hosted::SpaceRole::Owner) => {}
        }
        let existing = {
            let _g = self.store_lock.lock();
            store::load(&root, &h.account_id, sid).map_err(|_| fs_error("read its settings"))?
        };
        if let Some(p) = existing
            .as_ref()
            .filter(|p| p.state == PairingState::Active)
        {
            let running = inner
                .workers
                .get(sid)
                .map(|w| {
                    w.etag == p.etag && !w.is_finished() && w.status().state != WorkerState::Revoked
                })
                .unwrap_or(false);
            if !running {
                if let Some(old) = inner.workers.remove(sid) {
                    old.stop().await;
                }
                self.spawn_worker(&mut inner, &api, &root, p)?;
            }
            return Ok(self.entry(&inner, &h.account_id, sid, Some(p)));
        }
        // No live pairing of ours: pair only if the Space has no runner at
        // all, decided atomically by the server.
        let (token, etag) = match api.issue_guarded(&h.token, sid).await {
            Ok(t) => t,
            Err(CallError::Precondition(_)) => {
                let msg = match api.space_runner(&h.token, sid).await {
                    Ok(SpaceRunner::Dedicated) => {
                        "This Space already has a dedicated runner token. Metro won't replace \
                         it: revoke it in this Space's runner settings first, then turn on \
                         local tools."
                            .to_string()
                    }
                    Ok(SpaceRunner::Registered(name)) => format!(
                        "This Space uses the runner \"{name}\". Metro won't replace it: detach \
                         it first, then turn on local tools."
                    ),
                    _ => "This Space's runner changed while turning on local tools; nothing \
                          was replaced. Try again."
                        .to_string(),
                };
                return Err(ToolsError::new(409, "runner_present", msg));
            }
            Err(e) => return Err(from_call(e)),
        };
        let pairing = Pairing::new(&h.account_id, sid, etag, token);
        let saved = {
            let _g = self.store_lock.lock();
            store::workspace(&root, &h.account_id, sid, true)
                .and_then(|_| store::save(&root, &pairing))
        };
        if saved.is_err() {
            // Don't leave a token on the server that nobody holds.
            let _ = api.revoke_guarded(&h.token, sid, &pairing.etag).await;
            return Err(fs_error("save this Space's pairing"));
        }
        if let Some(old) = inner.workers.remove(sid) {
            old.stop().await;
        }
        self.spawn_worker(&mut inner, &api, &root, &pairing)?;
        Ok(self.entry(&inner, &h.account_id, sid, Some(&pairing)))
    }

    async fn disable(
        &self,
        h: &BoundSession,
        sid: &str,
        expected: Option<&str>,
    ) -> Result<Value, ToolsError> {
        let mut inner = self.inner.lock().await;
        if expected.is_some() {
            // Do not rebind or stop anybody else's workers for a stale UI.
            if inner.bound.as_ref() != Some(&(h.server_url.clone(), h.account_id.clone())) {
                return Err(pairing_changed());
            }
        } else {
            Self::rebind(&mut inner, Some(h)).await;
        }
        let root = self.root(&mut inner)?;
        let existing = {
            let _g = self.store_lock.lock();
            store::load(&root, &h.account_id, sid).map_err(|_| fs_error("read its settings"))?
        };
        let Some(p) = existing else {
            if expected.is_some() {
                return Err(pairing_changed());
            }
            return Ok(self.entry(&inner, &h.account_id, sid, None));
        };
        if let Some(revision) = expected
            && (hosted::pairing_revision(&p.etag) != Some(revision)
                || inner.workers.get(sid).is_some_and(|w| w.etag != p.etag))
        {
            return Err(pairing_changed());
        }
        let mut detail: Option<&str> = None;
        if p.state == PairingState::Active {
            let api = Hosted::new(&h.server_url).map_err(|_| fs_error("start"))?;
            Self::check_account(&api, h).await?;
            match api.revoke_guarded(&h.token, sid, &p.etag).await {
                Ok(()) => {}
                Err(CallError::Precondition(_)) => {
                    detail = Some(
                        "This Space's runner had already changed, so nothing was revoked on the \
                         server; this computer stopped serving it.",
                    );
                }
                Err(e @ CallError::NotFound(_)) => {
                    // A missing route on an older server is not loss of
                    // ownership. Keep the pairing and worker in that case.
                    match api.role(&h.token, sid).await.map_err(from_call)? {
                        Some(hosted::SpaceRole::Owner) => return Err(from_call(e)),
                        Some(hosted::SpaceRole::Member) | None => {}
                    }
                    detail = Some(
                        "You no longer own this Space, so nothing was revoked on the server; \
                         this computer stopped serving it.",
                    );
                }
                Err(CallError::Forbidden(_)) => {
                    detail = Some(
                        "You no longer own this Space, so nothing was revoked on the server; \
                         this computer stopped serving it.",
                    );
                }
                Err(e) => {
                    // No confirmed server change: keep both pairing and worker.
                    return Err(from_call(e));
                }
            }
        }
        if let Some(w) = inner.workers.remove(sid) {
            w.stop().await;
        }
        {
            let _g = self.store_lock.lock();
            store::remove(&root, &h.account_id, sid).map_err(|_| fs_error("remove the pairing"))?;
        }
        let mut v = self.entry(&inner, &h.account_id, sid, None);
        if let Some(d) = detail {
            v["detail"] = json!(d);
        }
        Ok(v)
    }

    /// Start the workers of this account's active pairings. Call after the
    /// bridge bound the desktop's own server. Nothing is issued here.
    pub async fn resume(&self, session: BoundSession) {
        let mut inner = self.inner.lock().await;
        Self::rebind(&mut inner, Some(&session)).await;
        let problem = self.start(&mut inner, &session).await;
        if let Some(why) = &problem {
            eprintln!(
                "metro-desktop: local tools or this computer's runner didn't fully start: {why}"
            );
            runner::start_failed(&mut inner.runner, why);
        }
        // Still only the folder (set by `root_or_problem`), or something else.
        inner.problem_is_root &= problem.is_some();
        if problem.as_ref() != inner.problem.as_ref() {
            inner.problem_is_root = false;
        }
        inner.problem = problem;
    }

    /// Start every active pairing's worker (own hosted server only) and
    /// this computer's runner. `Some(reason)` when something can't be
    /// served; nothing here stays silent, and the next session bind tries
    /// again.
    async fn start(&self, inner: &mut Inner, session: &BoundSession) -> Option<String> {
        let root = match self.root_or_problem(inner) {
            Ok(r) => r,
            Err(why) => return Some(why),
        };
        let Ok(api) = Hosted::new(&session.server_url) else {
            return Some("Metro couldn't set up its connection to the Metro server.".into());
        };
        if let Err(e) = Self::check_account(&api, session).await {
            for (_, w) in inner.workers.drain() {
                w.stop().await;
            }
            inner.runner.stop().await;
            return Some(format!(
                "This computer's runner didn't start: {}",
                e.message
            ));
        }
        let unreadable = if session.is_hosted() {
            match self.start_pairings(inner, &api, &root, session) {
                Ok(u) => u,
                Err(why) => return Some(why),
            }
        } else {
            Vec::new()
        };
        // This computer's personal runner: resumed once set up (or, with the
        // installer's marker on the own server, created the first time);
        // never recreated on its own.
        let cx = runner::Ctx {
            marker: &self.marker,
            root: &root,
            root_path: &self.root_path,
            session,
            api: &api,
            store_lock: &self.store_lock,
        };
        runner::ensure(&mut inner.runner, &cx).await;
        (!unreadable.is_empty()).then(|| {
            format!(
                "Metro couldn't read the local tools pairing of {} on this computer, so it isn't \
                 served. Turn local tools off and on again there.",
                if unreadable.len() == 1 {
                    format!("Space {}", unreadable[0])
                } else {
                    format!("{} Spaces ({})", unreadable.len(), unreadable.join(", "))
                }
            )
        })
    }

    /// Start the active per-Space pairings; the Spaces whose pairing can't
    /// be read.
    fn start_pairings(
        &self,
        inner: &mut Inner,
        api: &Hosted,
        root: &Arc<File>,
        hosted: &BoundSession,
    ) -> Result<Vec<String>, String> {
        let listed = {
            let _g = self.store_lock.lock();
            store::list_all(root, &hosted.account_id)
        };
        let (records, unreadable) = match listed {
            Ok(l) => l,
            Err(e) => {
                return Err(crate::secret::clean_line(
                    &format!(
                        "Metro couldn't read its local tools settings in {}: {e}",
                        self.root_path.display()
                    ),
                    300,
                ));
            }
        };
        for p in records.iter().filter(|p| p.state == PairingState::Active) {
            let running = inner
                .workers
                .get(&p.space_id)
                .map(|w| w.etag == p.etag && !w.is_finished())
                .unwrap_or(false);
            if !running {
                let _ = self.spawn_worker(inner, api, root, p);
            }
        }
        Ok(unreadable)
    }

    /// Stop every worker (sign-out, server or account switch, app exit).
    /// Pairings stay; [`DesktopTools::resume`] restarts them next time.
    pub async fn stop_all(&self) {
        let mut inner = self.inner.lock().await;
        for (_, w) in inner.workers.drain() {
            w.stop().await;
        }
        inner.runner.stop().await;
        inner.bound = None;
    }

    /// The id of the personal runner this computer serves right now.
    pub async fn local_runner_id(&self) -> Option<String> {
        let inner = self.inner.lock().await;
        runner::running_id(&inner.runner).map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_session_requires_the_local_server_exactly() {
        let tok = Secret::new("local-bearer-1");
        let url = "http://127.0.0.1:41234";
        assert!(BoundSession::verify(Some(url), Some(&tok), url, &tok, "alice").is_some());
        // No running local server, or a different URL or bearer: refused.
        assert!(BoundSession::verify(None, Some(&tok), url, &tok, "alice").is_none());
        assert!(BoundSession::verify(Some(url), None, url, &tok, "alice").is_none());
        assert!(
            BoundSession::verify(
                Some(url),
                Some(&tok),
                "http://127.0.0.1:41235",
                &tok,
                "alice"
            )
            .is_none()
        );
        let other = Secret::new("local-bearer-2");
        assert!(BoundSession::verify(Some(url), Some(&tok), url, &other, "alice").is_none());
        // Only plain loopback http with a port.
        for bad in [
            "https://127.0.0.1:41234",
            "http://localhost:41234",
            "http://127.0.0.2:41234",
            "http://[::1]:41234",
            "http://127.0.0.1:41234/",
            "http://127.0.0.1:41234/x",
            "http://metro.example.com",
            "http://127.0.0.1",
        ] {
            assert!(
                BoundSession::verify(Some(bad), Some(&tok), bad, &tok, "alice").is_none(),
                "{bad}"
            );
        }
        let empty = Secret::new("");
        assert!(BoundSession::verify(Some(url), Some(&empty), url, &empty, "alice").is_none());
        assert!(BoundSession::verify(Some(url), Some(&tok), url, &tok, "").is_none());
        assert!(
            !format!(
                "{:?}",
                BoundSession::verify(Some(url), Some(&tok), url, &tok, "a")
            )
            .contains("local-bearer")
        );
    }

    #[tokio::test]
    async fn without_the_local_server_nothing_is_offered() {
        let t = tempfile::tempdir().unwrap();
        let tools = DesktopTools::new(t.path().join("local-tools"));
        let (s, v) = tools.handle("GET", "/v1/local-tools", b"", None).await;
        assert_eq!(s, 200);
        assert_eq!(v["available"], false);
        let (s, v) = tools
            .handle("POST", "/v1/local-tools/spaces/s-a/enable", b"{}", None)
            .await;
        assert_eq!(s, 409);
        assert_eq!(v["error"]["code"], "not_local");
        let (s, _) = tools
            .handle("POST", "/v1/local-tools/spaces/s-a/run", b"{}", None)
            .await;
        assert_eq!(s, 404);
        let (s, _) = tools
            .handle("POST", "/v1/local-tools/spaces/..%2f/enable", b"{}", None)
            .await;
        assert_eq!(s, 404);
        let (s, _) = tools
            .handle("GET", "/v1/local-tools/spaces/s-a/enable", b"", None)
            .await;
        assert_eq!(s, 405);
        let (s, _) = tools
            .handle(
                "POST",
                "/v1/local-tools/spaces/s-a/enable",
                b"{\"x\":1}",
                None,
            )
            .await;
        assert_eq!(s, 422);
    }
}
