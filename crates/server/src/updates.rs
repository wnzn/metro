//! Release checks, runner presence and runner update requests.
//!
//! # Server release check (operator notice)
//!
//! Once a day a release build of the server (`METRO_RELEASE_CHANNEL=release`
//! at compile time) fetches the latest published `SHA256SUMS`, verifies it
//! with [`metro_release`] (Sigstore bundle against the pinned release
//! workflow identity), and keeps the verified release and its `IMAGES.txt`
//! in memory. `METRO_UPDATE_CHECK=off` turns it off; the desktop app's
//! bundled local server always runs with it off. Source and preview builds
//! never check. The result is an operator notice only: a log line, the
//! admin page, and `GET /v1/server/update` for org admins (404 for anyone
//! else). Nothing is ever downloaded or installed by the server.
//!
//! # Runner presence
//!
//! Runners report `{"runner": {version, channel, install, self_update,
//! update}}` in their claim body. It's untrusted display data kept in
//! memory: it never grants anything, an invalid body never fails a claim,
//! and it refreshes within one poll after a server restart.
//!
//! # Runner update requests
//!
//! A runner's manager (personal runner: its owner; organization runner: org
//! admins; dedicated runner: the Space owner) can ask a self-updating runner
//! to update to the verified latest version. The only thing the server ever
//! sends is the response header `X-Metro-Runner-Update: X.Y.Z` on that
//! runner's claims: no URL, no hash, no code. The runner fetches and
//! verifies the release itself.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use metro_release::fetch::{AsyncFetcher, CheckOutcome, Source};
use metro_release::images::Images;
use metro_release::{Channel, ReleaseVerifier, Version};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::api::{self, ApiError, ApiResult, JsonBody};
use crate::auth::{Human, Runner, RunnerCredential};
use crate::db::now_ms;
use crate::monitoring::{Code, Scope};
use crate::runners::{RegisteredRunner, SpaceRunner, Visibility};

/// The one header a runner is ever sent about updates.
pub const UPDATE_HEADER: &str = "x-metro-runner-update";
/// A runner counts as online when it claimed within this long.
pub const ONLINE_WINDOW_MS: i64 = 2 * 60 * 1000;
/// A pending request expires after this long.
pub const PENDING_TTL_MS: i64 = 60 * 60 * 1000;
/// Largest claim body parsed for presence; bigger ones are ignored.
pub const MAX_PRESENCE_BODY: usize = 2 * 1024;
const MAX_TRACKED: usize = 10_000;

const CHECK_FIRST_DELAY: Duration = Duration::from_secs(120);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// `METRO_UPDATE_CHECK`: `on` (default) or `off`.
pub fn check_enabled_from_env() -> bool {
    parse_check_setting(std::env::var("METRO_UPDATE_CHECK").ok().as_deref())
}

/// `METRO_UPDATE_CHECK`: `off`, `0`, `false` or `no` (any case, spaces
/// ignored) turn the check off; unset or anything else leaves it on.
pub fn parse_check_setting(value: Option<&str>) -> bool {
    match value {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        ),
        None => true,
    }
}

