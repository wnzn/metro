//! The desktop bridge end to end, in process: a fake Metro server (identity,
//! connections, device tokens, device claims), a fake OpenAI-compatible
//! endpoint, a fake local workspace, and the real bridge over loopback HTTP.
//! No vendor accounts are called.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use metro_connector::desktop::bridge::{self, Bridge};
use metro_connector::desktop::core::{Desktop, DesktopConfig};
use metro_connector::desktop::local::{BoxFut, LocalState, LocalWorkspace, LocalWorkspaceHost};
use metro_connector::openai_oauth::Endpoints;
use metro_connector::{SafeError, Secret};
use serde_json::{Value, json};

#[derive(Default)]
struct Fake {
    /// connection id -> (owner, provider_id)
    conns: HashMap<String, (String, String)>,
    /// connection id -> current device token
    tokens: HashMap<String, String>,
    issued: u32,
    revoked: Vec<String>,
    /// connection id -> claims seen with its current token
    claims: HashMap<String, u32>,
    refused_claims: u32,
    /// Every request that carried an Authorization header.
    authed: u32,
}

type Shared = Arc<Mutex<Fake>>;

fn user(h: &HeaderMap) -> Option<String> {
    match h.get("authorization")?.to_str().ok()? {
        "Bearer tok-alice" => Some("alice".into()),
        "Bearer tok-bob" => Some("bob".into()),
        _ => None,
    }
}

