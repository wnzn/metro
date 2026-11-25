//! The launcher's loopback bridge: a bounded HTTP/1.1 server the desktop UI
//! (and only it) talks to.
//!
//! - Bound to `127.0.0.1:<ephemeral>` only; the UI gets the URL and a fresh
//!   256-bit bearer through its environment (`METRO_DESKTOP_BRIDGE_URL`,
//!   `METRO_DESKTOP_BRIDGE_TOKEN`), never argv or a file.
//! - Every request needs exactly one `Host: 127.0.0.1:<port>` (DNS
//!   rebinding), no `Origin` (browsers), and exactly one matching
//!   `Authorization: Bearer` (compared in constant time). Bodies are JSON,
//!   at most 64 KiB, with a Content-Length (no chunked uploads).
//! - hyper handles framing; one request per connection, 10 s to send the
//!   head, at most 8 connections at once (more are closed), 120 s per
//!   connection. No redirects; responses are `Cache-Control: no-store`.
//! - Routes and their bodies: see the contract in `desktop/mod.rs`.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, post, put};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;

use super::core::{BridgeError, Desktop, err};
use crate::Secret;

pub const ENV_URL: &str = "METRO_DESKTOP_BRIDGE_URL";
/// `METRO_DESKTOP_LOG=1`: log each bridge request's method, path and status
/// to stderr (never headers or bodies), for diagnostics.
pub const ENV_LOG: &str = "METRO_DESKTOP_LOG";
pub const ENV_TOKEN: &str = "METRO_DESKTOP_BRIDGE_TOKEN";
pub const MAX_BODY: usize = 64 * 1024;
pub const MAX_CONNECTIONS: usize = 8;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(120);

struct Ctx {
    desktop: Arc<Desktop>,
    log: bool,
    host: String,
    /// `Bearer <token>`, as bytes for the constant-time compare.
    auth: Secret,
}

/// A running bridge. Dropping it does not stop it; call [`Bridge::stop`].
pub struct Bridge {
    pub url: String,
    pub token: Secret,
    pub port: u16,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Bridge {
    /// Stop accepting; connections in flight end with their requests.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

/// 256 random bits from the OS, as 64 lowercase hex characters.
pub fn random_token() -> std::io::Result<Secret> {
    use rand::TryRngCore;
    let mut b = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut b)
        .map_err(|_| std::io::Error::other("no OS randomness"))?;
    let mut s = String::with_capacity(64);
    for x in b {
        s.push(char::from(b"0123456789abcdef"[(x >> 4) as usize]));
        s.push(char::from(b"0123456789abcdef"[(x & 15) as usize]));
    }
    zeroize::Zeroize::zeroize(&mut b);
    Ok(Secret::new(s))
}

/// Bind `127.0.0.1:0` and serve the bridge for `desktop`.
pub async fn start(desktop: Arc<Desktop>) -> std::io::Result<Bridge> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await?;
    let port = listener.local_addr()?.port();
    let token = random_token()?;
    let ctx = Arc::new(Ctx {
        desktop,
        log: std::env::var(ENV_LOG).is_ok_and(|v| v == "1"),
        host: format!("127.0.0.1:{port}"),
        auth: Secret::new(format!("Bearer {}", token.expose())),
    });
    let (stop, rx) = watch::channel(false);
    let task = tokio::spawn(serve(listener, router(ctx), rx));
    Ok(Bridge {
        url: format!("http://127.0.0.1:{port}"),
        token,
        port,
        stop,
        task,
    })
}

async fn serve(listener: TcpListener, app: Router, mut stop: watch::Receiver<bool>) {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(x) => x,
                Err(_) => {
                    // Out of descriptors and the like: don't spin.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = async { let _ = stop.wait_for(|v| *v).await; } => break,
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            continue; // dropping the stream closes it
        };
        let svc = TowerToHyperService::new(app.clone());
        tokio::spawn(async move {
            let conn = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(HEAD_TIMEOUT)
                .keep_alive(false)
                .max_buf_size(16 * 1024)
                .serve_connection(TokioIo::new(stream), svc);
            let _ = tokio::time::timeout(CONNECTION_TIMEOUT, conn).await;
            drop(permit);
        });
    }
}

