//! Sign in with ChatGPT through the desktop bridge, end to end and
//! deterministic: a fake Metro server (identity, connections, device
//! tokens), the loopback stand-in for OpenAI's issuer (`common`), a scripted
//! OS opener, and the real bridge and credential store. No vendor account,
//! no real browser, nothing leaves 127.0.0.1.
//!
//! Covers the steps before a browser is needed (private storage, the
//! issuer), a default browser that fails to open and the recovery with Open
//! browser again, the happy path, no double opening, cancel, and a sign-out
//! or account switch during a waiting sign-in. The same file is built for
//! Windows and run there (or under Wine) to exercise the Windows store.

mod common;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use common::{Browser, Fixture, PLAN_SCOPES, browse, fixture, fixture_with_discovery_gate};
use metro_connector::desktop::bridge::{self, Bridge};
use metro_connector::desktop::core::{Desktop, DesktopConfig};
use metro_connector::desktop::store;
use metro_connector::openai_oauth::Endpoints;
use metro_connector::opener::is_start_link;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

// ------------------------------------------------------------------ fake Metro

type Owners = Arc<Mutex<HashMap<String, String>>>;

fn user(h: &HeaderMap) -> Option<&'static str> {
    match h.get("authorization")?.to_str().ok()? {
        "Bearer tok-alice" => Some("alice"),
        "Bearer tok-bob" => Some("bob"),
        _ => None,
    }
}