/// What a runner says about itself (bounded, validated, untrusted).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Presence {
    pub seen_at: i64,
    pub version: Option<String>,
    pub channel: Option<&'static str>,
    pub install: Option<&'static str>,
    pub self_update: bool,
    pub update: Option<ReportedUpdate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReportedUpdate {
    pub state: &'static str,
    pub target: Option<String>,
    pub code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Pending {
    pub target: String,
    pub requested_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LastResult {
    pub target: String,
    pub state: &'static str,
    pub code: Option<String>,
    pub at: i64,
}

/// The server's verified view of the latest release.
#[derive(Clone, Debug, Serialize)]
pub struct Latest {
    pub version: Version,
    pub commit: String,
    pub signed_at: i64,
    pub images: Images,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CheckStatus {
    pub checked_at: Option<i64>,
    /// `never`, `ok` or `error`.
    pub result: &'static str,
    pub error: Option<&'static str>,
    /// A verified release older than one verified before was served as
    /// latest (the newer one stays offered).
    pub anomaly: Option<&'static str>,
}

#[derive(Default)]
struct Inner {
    floor: Option<Version>,
    floor_unreadable: bool,
    latest: Option<Latest>,
    status: CheckStatus,
    presence: HashMap<String, Presence>,
    pending: HashMap<String, Pending>,
    last_result: HashMap<String, LastResult>,
}

/// Shared update state (in memory only; no schema change).
#[derive(Clone)]
pub struct Updates {
    pub check_enabled: bool,
    inner: Arc<Mutex<Inner>>,
    /// Where the highest version ever verified is kept across restarts.
    floor_file: Option<std::path::PathBuf>,
}

impl Updates {
    pub fn new(check_enabled: bool) -> Updates {
        let inner = Inner {
            status: CheckStatus {
                result: "never",
                ..Default::default()
            },
            ..Default::default()
        };
        Updates {
            check_enabled,
            inner: Arc::new(Mutex::new(inner)),
            floor_file: None,
        }
    }

    /// Persist the version floor in `path`. An unreadable floor blocks
    /// publication rather than silently starting over at zero.
    pub fn with_floor_file(mut self, path: std::path::PathBuf) -> Updates {
        {
            let mut inner = self.lock();
            match read_floor(&path) {
                Ok(floor) => inner.floor = floor,
                Err(error) => {
                    tracing::warn!(%error, "could not read the stored release floor; checks blocked");
                    inner.floor_unreadable = true;
                }
            }
        }
        self.floor_file = Some(path);
        self
    }

    /// The highest version verified so far (persisted when configured).
    pub fn floor(&self) -> Option<Version> {
        let inner = self.lock();
        match (inner.floor, inner.latest.as_ref().map(|l| l.version)) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// Returns false for a backwards check, without changing any offer.
    fn raise_floor(&self, v: Version) -> Result<bool, metro_release::Error> {
        let error = || {
            metro_release::Error::new(
                metro_release::ErrorCode::Io,
                "could not read or persist the release floor",
            )
        };
        let mut inner = self.lock();
        if inner.floor_unreadable {
            return Err(error());
        }
        if inner
            .floor
            .into_iter()
            .chain(inner.latest.as_ref().map(|l| l.version))
            .any(|f| f > v)
        {
            return Ok(false);
        }
        if inner.floor != Some(v) {
            if let Some(path) = &self.floor_file {
                metro_release::write_atomic(path, v.to_string().as_bytes()).map_err(|_| error())?;
            }
            inner.floor = Some(v);
        }
        Ok(true)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn latest(&self) -> Option<Latest> {
        self.lock().latest.clone()
    }

    /// For tests and the checker: set the verified latest release.
    pub fn set_latest(&self, latest: Latest) {
        let mut inner = self.lock();
        // Never go backwards: a replayed older manifest can't hide a newer one.
        if inner
            .latest
            .as_ref()
            .is_some_and(|l| l.version > latest.version)
        {
            tracing::warn!(seen = %latest.version, "an older release was served as latest; ignored");
            return;
        }
        inner.latest = Some(latest);
    }

    pub fn presence(&self, key: &str) -> Option<Presence> {
        self.lock().presence.get(key).cloned()
    }

    fn pending(&self, key: &str, now: i64) -> Option<Pending> {
        let mut inner = self.lock();
        if inner
            .pending
            .get(key)
            .is_some_and(|p| now - p.requested_at > PENDING_TTL_MS)
        {
            inner.pending.remove(key);
        }
        inner.pending.get(key).cloned()
    }
}

fn read_floor(path: &std::path::Path) -> std::io::Result<Option<Version>> {
    use std::io::{self, Read};
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid release floor");
    if !file.metadata()?.is_file() {
        return Err(invalid());
    }
    let mut value = String::new();
    file.take(65).read_to_string(&mut value)?;
    if value.len() > 64 {
        return Err(invalid());
    }
    Version::parse(value.trim()).map(Some).ok_or_else(invalid)
}

/// The presence key of a runner credential (a dedicated runner is its Space).
pub fn runner_key(runner: &Runner) -> String {
    match &runner.credential {
        RunnerCredential::Registered { runner_id } => format!("runner:{runner_id}"),
        RunnerCredential::Dedicated => {
            format!(
                "space:{}",
                runner
                    .spaces
                    .first()
                    .map(|s| s.0.as_str())
                    .unwrap_or_default()
            )
        }
    }
}

fn enum_value(v: Option<&serde_json::Value>, allowed: &[&'static str]) -> Option<&'static str> {
    let s = v?.as_str()?;
    allowed.iter().copied().find(|a| *a == s)
}

fn semver(v: Option<&serde_json::Value>) -> Option<String> {
    let s = v?.as_str()?;
    (s.len() <= 64)
        .then(|| Version::parse(s))
        .flatten()
        .map(|v| v.to_string())
}

fn code_value(v: Option<&serde_json::Value>) -> Option<String> {
    let s = v?.as_str()?;
    (!s.is_empty() && s.len() <= 40 && s.bytes().all(|c| c.is_ascii_lowercase() || c == b'_'))
        .then(|| s.to_string())
}

/// Parse a claim body leniently. Anything unexpected → only `seen_at`.
pub fn parse_presence(body: &[u8], now: i64) -> Presence {
    let mut p = Presence {
        seen_at: now,
        ..Default::default()
    };
    if body.len() > MAX_PRESENCE_BODY {
        return p;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return p;
    };
    let Some(r) = value.get("runner").and_then(|r| r.as_object()) else {
        return p;
    };
    p.version = semver(r.get("version"));
    p.channel = enum_value(r.get("channel"), &["release", "preview", "source"]);
    p.install = enum_value(
        r.get("install"),
        &["managed", "unmanaged", "container", "desktop"],
    );
    p.self_update = r
        .get("self_update")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && p.install == Some("managed")
        && p.channel == Some("release");
    p.update = r.get("update").and_then(|u| u.as_object()).and_then(|u| {
        Some(ReportedUpdate {
            state: enum_value(u.get("state"), &["idle", "updating", "failed"])?,
            target: semver(u.get("target")),
            code: code_value(u.get("code")),
        })
    });
    p
}

/// Called on every claim: record presence, settle a pending request, and
/// return the version to put in [`UPDATE_HEADER`], if any.
pub async fn on_claim(state: &AppState, runner: &Runner, body: &[u8]) -> Option<String> {
    let key = runner_key(runner);
    let now = now_ms();
    let presence = parse_presence(body, now);
    let settled = {
        let updates = &state.updates;
        let mut inner = updates.lock();
        if inner.presence.len() >= MAX_TRACKED && !inner.presence.contains_key(&key) {
            inner
                .presence
                .retain(|_, p| now - p.seen_at < ONLINE_WINDOW_MS);
        }
        inner.presence.insert(key.clone(), presence.clone());
        let mut settled = None;
        if let Some(pending) = inner.pending.get(&key).cloned() {
            if now - pending.requested_at > PENDING_TTL_MS {
                inner.pending.remove(&key);
            } else if presence.version.as_deref() == Some(pending.target.as_str()) {
                inner.pending.remove(&key);
                let r = LastResult {
                    target: pending.target.clone(),
                    state: "updated",
                    code: None,
                    at: now,
                };
                inner.last_result.insert(key.clone(), r);
                settled = Some(Code::RunnerUpdated);
            } else if let Some(u) = &presence.update
                && u.state == "failed"
                && u.target.as_deref() == Some(pending.target.as_str())
            {
                inner.pending.remove(&key);
                let r = LastResult {
                    target: pending.target.clone(),
                    state: "failed",
                    code: u.code.clone(),
                    at: now,
                };
                inner.last_result.insert(key.clone(), r);
                settled = Some(Code::RunnerUpdateFailed);
            }
        }
        settled
    };
    if let Some(code) = settled {
        tracing::info!(runner = %key, version = ?presence.version, outcome = code.code(), "runner update settled");
        if let Some(scope) = monitoring_scope(state, runner).await {
            state.monitor(scope, code).await;
        }
    }
    state.updates.pending(&key, now).map(|p| p.target)
}

/// Where a runner's update events are readable: a dedicated runner's Space
/// owner, a personal runner's owner, org admins for an organization runner.
async fn monitoring_scope(state: &AppState, runner: &Runner) -> Option<Scope> {
    match &runner.credential {
        RunnerCredential::Dedicated => runner.spaces.first().map(|s| Scope::Space(s.0.clone())),
        RunnerCredential::Registered { runner_id } => {
            let id = runner_id.clone();
            let r = state.db.call(move |db| db.get_runner(&id)).await.ok()??;
            Some(scope_of(&r))
        }
    }
}

fn scope_of(r: &RegisteredRunner) -> Scope {
    match r.visibility {
        Visibility::Personal => Scope::User(r.owner_id.clone()),
        Visibility::Organization => Scope::Instance,
    }
}

// ---- views ----

/// `presence` and `update` objects for a runner view.
pub fn runner_fields(state: &AppState, key: &str) -> (serde_json::Value, serde_json::Value) {
    let now = now_ms();
    let p = state.updates.presence(key);
    let pending = state.updates.pending(key, now);
    let last = state.updates.lock().last_result.get(key).cloned();
    let latest = state.updates.latest();
    let presence = json!({
        "seen_at": p.as_ref().map(|p| p.seen_at),
        "online": p.as_ref().is_some_and(|p| now - p.seen_at < ONLINE_WINDOW_MS),
        "version": p.as_ref().and_then(|p| p.version.clone()),
        "install": p.as_ref().and_then(|p| p.install),
        "self_update": p.as_ref().is_some_and(|p| p.self_update),
    });
    let running = p
        .as_ref()
        .and_then(|p| p.version.as_deref())
        .and_then(Version::parse);
    let available = matches!((&latest, running), (Some(l), Some(r)) if l.version > r);
    let action = match p.as_ref() {
        _ if !available => "none",
        Some(p) if p.self_update => "self_update",
        Some(p) => match p.install {
            Some("container") => "container",
            Some("desktop") => "with_app",
            Some(_) => "install_script",
            None => "none",
        },
        None => "none",
    };
    let update = json!({
        "latest": latest.as_ref().map(|l| l.version.to_string()),
        "available": available,
        "action": action,
        "container_image": if action == "container" { latest.as_ref().map(|l| l.images.runner.clone()) } else { None },
        "pending": pending,
        "last_result": last,
    });
    (presence, update)
}

// ---- HTTP ----

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/v1/server/update", get(server_update))
        .route("/v1/runners/{id}/update", post(request_registered_update))
        .route(
            "/v1/spaces/{sid}/runner/update",
            post(request_dedicated_update),
        )
}

/// `GET /v1/server/update`: org admins only; 404 for anyone else.
pub async fn server_update(
    State(state): State<AppState>,
    who: Human,
) -> ApiResult<Json<serde_json::Value>> {
    if !state.config.is_org_admin(&who.id) {
        return Err(ApiError::not_found("not found"));
    }
    let current = env!("CARGO_PKG_VERSION");
    let channel = channel();
    let latest = state.updates.latest();
    let status = state.updates.lock().status.clone();
    let running = Version::parse(current);
    let available = matches!((&latest, running), (Some(l), Some(r)) if channel == Channel::Release && l.version > r);
    Ok(Json(json!({
        "current": current,
        "channel": channel.as_str(),
        "check_enabled": state.updates.check_enabled && channel == Channel::Release,
        "checked_at": status.checked_at,
        "result": status.result,
        "error": status.error,
        "anomaly": status.anomaly,
        "available": available,
        "latest": latest.as_ref().map(|l| json!({
            "version": l.version.to_string(),
            "commit": l.commit,
            "signed_at": l.signed_at,
            "release_url": metro_release::release_url(&l.version),
            "images": {"server": l.images.server, "runner": l.images.runner},
        })),
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest {
    version: String,
}

fn refuse(code: &str, message: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"error": message, "code": code})),
    )
        .into_response()
}

/// Checks shared by both kinds; on success, records the pending request.
async fn request_update(state: &AppState, key: &str, version: &str, scope: Scope) -> Response {
    let now = now_ms();
    let Some(target) = Version::parse(version) else {
        return ApiError::bad_request("version must be X.Y.Z").into_response();
    };
    let Some(latest) = state.updates.latest() else {
        return refuse(
            "no_verified_release",
            "this server hasn't verified a newer release",
        );
    };
    let result = {
        let mut inner = state.updates.lock();
        let p = inner.presence.get(key).cloned();
        let pending = inner
            .pending
            .get(key)
            .filter(|p| now - p.requested_at <= PENDING_TTL_MS)
            .cloned();
        let running = p
            .as_ref()
            .and_then(|p| p.version.as_deref())
            .and_then(Version::parse);
        if target != latest.version {
            Err((
                "not_latest",
                "only the latest verified release can be requested",
            ))
        } else if !p
            .as_ref()
            .is_some_and(|p| now - p.seen_at < ONLINE_WINDOW_MS)
        {
            Err((
                "runner_offline",
                "the runner hasn't been seen in the last 2 minutes",
            ))
        } else if !p.as_ref().is_some_and(|p| p.self_update) {
            Err((
                "self_update_off",
                "this runner doesn't update itself; update it on its host",
            ))
        } else if !running.is_some_and(|r| target > r) {
            Err((
                "not_newer",
                "the runner already runs this version or a newer one",
            ))
        } else if pending.is_some() {
            Err(("already_pending", "an update was already requested"))
        } else {
            let pending = Pending {
                target: target.to_string(),
                requested_at: now,
            };
            inner.pending.insert(key.to_owned(), pending.clone());
            Ok((pending, running))
        }
    };
    match result {
        Err((code, message)) => refuse(code, message),
        Ok((pending, running)) => {
            tracing::info!(runner = %key, from = ?running.map(|r| r.to_string()), to = %pending.target, "runner update requested");
            state.monitor(scope, Code::RunnerUpdateRequested).await;
            api::no_store(StatusCode::ACCEPTED, json!({"pending": pending}))
        }
    }
}

/// `POST /v1/runners/{id}/update`: the owner of a personal runner, org
/// admins for an organization runner (404 if invisible, 403 otherwise).
pub async fn request_registered_update(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: JsonBody<UpdateRequest>,
) -> ApiResult<Response> {
    let runner = crate::runners::managed_runner(&state, &id, &who).await?;
    let Json(body) = body?;
    let key = format!("runner:{}", runner.id);
    Ok(request_update(&state, &key, &body.version, scope_of(&runner)).await)
}

/// `POST /v1/spaces/{sid}/runner/update`: a dedicated runner; the Space
/// owner only (404 outsiders, 403 members).
pub async fn request_dedicated_update(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<UpdateRequest>,
) -> ApiResult<Response> {
    let space = api::owner_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    let space_id = space.id.clone();
    match api::db(&state, move |db| db.space_runner(&space_id)).await? {
        SpaceRunner::Dedicated => {}
        SpaceRunner::Registered(_) => {
            return Ok(refuse(
                "registered_runner",
                "this Space uses a registered runner: POST /v1/runners/{id}/update",
            ));
        }
        SpaceRunner::None => return Err(ApiError::not_found("this Space has no runner")),
    }
    let key = format!("space:{}", space.id);
    Ok(request_update(&state, &key, &body.version, Scope::Space(space.id.clone())).await)
}

/// Claim handler helper: the claim body, bounded.
pub fn claim_body(body: &Bytes) -> &[u8] {
    if body.len() > MAX_PRESENCE_BODY {
        &[]
    } else {
        body
    }
}

// ---- the daily check ----

/// One verified check (also used by tests with a fixture source and a test
/// verifier). Verifies the latest release regardless of this server's own
/// version: runners need it even when the server is up to date.
pub async fn check_once(state: &AppState, fetcher: &AsyncFetcher, verifier: &dyn ReleaseVerifier) {
    let zero = Version {
        major: 0,
        minor: 0,
        patch: 0,
    };
    let outcome = async {
        let release = match fetcher.check(verifier, &zero).await? {
            CheckOutcome::Newer(r) => r,
            CheckOutcome::NotNewer { .. } => {
                return Err(metro_release::Error::new(
                    metro_release::ErrorCode::Manifest,
                    "no release",
                ));
            }
        };
        // The signed manifest establishes the floor, even if an optional
        // image manifest cannot be fetched. Only a complete check can offer it.
        if !state.updates.raise_floor(release.version)? {
            return Ok((release.version, None));
        }
        let raw = fetcher
            .get(
                &fetcher.source.asset_url(&release.version, "IMAGES.txt")?,
                metro_release::MAX_IMAGES,
            )
            .await?;
        release.check_bytes("IMAGES.txt", &raw)?;
        let images = metro_release::images::parse(&raw, &release.version)?;
        if images.commit != release.commit {
            return Err(metro_release::Error::new(
                metro_release::ErrorCode::Inconsistent,
                "IMAGES.txt names another commit than the signed release",
            ));
        }
        Ok((
            release.version,
            Some(Latest {
                version: release.version,
                commit: release.commit,
                signed_at: release.signed_at,
                images,
            }),
        ))
    }
    .await;
    let now = now_ms();
    match outcome {
        Ok((version, candidate)) => {
            let notify = {
                let mut inner = state.updates.lock();
                let latest = candidate.filter(|l| {
                    inner.floor.is_none_or(|f| l.version >= f)
                        && inner
                            .latest
                            .as_ref()
                            .is_none_or(|old| l.version >= old.version)
                });
                let mut anomaly = None;
                let notify = if let Some(latest) = latest {
                    let changed = inner.latest.as_ref().map(|l| l.version) != Some(version);
                    inner.latest = Some(latest);
                    changed
                        && Version::parse(env!("CARGO_PKG_VERSION")).is_some_and(|r| version > r)
                } else {
                    tracing::warn!(seen = %version, "an older release was served as latest; ignored");
                    anomaly = Some("older_release_served");
                    false
                };
                inner.status = CheckStatus {
                    checked_at: Some(now),
                    result: "ok",
                    error: None,
                    anomaly,
                };
                notify
            };
            if notify {
                tracing::warn!(
                    "Metro {} is available; this server runs {} ({})",
                    version,
                    env!("CARGO_PKG_VERSION"),
                    metro_release::release_url(&version)
                );
                state
                    .monitor(Scope::Instance, Code::ServerUpdateAvailable)
                    .await;
            }
        }
        Err(e) => {
            tracing::info!(
                error = e.code(),
                "release check failed; will retry at the next daily check"
            );
            state.updates.lock().status = CheckStatus {
                checked_at: Some(now),
                result: "error",
                error: Some(e.code()),
                anomaly: None,
            };
        }
    }
}

/// This build's channel (an e2e test build with the test release source
/// active counts as a release; see `metro_release::e2e`).
fn channel() -> Channel {
    #[cfg(feature = "e2e-test-release")]
    if metro_release::e2e::active() {
        return Channel::Release;
    }
    Channel::current()
}

/// An e2e test build's fixture source, verifier and check interval.
#[cfg(feature = "e2e-test-release")]
fn test_release() -> Option<(Source, Arc<dyn ReleaseVerifier>, Duration)> {
    let (source, verifier) = metro_release::e2e::release_source()?;
    let secs = std::env::var("METRO_TEST_RELEASE_CHECK_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    Some((source, Arc::from(verifier), Duration::from_secs(secs)))
}

/// Start the daily check (release builds with the check on only).
pub fn spawn(state: AppState) {
    if !state.updates.check_enabled {
        tracing::info!("release check is off (METRO_UPDATE_CHECK=off)");
        return;
    }
    if channel() != Channel::Release {
        tracing::info!("release check is off: this is not a published release build");
        return;
    }
    #[cfg(feature = "e2e-test-release")]
    if let Some((source, verifier, every)) = test_release() {
        if let Ok(fetcher) = AsyncFetcher::new(source, Duration::from_secs(30)) {
            spawn_checks(state, fetcher, verifier, None, Duration::ZERO, every);
        }
        return;
    }
    // The trust cache lives next to the database (private folder).
    let trust_dir = state
        .config
        .db_path
        .parent()
        .map(|d| {
            if d.as_os_str().is_empty() {
                std::path::Path::new(".")
            } else {
                d
            }
        })
        .map(|d| d.join("sigstore-trust"));
    let trust = trust_dir
        .and_then(|d| metro_release::trust_refresh::TrustRefresh::new(&d).ok())
        .map(Arc::new);
    let verifier: Arc<dyn ReleaseVerifier> = match &trust {
        Some(t) => t.verifier.clone(),
        None => match metro_release::sigstore::SigstoreVerifier::vendored() {
            Ok(v) => Arc::new(v),
            Err(e) => {
                tracing::error!(error = %e, "release check unavailable");
                return;
            }
        },
    };
    let Ok(fetcher) = AsyncFetcher::new(Source::github(), Duration::from_secs(60)) else {
        return;
    };
    let first = CHECK_FIRST_DELAY + jitter(Duration::from_secs(60));
    let every = CHECK_INTERVAL + jitter(Duration::from_secs(3600));
    spawn_checks(state, fetcher, verifier, trust, first, every);
}

/// The check loop. With the check off (`METRO_UPDATE_CHECK=off`) it starts
/// nothing and makes no request at all.
pub fn spawn_checks(
    state: AppState,
    fetcher: AsyncFetcher,
    verifier: Arc<dyn ReleaseVerifier>,
    trust: Option<Arc<metro_release::trust_refresh::TrustRefresh>>,
    first: Duration,
    every: Duration,
) -> Option<tokio::task::JoinHandle<()>> {
    if !state.updates.check_enabled {
        return None;
    }
    Some(tokio::spawn(async move {
        tokio::time::sleep(first).await;
        loop {
            check_once(&state, &fetcher, verifier.as_ref()).await;
            // Sigstore rotated keys since this build: refresh the trusted root
            // through TUF (weekly at most) and try once more.
            let stale = state.updates.lock().status.error == Some("trust_root_stale");
            if stale
                && let Some(trust) = &trust
                && trust.refresh_if_due().await
            {
                check_once(&state, &fetcher, verifier.as_ref()).await;
            }
            tokio::time::sleep(every).await;
        }
    }))
}

fn jitter(max: Duration) -> Duration {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    Duration::from_millis(u64::from(u32::from_le_bytes(b)) % max.as_millis().max(1) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_setting() {
        for off in ["off", "OFF", " off ", "0", "false", "False", "no"] {
            assert!(!parse_check_setting(Some(off)), "{off:?}");
        }
        for on in ["on", "1", "true", "yes", "", "maybe"] {
            assert!(parse_check_setting(Some(on)), "{on:?}");
        }
        assert!(parse_check_setting(None), "unset means on");
    }

    #[test]
    fn presence_is_lenient_and_bounded() {
        let p = parse_presence(b"", 5);
        assert_eq!((p.seen_at, p.version), (5, None));
        assert_eq!(parse_presence(b"{}", 1).version, None);
        assert_eq!(parse_presence(b"[1,2]", 1).version, None);
        assert_eq!(parse_presence(br#"{"runner":"x"}"#, 1).version, None);
        let p = parse_presence(
            br#"{"runner":{"version":"0.5.0","channel":"release","install":"managed","self_update":true,
                "update":{"state":"failed","target":"0.5.1","code":"verify_failed"}}}"#,
            1,
        );
        assert_eq!(p.version.as_deref(), Some("0.5.0"));
        assert!(p.self_update);
        assert_eq!(p.update.unwrap().code.as_deref(), Some("verify_failed"));
        // self_update needs managed + release.
        let p = parse_presence(br#"{"runner":{"version":"0.5.0","channel":"source","install":"managed","self_update":true}}"#, 1);
        assert!(!p.self_update);
        let p = parse_presence(br#"{"runner":{"version":"0.5.0","channel":"release","install":"container","self_update":true}}"#, 1);
        assert!(!p.self_update);
        // Bad values are dropped, not trusted.
        let p = parse_presence(
            br#"{"runner":{"version":"0.5.0-rc.1","channel":"evil","install":"root","update":{"state":"x"}}}"#,
            1,
        );
        assert_eq!(
            (p.version, p.channel, p.install, p.update),
            (None, None, None, None)
        );
        let p = parse_presence(
            br#"{"runner":{"update":{"state":"failed","code":"Has Spaces http://x"}}}"#,
            1,
        );
        assert_eq!(p.update.unwrap().code, None);
        let big = format!(
            r#"{{"runner":{{"version":"0.5.0","pad":"{}"}}}}"#,
            "a".repeat(3000)
        );
        assert_eq!(parse_presence(big.as_bytes(), 1).version, None);
    }
}