fn error_response(e: &BridgeError) -> Response {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        status,
        axum::Json(json!({"error": {"code": e.code, "message": e.message}})),
    )
        .into_response()
}

impl IntoResponse for BridgeError {
    fn into_response(self) -> Response {
        error_response(&self)
    }
}

fn ok(v: Value) -> Response {
    axum::Json(v).into_response()
}

/// Origin, Host, bearer and body-shape checks for every request, then the
/// response headers every answer carries.
async fn guard(State(ctx): State<Arc<Ctx>>, req: Request, next: Next) -> Response {
    let line = ctx.log.then(|| {
        format!(
            "{} {}",
            req.method(),
            crate::secret::clean_line(req.uri().path(), 200)
        )
    });
    let h = req.headers();
    let hosts: Vec<_> = h.get_all(header::HOST).iter().collect();
    let bad_host = hosts.len() != 1
        || hosts[0].as_bytes() != ctx.host.as_bytes()
        || req
            .uri()
            .authority()
            .is_some_and(|a| a.as_str() != ctx.host);
    let auths: Vec<_> = h.get_all(header::AUTHORIZATION).iter().collect();
    let bad_auth =
        auths.len() != 1 || !bool::from(auths[0].as_bytes().ct_eq(ctx.auth.expose().as_bytes()));
    let refusal = if h.contains_key(header::ORIGIN) {
        Some(err(
            403,
            "origin_refused",
            "Requests from web pages are refused",
        ))
    } else if bad_host {
        Some(err(403, "host_refused", "Wrong Host"))
    } else if bad_auth {
        Some(err(401, "unauthorized", "Missing or wrong bridge token"))
    } else if h.contains_key(header::TRANSFER_ENCODING) {
        Some(err(411, "length_required", "Send a Content-Length"))
    } else {
        let len = h
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let json_type = h
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|t| t.trim().eq_ignore_ascii_case("application/json"))
            });
        if len > MAX_BODY as u64 {
            Some(err(413, "too_large", "The request body is too large"))
        } else if len > 0 && !json_type {
            Some(err(
                415,
                "unsupported_media_type",
                "Send JSON (Content-Type: application/json)",
            ))
        } else {
            None
        }
    };
    let mut resp = match refusal {
        Some(e) => error_response(&e),
        None => next.run(req).await,
    };
    let hs = resp.headers_mut();
    hs.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    hs.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    hs.insert(header::CONNECTION, HeaderValue::from_static("close"));
    if let Some(l) = line {
        eprintln!("metro-desktop: bridge {l} -> {}", resp.status().as_u16());
    }
    resp
}

fn router(ctx: Arc<Ctx>) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/monitoring/events", get(monitoring_events))
        .route("/v1/session", post(session_open).delete(session_close))
        .route("/v1/connections/{id}/pair", post(pair))
        .route("/v1/connections/{id}/rotate", post(rotate))
        .route(
            "/v1/connections/{id}/credentials",
            post(credentials_set).delete(credentials_delete),
        )
        .route(
            "/v1/connections/{id}/available-models",
            get(available_models),
        )
        .route("/v1/connections/{id}/login", delete(login_cancel))
        .route("/v1/connections/{id}/login/open", post(login_open))
        .route("/v1/connections/{id}/models", put(models))
        .route("/v1/connections/{id}/endpoint", put(endpoint))
        .route("/v1/connections/{id}/forget", post(forget))
        .route("/v1/local-workspace/open", post(local_open))
        .route("/v1/update", get(update_state))
        .route("/v1/update/check", post(update_check))
        .route("/v1/update/settings", put(update_settings))
        .route("/v1/update/download", post(update_download))
        .route("/v1/update/apply", post(update_apply))
        .route("/v1/update/cancel", post(update_cancel))
        .route("/v1/update/started", post(update_started))
        .route("/v1/local-tools", any(tools))
        .route("/v1/local-tools/{*rest}", any(tools))
        .fallback(|| async { err(404, "not_found", "No such bridge endpoint").into_response() })
        .method_not_allowed_fallback(|| async {
            err(405, "method_not_allowed", "Wrong method for this endpoint").into_response()
        })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(ctx.clone(), guard))
        .with_state(ctx)
}