async fn me(h: HeaderMap) -> Response {
    match user(&h) {
        Some(u) => axum::Json(json!({"id": u, "actor": u, "is_org_admin": false})).into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn providers(State(o): State<Owners>, h: HeaderMap) -> Response {
    let Some(u) = user(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let list: Vec<Value> = o
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, owner)| owner.as_str() == u)
        .map(|(id, _)| json!({"id": id, "name": id, "provider_id": "device-openai-oauth", "model": "", "is_default": false, "has_key": false}))
        .collect();
    axum::Json(json!({"connections": list})).into_response()
}

async fn issue(State(o): State<Owners>, Path(id): Path<String>, h: HeaderMap) -> Response {
    let Some(u) = user(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if o.lock().unwrap().get(&id).map(String::as_str) != Some(u) {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        StatusCode::CREATED,
        axum::Json(json!({"token": format!("metro_pc_{id}_0000000000000001")})),
    )
        .into_response()
}

async fn fake_metro() -> String {
    let owners: Owners = Arc::new(Mutex::new(HashMap::from([
        ("c3".to_string(), "alice".to_string()),
        ("c4".to_string(), "alice".to_string()),
        ("c9".to_string(), "bob".to_string()),
    ])));
    let app = Router::new()
        .route("/v1/me", get(me))
        .route("/v1/providers", get(providers))
        .route(
            "/v1/providers/{id}/device-token",
            post(issue).delete(|| async { StatusCode::NO_CONTENT }),
        )
        .route(
            "/v1/device/claim",
            post(|| async { StatusCode::NO_CONTENT }),
        )
        .with_state(owners);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

// ------------------------------------------------------------------ the scripted OS opener

/// Replays scripted outcomes; after the script, opens successfully. A
/// successful open is a browser that follows the link and approves.
#[derive(Clone)]
struct Opener {
    calls: Arc<Mutex<Vec<String>>>,
    script: Arc<Mutex<VecDeque<Result<(), String>>>>,
    /// The IdP fixture whose "browser" runs on a successful open.
    browse: Option<common::S>,
}

impl Opener {
    fn new(browse: Option<&Fixture>) -> Self {
        Opener {
            calls: Arc::default(),
            script: Arc::default(),
            browse: browse.map(|f| f.s.clone()),
        }
    }

    fn fail_next(&self, why: &str) {
        self.script.lock().unwrap().push_back(Err(why.into()));
    }

    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn last_link(&self) -> String {
        self.calls.lock().unwrap().last().unwrap().clone()
    }

    fn install(&self, cfg: &mut DesktopConfig) {
        let me = self.clone();
        let rt = tokio::runtime::Handle::current();
        cfg.open_browser = Arc::new(move |url: &str| {
            // The real opener refuses anything but the start link; so must this.
            assert!(is_start_link(url), "opened something else");
            me.calls.lock().unwrap().push(url.to_owned());
            let next = me.script.lock().unwrap().pop_front().unwrap_or(Ok(()));
            if next.is_ok()
                && let Some(s) = &me.browse
            {
                rt.spawn(browse(s.clone(), url.to_owned()));
            }
            next
        });
    }
}

// ------------------------------------------------------------------ the UI

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
        // Nothing of the vendor sign-in ever reaches the UI.
        for secret in [
            "metro_pc_",
            "rt-0",
            "access_token",
            "id_token",
            "code_verifier",
            "authorize",
            "state=",
            "nonce",
            common::ISSUED,
            "auth_url",
            "/auth/start",
        ] {
            assert!(!text.contains(secret), "{secret} reached the UI");
        }
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn login(&self, id: &str) -> Value {
        let (s, st) = self.call("GET", "/v1/status", None).await;
        assert_eq!(s, 200);
        st["connections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["connection_id"] == id)
            .map(|c| c["login"].clone())
            .unwrap_or(Value::Null)
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

async fn eventually(what: &str, mut f: impl AsyncFnMut() -> bool) {
    let t = Instant::now();
    while !f().await {
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A bridge for `root` with these issuer endpoints and opener, signed in as
/// alice, with c3 paired as a ChatGPT connection.
async fn setup(root: &std::path::Path, ep: Endpoints, opener: &Opener) -> (Ui, String) {
    setup_with(root, ep, opener, |_| {}).await
}

async fn setup_with(
    root: &std::path::Path,
    ep: Endpoints,
    opener: &Opener,
    configure: impl FnOnce(&mut DesktopConfig),
) -> (Ui, String) {
    let metro = fake_metro().await;
    let mut cfg = DesktopConfig::new(root.to_path_buf(), true, None);
    cfg.poll_interval = Duration::from_millis(50);
    cfg.oauth_endpoints = ep;
    opener.install(&mut cfg);
    configure(&mut cfg);
    let ui = Ui {
        http: http(),
        bridge: bridge::start(Desktop::new(cfg)).await.unwrap(),
    };
    let (s, st) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": metro, "session_token": "tok-alice"})),
        )
        .await;
    assert_eq!(s, 200, "{st}");
    for id in ["c3", "c4"] {
        let (s, c) = ui
            .call(
                "POST",
                &format!("/v1/connections/{id}/pair"),
                Some(json!({"kind": "device-openai-oauth"})),
            )
            .await;
        assert_eq!(s, 200, "{c}");
        assert_eq!(c["credential"], "signed_out");
    }
    (ui, metro)
}

fn sign_in() -> Option<Value> {
    Some(json!({"open_browser": true}))
}

/// Nothing listens there: discovery fails, and no vendor is called.
fn unreachable_issuer() -> Endpoints {
    Endpoints {
        issuer: "http://127.0.0.1:9".into(),
        api_base: "http://127.0.0.1:9/v1".into(),
        resource: "https://api.openai.com/v1".into(),
    }
}

// ------------------------------------------------------------------ tests

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_sign_in_service_fails_before_any_browser_and_says_so() {
    let t = tempfile::tempdir().unwrap();
    let opener = Opener::new(None);
    let (ui, _) = setup(&t.path().join("desktop"), unreachable_issuer(), &opener).await;

    let (s, c) = ui
        .call("POST", "/v1/connections/c3/credentials", sign_in())
        .await;
    assert_eq!(s, 200, "{c}");
    let l = &c["login"];
    assert_eq!(l["state"], "failed", "{c}");
    assert_eq!(l["stage"], "discovery");
    assert_eq!(l["before_browser"], true);
    assert_eq!(l["browser"], "not_requested");
    assert_eq!(l["browser_ready"], false);
    let e = l["error"].as_str().unwrap();
    assert!(
        e.contains("couldn't reach ChatGPT's sign-in service"),
        "{e}"
    );
    assert!(e.contains("no browser was opened"), "{e}");
    assert_ne!(e, "Sign-in did not finish; try again");
    assert_eq!(
        opener.calls(),
        0,
        "nothing to open before the sign-in is ready"
    );
    assert_eq!(c["credential"], "signed_out");

    // A retry is allowed at once (no stuck "in progress").
    let (s, c) = ui
        .call("POST", "/v1/connections/c3/credentials", sign_in())
        .await;
    assert_eq!((s, c["login"]["stage"].as_str()), (200, Some("discovery")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unusable_private_storage_fails_before_any_browser_then_recovers() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("desktop");
    let opener = Opener::new(Some(&f));
    let (ui, metro) = setup(&root, f.ep.clone(), &opener).await;

    // The connection's credential store can't be a directory here.
    let vendor = store::profile_for(&root, &metro, "alice", "c3")
        .unwrap()
        .vendor_dir();
    let _ = std::fs::remove_dir_all(&vendor);
    std::fs::write(&vendor, b"not a directory").unwrap();

    let (s, c) = ui
        .call("POST", "/v1/connections/c3/credentials", sign_in())
        .await;
    assert_eq!(s, 200, "{c}");
    let l = &c["login"];
    assert_eq!(
        (
            l["state"].as_str(),
            l["stage"].as_str(),
            l["before_browser"].as_bool()
        ),
        (Some("failed"), Some("storage"), Some(true)),
        "{c}"
    );
    assert!(
        l["error"]
            .as_str()
            .unwrap()
            .contains("private sign-in storage"),
        "{c}"
    );
    assert_eq!(opener.calls(), 0);
    assert!(
        f.s.lock().unwrap().authorize.is_empty(),
        "the issuer saw nothing"
    );

    // Fixed: the next attempt goes all the way through.
    std::fs::remove_file(&vendor).unwrap();
    let (s, c) = ui
        .call("POST", "/v1/connections/c3/credentials", sign_in())
        .await;
    assert_eq!(s, 200, "{c}");
    eventually("the sign-in to finish", async || {
        ui.login("c3").await["state"] == "done"
    })
    .await;
    let (_, c) = ui
        .call(
            "POST",
            "/v1/connections/c3/credentials",
            Some(json!({"open_browser": false})),
        )
        .await;
    // Signed in now; a new attempt may start (it waits, and is cancelled).
    assert_eq!(c["credential"], "signed_in", "{c}");
    let (s, _) = ui.call("DELETE", "/v1/connections/c3/login", None).await;
    assert_eq!(s, 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_browser_that_fails_to_open_is_reported_and_open_again_completes_the_sign_in() {
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    let t = tempfile::tempdir().unwrap();
    let opener = Opener::new(Some(&f));
    let (ui, _) = setup(&t.path().join("desktop"), f.ep.clone(), &opener).await;

    // The default browser can't be opened (as `ShellExecuteW` reports it).
    opener.fail_next("Windows has no default web browser set for web links.");
    let (s, c) = ui
        .call("POST", "/v1/connections/c3/credentials", sign_in())
        .await;
    assert_eq!(s, 200, "{c}");
    let l = &c["login"];
    assert_eq!(l["state"], "waiting", "{c}");
    assert_eq!(l["browser"], "failed");
    assert!(
        l["browser_error"]
            .as_str()
            .unwrap()
            .contains("no default web browser"),
        "{c}"
    );
    // The sign-in still waits and allows reopening without revealing the link.
    assert_eq!(l["browser_ready"], true);
    assert_eq!(opener.calls(), 1);
    let link = opener.last_link();

    // Pressing Sign in again never opens a second attempt or a second tab.
    let (s, e) = ui
        .call("POST", "/v1/connections/c3/credentials", sign_in())
        .await;
    assert_eq!(
        (s, e["error"]["code"].as_str()),
        (409, Some("login_in_progress"))
    );
    assert_eq!(opener.calls(), 1);
    // Open again takes no URL from the UI.
    let (s, e) = ui
        .call(
            "POST",
            "/v1/connections/c3/login/open",
            Some(json!({"url": "https://example.invalid/"})),
        )
        .await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("bad_request")));
    assert_eq!(opener.calls(), 1);

    // Open the browser again: this time it opens, and the sign-in completes.
    let (s, c) = ui.call("POST", "/v1/connections/c3/login/open", None).await;
    assert_eq!(s, 200, "{c}");
    assert!(
        c["login"]["state"] == "done" || c["login"]["browser"] == "opened",
        "{c}"
    );
    assert_eq!(opener.calls(), 2);
    assert!(opener.last_link() == link, "the same attempt's link");
    eventually("the sign-in to finish", async || {
        ui.login("c3").await["state"] == "done"
    })
    .await;
    let (_, st) = ui.call("GET", "/v1/status", None).await;
    let c3 = st["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["connection_id"] == "c3")
        .unwrap();
    assert_eq!(c3["credential"], "signed_in", "{st}");
    // One authorization request: the reopen continued the same attempt.
    assert_eq!(f.s.lock().unwrap().authorize.len(), 1);

    // Nothing waits now: Open again says so instead of opening anything.
    let (s, e) = ui.call("POST", "/v1/connections/c3/login/open", None).await;
    assert_eq!((s, e["error"]["code"].as_str()), (409, Some("no_login")));
    assert_eq!(opener.calls(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_discovery_preserves_the_attempt_after_the_bridge_reply() {
    for auto_open in [true, false] {
        // Discovery cannot finish until after the POST has returned, regardless
        // of how fast or slow the private credential store is on this machine.
        let gate = Arc::new(Semaphore::new(0));
        let f = fixture_with_discovery_gate(
            Browser::Approve {
                sub: "user-1",
                scope: PLAN_SCOPES,
            },
            Some(gate.clone()),
        )
        .await;
        let t = tempfile::tempdir().unwrap();
        let opener = Opener::new(Some(&f));
        if auto_open {
            opener.fail_next("The default browser is unavailable.");
        }
        let (ui, _) = setup_with(&t.path().join("desktop"), f.ep.clone(), &opener, |cfg| {
            cfg.prepare_wait = Duration::from_millis(50);
        })
        .await;
        let (s, c) = ui
            .call(
                "POST",
                "/v1/connections/c3/credentials",
                Some(json!({"open_browser": auto_open})),
            )
            .await;
        assert_eq!(s, 200, "{c}");
        assert_eq!(c["login"]["state"], "waiting");
        assert_eq!(c["login"]["browser_ready"], false);
        assert_eq!(opener.calls(), 0);
        let (s, e) = ui.call("POST", "/v1/connections/c3/login/open", None).await;
        assert_eq!(
            (s, e["error"]["code"].as_str()),
            (409, Some("login_preparing"))
        );

        gate.add_permits(1);
        eventually("late browser readiness", async || {
            let login = ui.login("c3").await;
            login["browser_ready"] == true
                && login["browser"] == if auto_open { "failed" } else { "not_requested" }
        })
        .await;
        assert_eq!(opener.calls(), usize::from(auto_open));
        let (s, c) = ui.call("POST", "/v1/connections/c3/login/open", None).await;
        assert_eq!(s, 200, "{c}");
        eventually("the late sign-in to finish", async || {
            ui.login("c3").await["state"] == "done"
        })
        .await;
        assert_eq!(ui.login("c3").await["browser_ready"], false);
        assert_eq!(opener.calls(), 1 + usize::from(auto_open));
        assert_eq!(f.s.lock().unwrap().authorize.len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalidating_a_preparing_attempt_prevents_its_late_browser_open() {
    for action in ["cancel", "close", "switch"] {
        let gate = Arc::new(Semaphore::new(0));
        let f = fixture_with_discovery_gate(
            Browser::Approve {
                sub: "user-1",
                scope: PLAN_SCOPES,
            },
            Some(gate.clone()),
        )
        .await;
        let t = tempfile::tempdir().unwrap();
        let opener = Opener::new(Some(&f));
        let (ui, metro) = setup_with(&t.path().join("desktop"), f.ep.clone(), &opener, |cfg| {
            cfg.prepare_wait = Duration::from_millis(50);
        })
        .await;
        let (s, c) = ui
            .call("POST", "/v1/connections/c3/credentials", sign_in())
            .await;
        assert_eq!(s, 200, "{c}");
        assert_eq!(c["login"]["browser_ready"], false);

        let (s, _) = match action {
            "cancel" => ui.call("DELETE", "/v1/connections/c3/login", None).await,
            "close" => ui.call("DELETE", "/v1/session", None).await,
            "switch" => {
                ui.call(
                    "POST",
                    "/v1/session",
                    Some(json!({"server_url": metro, "session_token": "tok-bob"})),
                )
                .await
            }
            _ => unreachable!(),
        };
        assert_eq!(s, 200, "{action}");
        gate.add_permits(1);
        if action != "cancel" {
            let (s, _) = ui
                .call(
                    "POST",
                    "/v1/session",
                    Some(json!({"server_url": metro, "session_token": "tok-alice"})),
                )
                .await;
            assert_eq!(s, 200);
        }
        assert!(ui.login("c3").await.is_null(), "{action}");
        let (s, c) = ui
            .call("POST", "/v1/connections/c3/credentials", sign_in())
            .await;
        assert_eq!(s, 200, "{c}");
        eventually("replacement sign-in to finish", async || {
            ui.login("c3").await["state"] == "done"
        })
        .await;
        assert_eq!(opener.calls(), 1, "{action}: only the replacement opens");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_and_account_switch_end_a_waiting_sign_in() {
    // An issuer the fake browser never completes with: the sign-in waits.
    let f = fixture(Browser::Approve {
        sub: "user-1",
        scope: PLAN_SCOPES,
    })
    .await;
    let t = tempfile::tempdir().unwrap();
    let opener = Opener::new(None);
    let (ui, metro) = setup(&t.path().join("desktop"), f.ep.clone(), &opener).await;
    let link_works = async |link: &str| {
        http()
            .get(link)
            .send()
            .await
            .is_ok_and(|r| r.status().as_u16() == 302)
    };

    let (_, c) = ui
        .call("POST", "/v1/connections/c3/credentials", sign_in())
        .await;
    assert_eq!(
        (c["login"]["state"].as_str(), c["login"]["browser"].as_str()),
        (Some("waiting"), Some("opened")),
        "{c}"
    );
    let link = opener.last_link();
    assert!(link_works(&link).await);

    // Cancel: the link stops working, and a new sign-in may start at once.
    let (s, c) = ui.call("DELETE", "/v1/connections/c3/login", None).await;
    assert_eq!((s, &c["login"]), (200, &Value::Null), "{c}");
    eventually("the cancelled link to stop", async || {
        !link_works(&link).await
    })
    .await;
    let (_, c) = ui
        .call("POST", "/v1/connections/c3/credentials", sign_in())
        .await;
    assert_eq!(c["login"]["state"], "waiting", "{c}");
    let link = opener.last_link();

    // Account switch while it waits: alice's sign-in ends, bob can't reach it.
    let (s, st) = ui
        .call(
            "POST",
            "/v1/session",
            Some(json!({"server_url": metro, "session_token": "tok-bob"})),
        )
        .await;
    assert_eq!((s, st["connections"].clone()), (200, json!([])));
    eventually("the switched-away link to stop", async || {
        !link_works(&link).await
    })
    .await;
    let (s, e) = ui.call("POST", "/v1/connections/c3/login/open", None).await;
    assert_eq!((s, e["error"]["code"].as_str()), (404, Some("not_paired")));

    // Sign-out while a sign-in waits ends it too.
    ui.call(
        "POST",
        "/v1/session",
        Some(json!({"server_url": metro, "session_token": "tok-alice"})),
    )
    .await;
    let (_, c) = ui
        .call("POST", "/v1/connections/c4/credentials", sign_in())
        .await;
    assert_eq!(c["login"]["browser"], "opened");
    let link = opener.last_link();
    ui.call("DELETE", "/v1/session", None).await;
    eventually("the signed-out link to stop", async || {
        !link_works(&link).await
    })
    .await;
    let calls = opener.calls();
    assert_eq!(calls, 3, "one open per attempt, none after it ended");
    assert!(f.s.lock().unwrap().authorize.len() <= 1);
}
