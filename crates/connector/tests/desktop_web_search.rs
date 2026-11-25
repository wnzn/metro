//! Web search in the desktop app, end to end in process: a fake Metro
//! server (identity, web devices, device claims and results), a fake Tavily
//! and the real bridge over loopback HTTP. No vendor is called.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use metro_connector::desktop::bridge::{self, Bridge};
use metro_connector::desktop::core::{Desktop, DesktopConfig};
use metro_connector::web::Endpoints;
use serde_json::{Value, json};

const CANARY: &str = "tvly-canary-key-123456";

#[derive(Default)]
struct Fake {
    created: Vec<(String, String)>,
    deleted: Vec<String>,
    token: Option<String>,
    device: Option<String>,
    /// A job waiting for the device.
    job: Option<Value>,
    results: Vec<Value>,
    claims: u32,
    /// Every request body Metro received.
    bodies: Vec<String>,
}

type Shared = Arc<Mutex<Fake>>;

fn alice(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer tok-alice")
}

async fn me(h: HeaderMap) -> Response {
    if !alice(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(json!({"id": "alice", "actor": "alice"})).into_response()
}

async fn providers() -> Response {
    axum::Json(json!({"connections": []})).into_response()
}

async fn create(State(s): State<Shared>, h: HeaderMap, body: String) -> Response {
    if !alice(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut f = s.lock().unwrap();
    f.bodies.push(body.clone());
    let label = serde_json::from_str::<Value>(&body).unwrap()["label"]
        .as_str()
        .unwrap()
        .to_owned();
    let n = f.created.len();
    let id = format!("wd_{:032x}", n + 1);
    let token = format!("metro_pc_{:064x}", n + 1);
    f.created.push((id.clone(), label));
    f.token = Some(token.clone());
    f.device = Some(id.clone());
    (
        StatusCode::CREATED,
        axum::Json(json!({"id": id, "label": "x", "token": token})),
    )
        .into_response()
}

async fn remove(State(s): State<Shared>, Path(id): Path<String>, h: HeaderMap) -> Response {
    if !alice(&h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut f = s.lock().unwrap();
    f.deleted.push(id);
    f.token = None;
    StatusCode::NO_CONTENT.into_response()
}

async fn claim(State(s): State<Shared>, h: HeaderMap, body: String) -> Response {
    let mut f = s.lock().unwrap();
    f.bodies.push(body);
    let auth = h.get("authorization").and_then(|v| v.to_str().ok());
    if f.token.as_ref().map(|t| format!("Bearer {t}")).as_deref() != auth {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    f.claims += 1;
    match f.job.take() {
        Some(j) => axum::Json(j).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

async fn result(State(s): State<Shared>, Path(_id): Path<String>, body: String) -> Response {
    let mut f = s.lock().unwrap();
    f.bodies.push(body.clone());
    f.results.push(serde_json::from_str(&body).unwrap());
    StatusCode::NO_CONTENT.into_response()
}

async fn tavily(State(s): State<Shared>, h: HeaderMap, body: String) -> Response {
    let mut f = s.lock().unwrap();
    let keyed = h.get("authorization").is_some();
    f.bodies.push(format!("tavily keyed={keyed}"));
    let _ = body;
    axum::Json(json!({"results": [{"title": "T", "url": "https://t.example/", "content": "c"}]}))
        .into_response()
}

async fn fake() -> (String, Shared) {
    let s: Shared = Arc::new(Mutex::new(Fake::default()));
    let app = Router::new()
        .route("/v1/me", get(me))
        .route("/v1/providers", get(providers))
        .route("/v1/web-devices", post(create))
        .route("/v1/web-devices/{id}", delete(remove))
        .route("/v1/device/claim", post(claim))
        .route("/v1/device/jobs/{id}/result", post(result))
        .route("/tavily/search", post(tavily))
        .with_state(s.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), s)
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
        let text = r.text().await.unwrap();
        assert!(!text.contains("metro_pc_"), "device token leaked: {text}");
        assert!(!text.contains(CANARY), "key leaked: {text}");
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn endpoints(base: &str) -> Endpoints {
    Endpoints {
        tavily: format!("{base}/tavily"),
        exa: format!("{base}/exa"),
        parallel: format!("{base}/parallel"),
        firecrawl: format!("{base}/firecrawl"),
        tinyfish_search: format!("{base}/tinyfish-search"),
        tinyfish_fetch: format!("{base}/tinyfish-fetch"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_search_is_on_by_default_serves_jobs_and_keeps_keys_local() {
    let (base, s) = fake().await;
    let t = tempfile::tempdir().unwrap();
    let mut cfg = DesktopConfig::new(t.path().join("desktop"), true, None);
    cfg.poll_interval = Duration::from_millis(30);
    cfg.web_endpoints = Some(endpoints(&base));
    let desktop = Desktop::new(cfg);
    let ui = Ui {
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        bridge: bridge::start(desktop.clone()).await.unwrap(),
    };
    // Before sign-in: no session.
    assert_eq!(ui.call("GET", "/v1/web-search", None).await.0, 409);

    let (status, _) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": base, "session_token": "tok-alice"})),
        )
        .await;
    assert_eq!(status, 200);
    // On by default: registered with Metro, worker polling, notice pending.
    wait_for("claims", || s.lock().unwrap().claims > 0).await;
    let (created_label, device_id) = {
        let f = s.lock().unwrap();
        assert_eq!(f.created.len(), 1);
        (f.created[0].1.clone(), f.created[0].0.clone())
    };
    assert!(created_label.starts_with("Metro desktop ("));
    let (status, state) = ui.call("GET", "/v1/web-search", None).await;
    assert_eq!(status, 200, "{state}");
    assert_eq!(state["enabled"], true);
    assert_eq!(state["notice_acknowledged"], false);
    assert_eq!(state["device"]["paired"], true);
    assert_eq!(state["device"]["id"], device_id);
    let ids: Vec<&str> = state["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["tavily", "exa", "parallel", "firecrawl", "tinyfish"]
    );
    let tinyfish = &state["providers"][4];
    assert_eq!(tinyfish["keyless"], false);
    assert_eq!(tinyfish["key_saved"], false);

    // A web job from Metro runs here and its answer goes back.
    s.lock().unwrap().job =
        Some(json!({"id": "job-1", "operation": "web_search", "request": {"v": 1, "query": "q"}}));
    wait_for("a result", || !s.lock().unwrap().results.is_empty()).await;
    let r = s.lock().unwrap().results[0].clone();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["response"]["provider"], "tavily");
    assert_eq!(r["response"]["mode"], "keyless");
    // A model job is refused.
    s.lock().unwrap().job =
        Some(json!({"id": "job-2", "operation": "complete", "request": {"model": "m"}}));
    wait_for("a refusal", || s.lock().unwrap().results.len() == 2).await;
    assert_eq!(s.lock().unwrap().results[1]["ok"], false);

    // Keys: write-only, used on the next job, never sent to Metro.
    let (status, row) = ui
        .call(
            "PUT",
            "/v1/web-search/providers/tavily/key",
            Some(json!({"api_key": CANARY})),
        )
        .await;
    assert_eq!(status, 200, "{row}");
    assert_eq!(row["key_saved"], true);
    assert_eq!(row["key_status"], "ok");
    assert_eq!(
        ui.call(
            "PUT",
            "/v1/web-search/providers/tavily/key",
            Some(json!({"api_key": "x y"}))
        )
        .await
        .0,
        422
    );
    assert_eq!(
        ui.call(
            "PUT",
            "/v1/web-search/providers/google/key",
            Some(json!({"api_key": CANARY}))
        )
        .await
        .0,
        404
    );
    s.lock().unwrap().job =
        Some(json!({"id": "job-3", "operation": "web_search", "request": {"v": 1, "query": "q"}}));
    wait_for("a keyed result", || s.lock().unwrap().results.len() == 3).await;
    assert_eq!(s.lock().unwrap().results[2]["response"]["mode"], "key");
    assert!(
        s.lock()
            .unwrap()
            .bodies
            .iter()
            .any(|b| b == "tavily keyed=true")
    );
    let (_, row) = ui
        .call("DELETE", "/v1/web-search/providers/tavily/key", None)
        .await;
    assert_eq!(row["key_saved"], false);

    // Order and on/off; a partial list is refused.
    let mut order: Vec<Value> = ["exa", "tavily", "parallel", "firecrawl", "tinyfish"]
        .iter()
        .map(|id| json!({"id": id, "enabled": *id != "parallel"}))
        .collect();
    let (status, state) = ui
        .call(
            "PUT",
            "/v1/web-search",
            Some(json!({"providers": order, "notice_acknowledged": true})),
        )
        .await;
    assert_eq!(status, 200, "{state}");
    assert_eq!(state["providers"][0]["id"], "exa");
    assert_eq!(state["providers"][2]["enabled"], false);
    assert_eq!(state["notice_acknowledged"], true);
    order.pop();
    assert_eq!(
        ui.call("PUT", "/v1/web-search", Some(json!({"providers": order})))
            .await
            .0,
        422
    );
    // A test without a key sends nothing for TinyFish.
    let (_, row) = ui
        .call(
            "POST",
            "/v1/web-search/providers/tinyfish/test",
            Some(json!({})),
        )
        .await;
    assert_eq!(row["test"]["outcome"], "skipped_no_key");
    let (status, _) = ui
        .call(
            "POST",
            "/v1/web-search/providers/exa/clear-cooldown",
            Some(json!({})),
        )
        .await;
    assert_eq!(status, 200);

    // Off: removed from Metro, worker stops. On: registered again.
    let (_, state) = ui
        .call("PUT", "/v1/web-search", Some(json!({"enabled": false})))
        .await;
    assert_eq!(state["enabled"], false);
    assert_eq!(state["device"]["paired"], false);
    assert_eq!(s.lock().unwrap().deleted, vec![device_id.clone()]);
    let claims = s.lock().unwrap().claims;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.lock().unwrap().claims, claims, "no polling while off");
    let (_, state) = ui
        .call("PUT", "/v1/web-search", Some(json!({"enabled": true})))
        .await;
    assert_eq!(state["device"]["paired"], true);
    assert_eq!(s.lock().unwrap().created.len(), 2);
    wait_for("polling again", || s.lock().unwrap().claims > claims).await;

    // Sign-out stops it.
    ui.call("DELETE", "/v1/session", None).await;
    let claims = s.lock().unwrap().claims;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.lock().unwrap().claims, claims);

    // The key never reached Metro, and nothing on disk outside the key file
    // holds it.
    assert!(s.lock().unwrap().bodies.iter().all(|b| !b.contains(CANARY)));
    desktop.shutdown().await;
    ui.bridge.stop().await;
}