async fn me(State(s): State<Shared>, h: HeaderMap) -> Response {
    s.lock().unwrap().authed += 1;
    match user(&h) {
        Some(u) => axum::Json(json!({"id": u, "actor": u, "is_org_admin": false})).into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn providers(State(s): State<Shared>, h: HeaderMap) -> Response {
    s.lock().unwrap().authed += 1;
    let Some(u) = user(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let f = s.lock().unwrap();
    let list: Vec<Value> = f
        .conns
        .iter()
        .filter(|(_, (o, _))| *o == u)
        .map(|(id, (_, p))| json!({"id": id, "name": id, "provider_id": p, "model": "", "is_default": false, "has_key": false}))
        .collect();
    axum::Json(json!({"connections": list})).into_response()
}

async fn issue(State(s): State<Shared>, Path(id): Path<String>, h: HeaderMap) -> Response {
    let Some(u) = user(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mut f = s.lock().unwrap();
    if f.conns.get(&id).map(|c| c.0.as_str()) != Some(u.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    f.issued += 1;
    let t = format!("metro_pc_{id}_{:016}", f.issued);
    f.tokens.insert(id, t.clone());
    (StatusCode::CREATED, axum::Json(json!({"token": t}))).into_response()
}

async fn revoke(State(s): State<Shared>, Path(id): Path<String>, h: HeaderMap) -> Response {
    let Some(u) = user(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mut f = s.lock().unwrap();
    if f.conns.get(&id).map(|c| c.0.as_str()) != Some(u.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    f.tokens.remove(&id);
    f.revoked.push(id);
    StatusCode::NO_CONTENT.into_response()
}

async fn claim(State(s): State<Shared>, h: HeaderMap) -> Response {
    let auth = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut f = s.lock().unwrap();
    f.authed += 1;
    let hit = f
        .tokens
        .iter()
        .find(|(_, t)| auth == format!("Bearer {t}"))
        .map(|(id, _)| id.clone());
    match hit {
        Some(id) => {
            *f.claims.entry(id).or_default() += 1;
            StatusCode::NO_CONTENT.into_response()
        }
        None => {
            f.refused_claims += 1;
            StatusCode::UNAUTHORIZED.into_response()
        }
    }
}

async fn models() -> Response {
    axum::Json(json!({"object": "list", "data": [{"id": "m1", "object": "model"}, {"id": "m2", "object": "model"}]}))
        .into_response()
}

async fn fake_metro() -> (String, Shared) {
    let mut f = Fake::default();
    for (id, owner, kind) in [
        ("c1", "alice", "device-openai-compatible"),
        ("c2", "alice", "device-claude-code"),
        ("c3", "alice", "device-openai-oauth"),
        ("c9", "bob", "device-openai-compatible"),
    ] {
        f.conns.insert(id.into(), (owner.into(), kind.into()));
    }
    let s: Shared = Arc::new(Mutex::new(f));
    let app = Router::new()
        .route("/v1/me", get(me))
        .route("/v1/providers", get(providers))
        .route(
            "/v1/providers/{id}/device-token",
            post(issue).delete(revoke),
        )
        .route("/v1/device/claim", post(claim))
        .route("/model/v1/models", get(models))
        .with_state(s.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), s)
}

struct FakeLocal {
    url: String,
    state: Mutex<LocalState>,
    /// Simulates a manager whose `current()` lags behind its state.
    stale_current: std::sync::atomic::AtomicBool,
}

impl FakeLocal {
    fn new(url: &str) -> Self {
        FakeLocal {
            url: url.to_string(),
            state: Mutex::new(LocalState::Stopped),
            stale_current: Default::default(),
        }
    }
    /// The server process died; the address may be reused by anyone.
    fn crash(&self) {
        *self.state.lock().unwrap() = LocalState::Failed;
    }
}

impl LocalWorkspaceHost for FakeLocal {
    fn open(&self) -> BoxFut<'_, Result<LocalWorkspace, SafeError>> {
        Box::pin(async move {
            *self.state.lock().unwrap() = LocalState::Running;
            Ok(LocalWorkspace {
                server_url: self.url.clone(),
                token: Secret::new("tok-alice"),
            })
        })
    }
    fn state(&self) -> LocalState {
        *self.state.lock().unwrap()
    }
    fn shutdown(&self) -> BoxFut<'_, ()> {
        Box::pin(async move {
            *self.state.lock().unwrap() = LocalState::Stopped;
        })
    }
    fn current(&self) -> Option<LocalWorkspace> {
        let stale = self.stale_current.load(std::sync::atomic::Ordering::SeqCst);
        (stale || *self.state.lock().unwrap() == LocalState::Running).then(|| LocalWorkspace {
            server_url: self.url.clone(),
            token: Secret::new("tok-alice"),
        })
    }
}

struct Ui {
    http: reqwest::Client,
    bridge: Bridge,
}

impl Ui {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut rb = self
            .http
            .request(m, format!("{}{}", self.bridge.url, path))
            .bearer_auth(self.bridge.token.expose());
        if let Some(b) = body {
            rb = rb
                .header("content-type", "application/json")
                .body(serde_json::to_vec(&b).unwrap());
        }
        let r = rb.send().await.unwrap();
        let status = r.status().as_u16();
        assert_eq!(
            r.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        let text = r.text().await.unwrap();
        assert!(
            !text.contains("metro_pc_"),
            "device token leaked to the UI: {text}"
        );
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// Send raw bytes and return the status code.
fn raw(port: u16, req: &str) -> u16 {
    let mut s = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn connection<'a>(status: &'a Value, id: &str) -> &'a Value {
    status["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["connection_id"] == id)
        .unwrap_or_else(|| panic!("{id} missing from {status}"))
}

fn config(
    root: &std::path::Path,
    primary: bool,
    local: Option<Arc<dyn LocalWorkspaceHost>>,
) -> DesktopConfig {
    let mut c = DesktopConfig::new(root.to_path_buf(), primary, local);
    c.poll_interval = Duration::from_millis(30);
    c.open_browser = Arc::new(|_| Ok(()));
    // Nothing listens there: the sign-in fails at discovery, no vendor call.
    c.oauth_endpoints = Endpoints {
        issuer: "http://127.0.0.1:9".into(),
        api_base: "http://127.0.0.1:9/v1".into(),
        resource: "http://127.0.0.1:9/v1".into(),
    };
    c
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn guard_refuses_what_is_not_the_ui() {
    let t = tempfile::tempdir().unwrap();
    let desktop = Desktop::new(config(t.path(), true, None));
    let b = bridge::start(desktop).await.unwrap();
    let port = b.port;
    let tok = b.token.expose().to_string();
    assert_eq!(tok.len(), 64);
    assert!(
        tok.bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(b.url, format!("http://127.0.0.1:{port}"));
    let host = format!("127.0.0.1:{port}");
    tokio::task::spawn_blocking(move || {
        let ok = |extra: &str| {
            format!("GET /v1/status HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {tok}\r\n{extra}\r\n")
        };
        assert_eq!(raw(port, &ok("")), 200);
        // No or wrong bearer.
        assert_eq!(raw(port, &format!("GET /v1/status HTTP/1.1\r\nHost: {host}\r\n\r\n")), 401);
        assert_eq!(
            raw(port, &format!("GET /v1/status HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\n\r\n", "0".repeat(64))),
            401
        );
        // Browsers.
        assert_eq!(raw(port, &ok("Origin: http://evil.example\r\n")), 403);
        assert_eq!(raw(port, &ok("Origin: null\r\n")), 403);
        // DNS rebinding / wrong Host.
        for h in ["localhost", "127.0.0.1", "evil.example", "127.0.0.1:1"] {
            let r = format!(
                "GET /v1/status HTTP/1.1\r\nHost: {h}{}\r\nAuthorization: Bearer {tok}\r\n\r\n",
                if h == "localhost" { format!(":{port}") } else { String::new() }
            );
            assert_eq!(raw(port, &r), 403, "{h}");
        }
        assert_eq!(raw(port, &format!("GET /v1/status HTTP/1.1\r\nHost: {host}\r\nHost: {host}\r\nAuthorization: Bearer {tok}\r\n\r\n")), 403);
        // Bodies: JSON only, bounded, with a length.
        let post = |headers: &str, body: &str| {
            format!("POST /v1/session HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {tok}\r\n{headers}\r\n{body}")
        };
        assert_eq!(raw(port, &post("Content-Type: text/plain\r\nContent-Length: 2\r\n", "{}")), 415);
        assert_eq!(raw(port, &post("Content-Type: application/json\r\nContent-Length: 70000\r\n", "")), 413);
        assert_eq!(raw(port, &post("Content-Type: application/json\r\nTransfer-Encoding: chunked\r\n", "2\r\n{}\r\n0\r\n\r\n")), 411);
        assert_eq!(raw(port, &post("Content-Type: application/json\r\nContent-Length: 1\r\n", "x")), 400);
        // Unknown things.
        assert_eq!(raw(port, &format!("GET /v1/nope HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {tok}\r\n\r\n")), 404);
        assert_eq!(raw(port, &format!("PUT /v1/status HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {tok}\r\nContent-Length: 0\r\n\r\n")), 405);
        // A slow client is cut off, it doesn't hold the slot.
        let mut slow = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).unwrap();
        slow.write_all(b"GET /v1/status HTTP/1.1\r\n").unwrap();
        // Connections beyond the cap are closed; normal requests still work after.
        let held: Vec<_> = (0..12)
            .map(|_| TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).unwrap())
            .collect();
        drop(held);
        drop(slow);
    })
    .await
    .unwrap();
    // After the held connections closed, the bridge serves again.
    let http = http();
    let mut served = false;
    for _ in 0..100 {
        let r = http
            .get(format!("{}/v1/status", b.url))
            .bearer_auth(b.token.expose())
            .send()
            .await;
        if r.is_ok_and(|r| r.status() == 200) {
            served = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(served);
    b.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_sessions_and_workers() {
    let (metro, fake) = fake_metro().await;
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("desktop");
    // The local workspace is a separate server from the remote one.
    let (local_url, _local_fake) = fake_metro().await;
    let local: Arc<dyn LocalWorkspaceHost> = Arc::new(FakeLocal::new(&local_url));
    let desktop = Desktop::new(config(&root, true, Some(local)));
    let ui = Ui {
        http: http(),
        bridge: bridge::start(desktop.clone()).await.unwrap(),
    };

    let (s, st) = ui.call("GET", "/v1/status", None).await;
    assert_eq!(s, 200);
    assert_eq!(st["bridge"], 1);
    assert_eq!(st["instance"], "primary");
    assert_eq!(st["session"], Value::Null);
    assert_eq!(st["local"], "stopped");
    assert_eq!(st["kinds"].as_array().unwrap().len(), 6);

    // Nothing without a session.
    let (s, e) = ui
        .call(
            "POST",
            "/v1/connections/c1/pair",
            Some(json!({"kind": "device-openai-compatible"})),
        )
        .await;
    assert_eq!((s, e["error"]["code"].as_str()), (409, Some("no_session")));

    // The local workspace hands out its URL and bearer.
    let (s, l) = ui
        .call("POST", "/v1/local-workspace/open", Some(json!({})))
        .await;
    assert_eq!(s, 200);
    assert_eq!(l["server_url"], local_url.as_str());
    assert_eq!(l["token"], "tok-alice");
    assert_eq!(
        ui.call("GET", "/v1/status", None).await.1["local"],
        "running"
    );

    // Sign-in is verified with the server.
    let (s, e) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": metro, "session_token": "tok-mallory"})),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (422, Some("session_rejected"))
    );
    let (s, e) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": "http://127.0.0.1:9", "session_token": "tok-alice"})),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (502, Some("server_unreachable"))
    );
    let (s, e) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": "http://metro.example.com", "session_token": "tok-alice"})),
        )
        .await;
    assert_eq!((s, e["error"]["code"].as_str()), (422, Some("invalid")));
    let (s, st) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": metro, "session_token": "tok-alice"})),
        )
        .await;
    assert_eq!(s, 200, "{st}");
    assert_eq!(st["session"]["account_id"], "alice");
    assert_eq!(st["connections"], json!([]));

    // Pairing checks the connection is this account's, of this kind.
    let (s, e) = ui
        .call(
            "POST",
            "/v1/connections/c9/pair",
            Some(json!({"kind": "device-openai-compatible", "base_url": "http://127.0.0.1:1/v1"})),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (404, Some("not_your_connection"))
    );
    let (s, e) = ui
        .call(
            "POST",
            "/v1/connections/c1/pair",
            Some(json!({"kind": "device-claude-code"})),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (422, Some("kind_mismatch"))
    );
    let (s, e) = ui
        .call(
            "POST",
            "/v1/connections/c1/pair",
            Some(json!({"kind": "device-openai-compatible"})),
        )
        .await;
    assert_eq!((s, e["error"]["code"].as_str()), (422, Some("invalid")));
    let (s, e) = ui
        .call(
            "POST",
            "/v1/connections/..%2Fx/pair",
            Some(json!({"kind": "device-openai-compatible"})),
        )
        .await;
    assert_eq!(s, 404, "{e}");
    let (s, e) = ui
        .call(
            "POST",
            "/v1/connections/c1/pair",
            Some(json!({"kind": "device-openai-compatible", "extra": 1})),
        )
        .await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("bad_request")));
    assert_eq!(fake.lock().unwrap().issued, 0);

    let base = format!("{metro}/model/v1");
    let (s, c) = ui
        .call(
            "POST",
            "/v1/connections/c1/pair",
            Some(json!({"kind": "device-openai-compatible", "base_url": base})),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    assert_eq!(c["credential"], "missing");
    assert_eq!(c["worker"], "needs_setup");
    assert_eq!(c["base_url"], base.as_str());
    assert_eq!(fake.lock().unwrap().issued, 1);

    // Keys stay on the device; models come from the device.
    let (s, c) = ui
        .call(
            "POST",
            "/v1/connections/c1/credentials",
            Some(json!({"no_api_key": true})),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    assert_eq!(c["credential"], "no_key");
    assert_eq!(c["worker"], "needs_setup"); // no approved model yet
    let (s, m) = ui
        .call("GET", "/v1/connections/c1/available-models", None)
        .await;
    assert_eq!((s, m), (200, json!({"models": ["m1", "m2"]})));
    let (s, e) = ui
        .call(
            "PUT",
            "/v1/connections/c1/models",
            Some(json!({"models": []})),
        )
        .await;
    assert_eq!(s, 422, "{e}");
    let (s, c) = ui
        .call(
            "PUT",
            "/v1/connections/c1/models",
            Some(json!({"models": ["m1"]})),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    assert_eq!(c["models"], json!(["m1"]));
    wait_for("c1 claims", || {
        fake.lock().unwrap().claims.get("c1").copied().unwrap_or(0) >= 2
    })
    .await;
    let st = ui.call("GET", "/v1/status", None).await.1;
    assert_eq!(connection(&st, "c1")["worker"], "online");

    // The device token is stored bound to the server, private.
    let pdir = root
        .join("profiles")
        .join(metro_connector::desktop::store::profile_name(
            &metro, "alice", "c1",
        ));
    let rec = std::fs::read_to_string(pdir.join("device-token")).unwrap();
    assert!(rec.contains(&metro) && rec.contains("metro_pc_c1_"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(pdir.join("device-token"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    // Rotation: the new token serves, the old one is gone.
    let before = fake.lock().unwrap().tokens["c1"].clone();
    let (s, _) = ui
        .call("POST", "/v1/connections/c1/rotate", Some(json!({})))
        .await;
    assert_eq!(s, 200);
    let after = fake.lock().unwrap().tokens["c1"].clone();
    assert_ne!(before, after);
    fake.lock().unwrap().claims.clear();
    wait_for("claims after rotation", || {
        fake.lock().unwrap().claims.get("c1").copied().unwrap_or(0) >= 2
    })
    .await;

    // Claude Code: the key is checked locally before anything else.
    let (s, c) = ui
        .call(
            "POST",
            "/v1/connections/c2/pair",
            Some(json!({"kind": "device-claude-code"})),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    assert_eq!(c["credential"], "signed_out");
    let (s, e) = ui
        .call(
            "POST",
            "/v1/connections/c2/credentials",
            Some(json!({"api_key": "sk-ant-oat01-subscription-token"})),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (422, Some("credential_rejected"))
    );
    assert!(
        e["error"]["message"]
            .as_str()
            .unwrap()
            .contains("subscription")
    );

    // OpenAI sign-in: driven from the app; here the issuer is unreachable.
    let (s, _) = ui
        .call(
            "POST",
            "/v1/connections/c3/pair",
            Some(json!({"kind": "device-openai-oauth"})),
        )
        .await;
    assert_eq!(s, 200);
    let (s, c) = ui
        .call(
            "POST",
            "/v1/connections/c3/credentials",
            Some(json!({"open_browser": false})),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    assert_eq!(c["login"]["state"], "failed", "{c}");
    assert_eq!(c["credential"], "signed_out");

    // Account switch: alice's workers stop; bob sees none of hers.
    let (s, st) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": metro, "session_token": "tok-bob"})),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(st["session"]["account_id"], "bob");
    assert_eq!(st["connections"], json!([]));
    let (s, e) = ui
        .call(
            "PUT",
            "/v1/connections/c1/models",
            Some(json!({"models": ["m2"]})),
        )
        .await;
    assert_eq!((s, e["error"]["code"].as_str()), (404, Some("not_paired")));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let n = fake.lock().unwrap().claims.get("c1").copied().unwrap_or(0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        fake.lock().unwrap().claims.get("c1").copied().unwrap_or(0),
        n,
        "alice's worker kept running for bob"
    );

    // Back to alice: her confirmed connection resumes.
    let (s, st) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": metro, "session_token": "tok-alice"})),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(st["connections"].as_array().unwrap().len(), 3);
    wait_for("claims after resume", || {
        fake.lock().unwrap().claims.get("c1").copied().unwrap_or(0) > n
    })
    .await;

    // A connection deleted in Metro is not served.
    fake.lock().unwrap().conns.remove("c1");
    ui.call("DELETE", "/v1/session", None).await;
    let (_, st) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": metro, "session_token": "tok-alice"})),
        )
        .await;
    assert_eq!(connection(&st, "c1")["worker"], "error");
    fake.lock().unwrap().conns.insert(
        "c1".into(),
        ("alice".into(), "device-openai-compatible".into()),
    );

    // Sign-out stops everything.
    let (s, st) = ui.call("DELETE", "/v1/session", None).await;
    assert_eq!(s, 200);
    assert_eq!(st["session"], Value::Null);
    let n = fake.lock().unwrap().claims.get("c1").copied().unwrap_or(0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        fake.lock().unwrap().claims.get("c1").copied().unwrap_or(0),
        n
    );

    // Forget with revoke: server-side token revoked, local files gone.
    ui.call(
        "POST",
        "/v1/session",
        Some(json!({"server_url": metro, "session_token": "tok-alice"})),
    )
    .await;
    let (s, f) = ui
        .call(
            "POST",
            "/v1/connections/c1/forget",
            Some(json!({"revoke": true})),
        )
        .await;
    assert_eq!((s, f), (200, json!({"forgotten": true, "revoked": true})));
    assert_eq!(fake.lock().unwrap().revoked, vec!["c1".to_string()]);
    assert!(!pdir.exists());

    // The session tokens were never written to disk.
    for e in walk(&root) {
        let bytes = std::fs::read(&e).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains("tok-alice") && !text.contains("tok-bob"),
            "{}",
            e.display()
        );
    }

    desktop.shutdown().await;
    assert_eq!(
        ui.call("GET", "/v1/status", None).await.1["local"],
        "stopped"
    );
    ui.bridge.stop().await;
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secondary_instance_serves_nothing_and_no_local_without_server() {
    let (metro, _fake) = fake_metro().await;
    let t = tempfile::tempdir().unwrap();
    let desktop = Desktop::new(config(t.path(), false, None));
    let ui = Ui {
        http: http(),
        bridge: bridge::start(desktop).await.unwrap(),
    };
    let st = ui.call("GET", "/v1/status", None).await.1;
    assert_eq!(st["instance"], "secondary");
    assert_eq!(st["local"], "unavailable");
    let (s, e) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": metro, "session_token": "tok-alice"})),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (409, Some("other_instance"))
    );
    let (s, e) = ui
        .call("POST", "/v1/local-workspace/open", Some(json!({})))
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (503, Some("local_unavailable"))
    );
    let (s, _) = ui.call("GET", "/v1/local-tools", None).await;
    assert_eq!(s, 404);
    ui.bridge.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_tools_only_for_the_apps_own_hosted_session() {
    let (metro, _fake) = fake_metro().await;
    let t = tempfile::tempdir().unwrap();
    let local: Arc<dyn LocalWorkspaceHost> = Arc::new(FakeLocal::new(&metro));
    let mut cfg = config(t.path(), true, Some(local));
    cfg.tools = Some(Arc::new(metro_connector::desktop_tools::DesktopTools::new(
        t.path().join("local-tools"),
    )));
    let desktop = Desktop::new(cfg);
    let ui = Ui {
        http: http(),
        bridge: bridge::start(desktop.clone()).await.unwrap(),
    };
    let (s, v) = ui.call("GET", "/v1/local-tools", None).await;
    assert_eq!((s, &v["available"]), (200, &json!(false)), "{v}");

    // The app's own hosted server and bearer: tools are available.
    let (_, w) = ui
        .call("POST", "/v1/local-workspace/open", Some(json!({})))
        .await;
    let (s, _) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": w["server_url"], "session_token": w["token"]})),
        )
        .await;
    assert_eq!(s, 200);
    let (s, v) = ui.call("GET", "/v1/local-tools", None).await;
    assert_eq!((s, &v["available"]), (200, &json!(true)), "{v}");

    // Same server, another bearer: refused outright (it isn't the hosted
    // session, and the address belongs to the local workspace).
    let (s, e) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": metro, "session_token": "tok-bob"})),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (409, Some("local_not_running")),
        "{e}"
    );
    // A remote session: no tools.
    let (remote, _r) = fake_metro().await;
    let (s, _) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": remote, "session_token": "tok-bob"})),
        )
        .await;
    assert_eq!(s, 200);
    let (s, v) = ui.call("GET", "/v1/local-tools", None).await;
    assert_eq!((s, &v["available"]), (200, &json!(false)), "{v}");
    let (s, v) = ui
        .call("POST", "/v1/local-tools/spaces/s1/enable", Some(json!({})))
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (409, Some("not_local")),
        "{v}"
    );

    // Sign-out: nothing hosted.
    ui.call("DELETE", "/v1/session", None).await;
    let (_, v) = ui.call("GET", "/v1/local-tools", None).await;
    assert_eq!(v["available"], false);
    desktop.shutdown().await;
    ui.bridge.stop().await;
}

fn tools_cfg(root: &std::path::Path, local: Arc<FakeLocal>) -> DesktopConfig {
    let mut cfg = config(root, true, Some(local as Arc<dyn LocalWorkspaceHost>));
    cfg.tools = Some(Arc::new(metro_connector::desktop_tools::DesktopTools::new(
        root.join("local-tools"),
    )));
    cfg
}

/// The hosted server dies while its session is bound. Its address is then
/// owned by "someone else" (the fake keeps listening): the bridge must end
/// the session, stop workers and tools, never resume them, and send that
/// address nothing more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hosted_session_ends_when_the_local_server_stops() {
    let (addr, other) = fake_metro().await; // the local server, later "someone else"
    let t = tempfile::tempdir().unwrap();
    let local = Arc::new(FakeLocal::new(&addr));
    let desktop = Desktop::new(tools_cfg(t.path(), local.clone()));
    let ui = Ui {
        http: http(),
        bridge: bridge::start(desktop.clone()).await.unwrap(),
    };
    let (_, w) = ui
        .call("POST", "/v1/local-workspace/open", Some(json!({})))
        .await;
    let (s, _) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": w["server_url"], "session_token": w["token"]})),
        )
        .await;
    assert_eq!(s, 200);
    // A device connection paired against the hosted server, serving.
    let base = format!("{addr}/model/v1");
    ui.call(
        "POST",
        "/v1/connections/c1/pair",
        Some(json!({"kind": "device-openai-compatible", "base_url": base, "models": ["m1"]})),
    )
    .await;
    let (s, c) = ui
        .call(
            "POST",
            "/v1/connections/c1/credentials",
            Some(json!({"no_api_key": true})),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    wait_for("hosted claims", || {
        other.lock().unwrap().claims.get("c1").copied().unwrap_or(0) >= 2
    })
    .await;
    assert_eq!(
        ui.call("GET", "/v1/local-tools", None).await.1["available"],
        true
    );

    local.crash();
    wait_for("session ended", || {
        let d = desktop.clone();
        futures_now(async move { d.status().await["session"].is_null() })
    })
    .await;
    // Let anything in flight land, then nothing more may reach the address.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = other.lock().unwrap().authed;
    let (_, v) = ui.call("GET", "/v1/local-tools", None).await;
    assert_eq!(v["available"], false, "{v}");
    let (s, v) = ui
        .call("POST", "/v1/local-tools/spaces/s1/enable", Some(json!({})))
        .await;
    assert_eq!(s, 409, "{v}");
    // Re-binding the old URL and bearer is refused before anything is sent.
    let (s, e) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": w["server_url"], "session_token": w["token"]})),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (409, Some("local_not_running")),
        "{e}"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        other.lock().unwrap().authed,
        before,
        "credentials went to the reused address"
    );
    let st = ui.call("GET", "/v1/status", None).await.1;
    assert_eq!(st["session"], Value::Null);
    assert_eq!(st["local"], "failed");
    desktop.shutdown().await;
    ui.bridge.stop().await;
}

/// A manager whose `current()` still answers after its state left Running
/// is not trusted: no hosted context, no session to that address.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_current_is_not_a_hosted_session() {
    let (addr, other) = fake_metro().await;
    let t = tempfile::tempdir().unwrap();
    let local = Arc::new(FakeLocal::new(&addr));
    let desktop = Desktop::new(tools_cfg(t.path(), local.clone()));
    let ui = Ui {
        http: http(),
        bridge: bridge::start(desktop.clone()).await.unwrap(),
    };
    let (_, w) = ui
        .call("POST", "/v1/local-workspace/open", Some(json!({})))
        .await;
    local
        .stale_current
        .store(true, std::sync::atomic::Ordering::SeqCst);
    *local.state.lock().unwrap() = LocalState::Stopped;
    let before = other.lock().unwrap().authed;
    let (s, e) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": w["server_url"], "session_token": w["token"]})),
        )
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (409, Some("local_not_running")),
        "{e}"
    );
    assert_eq!(
        ui.call("GET", "/v1/local-tools", None).await.1["available"],
        false
    );
    assert_eq!(other.lock().unwrap().authed, before);
    ui.bridge.stop().await;
}

/// The local server stopping has no effect on an ordinary remote session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_sessions_ignore_the_local_server() {
    let (remote, fake) = fake_metro().await;
    let (addr, _local_fake) = fake_metro().await;
    let t = tempfile::tempdir().unwrap();
    let local = Arc::new(FakeLocal::new(&addr));
    let desktop = Desktop::new(tools_cfg(t.path(), local.clone()));
    let ui = Ui {
        http: http(),
        bridge: bridge::start(desktop.clone()).await.unwrap(),
    };
    ui.call("POST", "/v1/local-workspace/open", Some(json!({})))
        .await;
    let (s, _) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": remote, "session_token": "tok-alice"})),
        )
        .await;
    assert_eq!(s, 200);
    let base = format!("{remote}/model/v1");
    ui.call(
        "POST",
        "/v1/connections/c1/pair",
        Some(json!({"kind": "device-openai-compatible", "base_url": base, "models": ["m1"]})),
    )
    .await;
    ui.call(
        "POST",
        "/v1/connections/c1/credentials",
        Some(json!({"no_api_key": true})),
    )
    .await;
    wait_for("remote claims", || {
        fake.lock().unwrap().claims.get("c1").copied().unwrap_or(0) >= 2
    })
    .await;
    local.crash();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let n = fake.lock().unwrap().claims["c1"];
    wait_for("remote claims continue", || {
        fake.lock().unwrap().claims["c1"] > n + 2
    })
    .await;
    let st = ui.call("GET", "/v1/status", None).await.1;
    assert_eq!(st["session"]["server_url"], remote.as_str());
    assert_eq!(connection(&st, "c1")["worker"], "online");
    desktop.shutdown().await;
    ui.bridge.stop().await;
}

/// Runs a future to completion from a sync predicate (tests only).
fn futures_now<F: std::future::Future<Output = bool> + Send + 'static>(f: F) -> bool {
    let h = tokio::runtime::Handle::current();
    std::thread::spawn(move || h.block_on(f)).join().unwrap()
}