/// Parse a JSON body (empty means `{}`); unknown fields are refused.
fn body<T: for<'de> Deserialize<'de>>(b: &Bytes) -> Result<T, BridgeError> {
    let bytes: &[u8] = if b.is_empty() { b"{}" } else { b };
    serde_json::from_slice(bytes).map_err(|_| {
        err(
            400,
            "bad_request",
            "The request body is not valid for this endpoint",
        )
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

async fn status(State(ctx): State<Arc<Ctx>>) -> Response {
    ok(ctx.desktop.status().await)
}

async fn monitoring_events(State(ctx): State<Arc<Ctx>>) -> Response {
    let events = ctx.desktop.events.clone();
    match tokio::task::spawn_blocking(move || events.recent()).await {
        Ok(Ok(events)) => ok(json!({"events": events})),
        _ => err(
            503,
            "monitoring_unavailable",
            "Device events are unavailable",
        )
        .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionBody {
    server_url: String,
    session_token: String,
}

async fn session_open(State(ctx): State<Arc<Ctx>>, b: Bytes) -> Response {
    let mut parsed: SessionBody = match body(&b) {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let token = Secret::new(std::mem::take(&mut parsed.session_token));
    match ctx.desktop.open_session(&parsed.server_url, token).await {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn session_close(State(ctx): State<Arc<Ctx>>, b: Bytes) -> Response {
    if let Err(e) = body::<Empty>(&b) {
        return e.into_response();
    }
    ok(ctx.desktop.close_session().await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairBody {
    kind: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    models: Option<Vec<String>>,
}

fn reply(r: Result<Value, BridgeError>) -> Response {
    match r {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn pair(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, b: Bytes) -> Response {
    match body::<PairBody>(&b) {
        Ok(p) => reply(
            ctx.desktop
                .pair(&id, &p.kind, p.base_url.as_deref(), p.models)
                .await,
        ),
        Err(e) => e.into_response(),
    }
}

async fn rotate(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, b: Bytes) -> Response {
    match body::<Empty>(&b) {
        Ok(_) => reply(ctx.desktop.rotate(&id).await),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialsBody {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    no_api_key: bool,
    #[serde(default = "yes")]
    open_browser: bool,
}

fn yes() -> bool {
    true
}

async fn credentials_set(
    State(ctx): State<Arc<Ctx>>,
    Path(id): Path<String>,
    b: Bytes,
) -> Response {
    match body::<CredentialsBody>(&b) {
        Ok(mut c) => {
            let key = c.api_key.take().map(Secret::new);
            reply(
                ctx.desktop
                    .set_credentials(&id, key, c.no_api_key, c.open_browser)
                    .await,
            )
        }
        Err(e) => e.into_response(),
    }
}

/// Opens the waiting sign-in's own start link again; the body is empty (no URL is accepted).
async fn login_open(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, b: Bytes) -> Response {
    match body::<Empty>(&b) {
        Ok(_) => reply(ctx.desktop.reopen_sign_in(&id).await),
        Err(e) => e.into_response(),
    }
}

async fn login_cancel(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>) -> Response {
    reply(ctx.desktop.cancel_sign_in(&id).await)
}

async fn credentials_delete(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>) -> Response {
    reply(ctx.desktop.delete_credentials(&id).await)
}

async fn available_models(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>) -> Response {
    reply(ctx.desktop.available_models(&id).await)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelsBody {
    models: Vec<String>,
}

async fn models(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, b: Bytes) -> Response {
    match body::<ModelsBody>(&b) {
        Ok(m) => reply(ctx.desktop.set_models(&id, m.models).await),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointBody {
    base_url: String,
}

async fn endpoint(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, b: Bytes) -> Response {
    match body::<EndpointBody>(&b) {
        Ok(e) => reply(ctx.desktop.set_endpoint(&id, &e.base_url).await),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgetBody {
    #[serde(default)]
    revoke: bool,
}

async fn forget(State(ctx): State<Arc<Ctx>>, Path(id): Path<String>, b: Bytes) -> Response {
    match body::<ForgetBody>(&b) {
        Ok(f) => reply(ctx.desktop.forget(&id, f.revoke).await),
        Err(e) => e.into_response(),
    }
}

async fn local_open(State(ctx): State<Arc<Ctx>>, b: Bytes) -> Response {
    if let Err(e) = body::<Empty>(&b) {
        return e.into_response();
    }
    match ctx.desktop.open_local().await {
        Ok((url, token)) => ok(json!({"server_url": url, "token": token.expose()})),
        Err(e) => e.into_response(),
    }
}

async fn tools(State(ctx): State<Arc<Ctx>>, req: Request<Body>) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let bytes = match axum::body::to_bytes(req.into_body(), MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return err(413, "too_large", "The request body is too large").into_response(),
    };
    let m = match method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        _ => {
            return err(405, "method_not_allowed", "Wrong method for this endpoint")
                .into_response();
        }
    };
    let (status, v) = ctx.desktop.tools_request(m, &path, &bytes).await;
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, axum::Json(v)).into_response()
}

// ---- updates (see crate::desktop::update) ----
//
// A `would_interrupt` refusal of apply is answered from the same state the
// UI reads: `GET /v1/update` carries `busy` (serving connections, local
// workspace).

fn updater(ctx: &Ctx) -> Option<&Arc<crate::desktop::update::Updater>> {
    ctx.desktop.updater()
}

fn unavailable() -> Response {
    err(
        503,
        "update_unavailable",
        "Updates are handled by the first Metro window",
    )
    .into_response()
}

fn refusal(r: crate::desktop::update::Refusal) -> Response {
    err(r.status, r.code, &r.message).into_response()
}

fn update_view(ctx: &Ctx, u: &crate::desktop::update::Updater) -> Response {
    ok(u.state(ctx.desktop.update_busy()))
}

async fn update_state(State(ctx): State<Arc<Ctx>>) -> Response {
    match updater(&ctx) {
        Some(u) => update_view(&ctx, u),
        None => unavailable(),
    }
}

async fn update_check(State(ctx): State<Arc<Ctx>>, b: Bytes) -> Response {
    if let Err(e) = body::<Empty>(&b) {
        return error_response(&e);
    }
    let u = match updater(&ctx) {
        Some(u) => u.clone(),
        None => return unavailable(),
    };
    match u.check(true).await {
        Ok(()) => update_view(&ctx, &u),
        Err(r) => refusal(r),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateSettings {
    auto_check: bool,
}

async fn update_settings(State(ctx): State<Arc<Ctx>>, b: Bytes) -> Response {
    let s: UpdateSettings = match body(&b) {
        Ok(s) => s,
        Err(e) => return error_response(&e),
    };
    match updater(&ctx) {
        Some(u) => match u.set_auto_check(s.auto_check) {
            Ok(()) => update_view(&ctx, u),
            Err(r) => refusal(r),
        },
        None => unavailable(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateVersion {
    version: String,
}

async fn update_download(State(ctx): State<Arc<Ctx>>, b: Bytes) -> Response {
    let v: UpdateVersion = match body(&b) {
        Ok(v) => v,
        Err(e) => return error_response(&e),
    };
    match updater(&ctx) {
        Some(u) => match u.start_download(&v.version) {
            Ok(()) => update_view(&ctx, u),
            Err(r) => refusal(r),
        },
        None => unavailable(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateApply {
    version: String,
    #[serde(default)]
    confirm_interrupt: bool,
}

async fn update_apply(State(ctx): State<Arc<Ctx>>, b: Bytes) -> Response {
    let a: UpdateApply = match body(&b) {
        Ok(a) => a,
        Err(e) => return error_response(&e),
    };
    match updater(&ctx) {
        Some(u) => {
            let busy = ctx.desktop.update_busy();
            match u.request_apply(&a.version, a.confirm_interrupt, busy) {
                Ok(()) => update_view(&ctx, u),
                Err(r) => refusal(r),
            }
        }
        None => unavailable(),
    }
}

async fn update_cancel(State(ctx): State<Arc<Ctx>>, b: Bytes) -> Response {
    if let Err(e) = body::<Empty>(&b) {
        return error_response(&e);
    }
    match updater(&ctx) {
        Some(u) => match u.cancel() {
            Ok(()) => update_view(&ctx, u),
            Err(r) => refusal(r),
        },
        None => unavailable(),
    }
}

async fn update_started(State(ctx): State<Arc<Ctx>>, b: Bytes) -> Response {
    if let Err(e) = body::<Empty>(&b) {
        return error_response(&e);
    }
    match updater(&ctx) {
        Some(u) => ok(json!({"trial": u.started()})),
        None => unavailable(),
    }
}
