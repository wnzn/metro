//! Release check notice, runner presence and runner update requests.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use metro_release::Version;
use metro_release::fetch::{AsyncFetcher, Source};
use metro_release::images::Images;
use metro_release::testing::{FixtureRelease, TestVerifier};
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::updates::Latest;
use metro_server::{AppState, Config, Limits, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ALICE: &str = "alice-token"; // org admin, owns Space A
const BOB: &str = "bob-token"; // member of A
const CAROL: &str = "carol-token"; // outsider

struct FakeAuth(HashMap<String, UserIdentity>);

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            self.0
                .get(bearer)
                .cloned()
                .ok_or_else(|| AuthFailure::Rejected("unknown".into()))
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

struct Server {
    base: String,
    http: reqwest::Client,
    state: AppState,
}

impl Server {
    async fn start() -> Server {
        Self::start_with_floor(None).await
    }

    async fn start_with_floor(floor: Option<std::path::PathBuf>) -> Server {
        let user = |id: &str| UserIdentity {
            id: id.into(),
            actor: id.into(),
        };
        let auth = FakeAuth(HashMap::from([
            (ALICE.into(), user("alice")),
            (BOB.into(), user("bob")),
            (CAROL.into(), user("carol")),
        ]));
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits::default(),
            org_admins: vec!["alice".into()],
        };
        let mut state = AppState::new(config, Db::open_in_memory().unwrap(), Arc::new(auth))
            .await
            .unwrap();
        if let Some(floor) = floor {
            state.updates = metro_server::updates::Updates::new(true).with_floor_file(floor);
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Server {
            base,
            http: reqwest::Client::new(),
            state,
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        token: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        (r.status(), r.json().await.unwrap_or(Value::Null))
    }
    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.send(reqwest::Method::GET, token, path, None).await
    }
    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::POST, token, path, Some(body))
            .await
    }

    async fn space(&self, token: &str) -> String {
        let (s, b) = self.post(token, "/v1/spaces", json!({"name": "S"})).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        b["id"].as_str().unwrap().into()
    }

    async fn runner(&self, token: &str, visibility: &str) -> (String, String) {
        let (s, b) = self
            .post(
                token,
                "/v1/runners",
                json!({"name": "r", "visibility": visibility}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
        (
            b["runner"]["id"].as_str().unwrap().into(),
            b["token"].as_str().unwrap().into(),
        )
    }

    /// One claim with a body; returns status, all headers as text, and body text.
    async fn claim(&self, runner_token: &str, body: &str) -> (StatusCode, String, String) {
        let r = self
            .http
            .post(format!("{}/v1/runner/claim", self.base))
            .bearer_auth(runner_token)
            .header("X-Metro-Runner-Protocol", "2")
            .header("Content-Type", "application/json")
            .body(body.to_owned())
            .send()
            .await
            .unwrap();
        let status = r.status();
        let headers: String = r
            .headers()
            .iter()
            .map(|(k, v)| format!("{k}: {}\n", v.to_str().unwrap_or("")))
            .collect();
        (status, headers, r.text().await.unwrap_or_default())
    }

    fn set_latest(&self, v: &str) {
        let version = Version::parse(v).unwrap();
        self.state.updates.set_latest(Latest {
            version,
            commit: "a".repeat(40),
            signed_at: 1,
            images: Images {
                runner: format!("ghcr.io/wnzn/metro-runner:v{v}@sha256:{}", "b".repeat(64)),
                server: format!("ghcr.io/wnzn/metro-server:v{v}@sha256:{}", "c".repeat(64)),
                embeddinggemma2: None,
                commit: "a".repeat(40),
            },
        });
    }
}

fn presence(version: &str, install: &str, self_update: bool) -> String {
    json!({"runner": {"version": version, "channel": "release", "install": install, "self_update": self_update}})
        .to_string()
}

fn update_header(headers: &str) -> Option<String> {
    headers.lines().find_map(|l| {
        l.strip_prefix("x-metro-runner-update: ")
            .map(str::to_string)
    })
}

#[tokio::test]
async fn claim_body_never_fails_a_claim_and_sets_presence() {
    let s = Server::start().await;
    let (rid, token) = s.runner(ALICE, "personal").await;
    for body in [
        "",
        "{}",
        "not json",
        "[1]",
        &"x".repeat(5000),
        r#"{"runner":{"version":7}}"#,
    ] {
        let (status, _, _) = s.claim(&token, body).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body:?}");
    }
    let (_, list) = s.get(ALICE, "/v1/runners").await;
    let r = list["runners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == rid.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        r["presence"]["version"],
        Value::Null,
        "old runner without a body: unknown"
    );
    assert_eq!(r["presence"]["online"], true);

    s.claim(&token, &presence("0.5.0", "managed", true)).await;
    let (_, list) = s.get(ALICE, "/v1/runners").await;
    let r = &list["runners"][0];
    assert_eq!(r["presence"]["version"], "0.5.0");
    assert_eq!(r["presence"]["self_update"], true);
    assert_eq!(r["update"]["available"], false);
    assert_eq!(r["update"]["action"], "none");
    s.set_latest("0.5.1");
    let (_, list) = s.get(ALICE, "/v1/runners").await;
    let r = &list["runners"][0];
    assert_eq!(r["update"]["latest"], "0.5.1");
    assert_eq!(r["update"]["available"], true);
    assert_eq!(r["update"]["action"], "self_update");
}

#[tokio::test]
async fn actions_per_install_kind() {
    let s = Server::start().await;
    s.set_latest("0.5.1");
    let (_, t1) = s.runner(ALICE, "personal").await;
    let (_, t2) = s.runner(ALICE, "personal").await;
    let (_, t3) = s.runner(ALICE, "personal").await;
    s.claim(&t1, &presence("0.5.0", "container", false)).await;
    s.claim(&t2, &presence("0.5.0", "managed", false)).await;
    s.claim(&t3, &presence("0.5.0", "desktop", false)).await;
    let (_, list) = s.get(ALICE, "/v1/runners").await;
    let mut actions: Vec<(String, Value)> = list["runners"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["update"]["action"].as_str().unwrap().to_string(),
                r["update"]["container_image"].clone(),
            )
        })
        .collect();
    actions.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(actions[0].0, "container");
    assert!(
        actions[0]
            .1
            .as_str()
            .unwrap()
            .starts_with("ghcr.io/wnzn/metro-runner:v0.5.1@sha256:")
    );
    assert_eq!(actions[1].0, "install_script");
    assert_eq!(actions[1].1, Value::Null);
    assert_eq!(actions[2].0, "with_app");
}

#[tokio::test]
async fn source_runner_shows_version_without_a_release_offer() {
    let s = Server::start().await;
    s.set_latest("0.5.1");
    let (id, token) = s.runner(ALICE, "personal").await;
    let body = json!({"runner": {"version":"0.5.0", "channel":"source", "install":"container", "self_update":true}});
    s.claim(&token, &body.to_string()).await;
    let (_, list) = s.get(ALICE, "/v1/runners").await;
    let r = &list["runners"][0];
    assert_eq!(r["id"], id);
    assert_eq!(r["presence"]["version"], "0.5.0");
    assert_eq!(r["update"]["available"], false);
    assert_eq!(r["update"]["action"], "none");
    assert!(r["update"]["container_image"].is_null());
}

#[tokio::test]
async fn registered_update_authorization_and_header() {
    let s = Server::start().await;
    s.set_latest("0.5.1");
    // Alice's personal runner, an org runner (alice is the admin), Bob's personal runner.
    let (mine, mine_token) = s.runner(ALICE, "personal").await;
    let (org, org_token) = s.runner(ALICE, "organization").await;
    let (bobs, bobs_token) = s.runner(BOB, "personal").await;
    for t in [&mine_token, &org_token, &bobs_token] {
        s.claim(t, &presence("0.5.0", "managed", true)).await;
    }
    let body = json!({"version": "0.5.1"});
    // Outsider / not visible → 404.
    assert_eq!(
        s.post(CAROL, &format!("/v1/runners/{mine}/update"), body.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.post(ALICE, &format!("/v1/runners/{bobs}/update"), body.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.post(ALICE, "/v1/runners/rn-nope/update", body.clone())
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // Org runner: visible to Bob, but only admins manage it → 403.
    assert_eq!(
        s.post(BOB, &format!("/v1/runners/{org}/update"), body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Not the latest → 409.
    let (st, b) = s
        .post(
            ALICE,
            &format!("/v1/runners/{mine}/update"),
            json!({"version": "0.5.2"}),
        )
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("not_latest"))
    );
    // Bad body.
    assert_eq!(
        s.post(
            ALICE,
            &format!("/v1/runners/{mine}/update"),
            json!({"version": "v0.5.1"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.post(
            ALICE,
            &format!("/v1/runners/{mine}/update"),
            json!({"version": "0.5.1", "url": "x"})
        )
        .await
        .0
        .as_u16(),
        422
    );
    // OK → 202, then already pending → 409.
    let (st, b) = s
        .post(ALICE, &format!("/v1/runners/{mine}/update"), body.clone())
        .await;
    assert_eq!(st, StatusCode::ACCEPTED, "{b}");
    assert_eq!(b["pending"]["target"], "0.5.1");
    let (st, b) = s
        .post(ALICE, &format!("/v1/runners/{mine}/update"), body.clone())
        .await;
    assert_eq!(
        (st, b["code"].as_str()),
        (StatusCode::CONFLICT, Some("already_pending"))
    );
    // Org admin on the org runner → 202.
    assert_eq!(
        s.post(ALICE, &format!("/v1/runners/{org}/update"), body.clone())
            .await
            .0,
        StatusCode::ACCEPTED
    );
    // Bob on his own → 202.
    assert_eq!(
        s.post(BOB, &format!("/v1/runners/{bobs}/update"), body.clone())
            .await
            .0,
        StatusCode::ACCEPTED
    );

    // The header goes only to that runner, and nothing else does.
    let (status, headers, text) = s
        .claim(&mine_token, &presence("0.5.0", "managed", true))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(update_header(&headers).as_deref(), Some("0.5.1"));
    assert!(
        !headers.contains("://") && !text.contains("://"),
        "no URL to runners: {headers}{text}"
    );
    // Runner reports it updated → cleared, last_result updated.
    let (_, headers, _) = s
        .claim(&mine_token, &presence("0.5.1", "managed", true))
        .await;
    assert_eq!(update_header(&headers), None);
    let (_, list) = s.get(ALICE, "/v1/runners").await;
    let r = list["runners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == mine.as_str())
        .unwrap()
        .clone();
    assert_eq!(r["update"]["pending"], Value::Null);
    assert_eq!(r["update"]["last_result"]["state"], "updated");
    assert_eq!(r["update"]["available"], false);
    // Failure report clears too.
    let failed = json!({"runner": {"version": "0.5.0", "channel": "release", "install": "managed", "self_update": true,
        "update": {"state": "failed", "target": "0.5.1", "code": "verify_failed"}}})
    .to_string();
    let (_, headers, _) = s.claim(&org_token, &failed).await;
    assert_eq!(update_header(&headers), None);
    let (_, list) = s.get(ALICE, "/v1/runners").await;
    let r = list["runners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == org.as_str())
        .unwrap()
        .clone();
    assert_eq!(r["update"]["last_result"]["code"], "verify_failed");
    // Monitoring: the org runner's events are instance events for admins.
    let (_, ev) = s.get(ALICE, "/v1/monitoring/events").await;
    let codes: Vec<&str> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["code"].as_str())
        .collect();
    assert!(
        codes.contains(&"update_requested") && codes.contains(&"update_failed"),
        "{codes:?}"
    );
}

#[tokio::test]
async fn update_preconditions() {
    let s = Server::start().await;
    let (rid, token) = s.runner(ALICE, "personal").await;
    let body = json!({"version": "0.5.1"});
    let path = format!("/v1/runners/{rid}/update");
    // No verified release yet.
    s.claim(&token, &presence("0.5.0", "managed", true)).await;
    assert_eq!(
        s.post(ALICE, &path, body.clone()).await.1["code"],
        "no_verified_release"
    );
    s.set_latest("0.5.1");
    // Self-update off.
    s.claim(&token, &presence("0.5.0", "managed", false)).await;
    assert_eq!(
        s.post(ALICE, &path, body.clone()).await.1["code"],
        "self_update_off"
    );
    // Already on the latest.
    s.claim(&token, &presence("0.5.1", "managed", true)).await;
    assert_eq!(
        s.post(ALICE, &path, body.clone()).await.1["code"],
        "not_newer"
    );
    // Never seen.
    let (rid2, _) = s.runner(ALICE, "personal").await;
    assert_eq!(
        s.post(ALICE, &format!("/v1/runners/{rid2}/update"), body.clone())
            .await
            .1["code"],
        "runner_offline"
    );
    // Latest never goes backwards.
    s.set_latest("0.5.0");
    assert_eq!(
        s.state.updates.latest().unwrap().version.to_string(),
        "0.5.1"
    );
}

#[tokio::test]
async fn dedicated_update_is_space_owner_only_and_isolated() {
    let s = Server::start().await;
    s.set_latest("0.5.1");
    let a = s.space(ALICE).await;
    let (st, _) = s
        .post(
            ALICE,
            &format!("/v1/spaces/{a}/members"),
            json!({"user_id": "bob"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    let body = json!({"version": "0.5.1"});
    let path = format!("/v1/spaces/{a}/runner/update");
    // No runner yet → 404 for the owner.
    assert_eq!(
        s.post(ALICE, &path, body.clone()).await.0,
        StatusCode::NOT_FOUND
    );
    let (st, tok) = s
        .post(ALICE, &format!("/v1/spaces/{a}/runner-token"), json!({}))
        .await;
    assert_eq!(st, StatusCode::OK);
    let token = tok["token"].as_str().unwrap().to_string();
    // A second Space with its own dedicated runner.
    let b = s.space(ALICE).await;
    let (_, tok_b) = s
        .post(ALICE, &format!("/v1/spaces/{b}/runner-token"), json!({}))
        .await;
    let token_b = tok_b["token"].as_str().unwrap().to_string();
    s.claim(&token, &presence("0.5.0", "managed", true)).await;
    s.claim(&token_b, &presence("0.5.0", "managed", true)).await;
    // Outsider 404, member 403, owner 202.
    assert_eq!(
        s.post(CAROL, &path, body.clone()).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.post(BOB, &path, body.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(ALICE, &path, body.clone()).await.0,
        StatusCode::ACCEPTED
    );
    // Members see presence and the pending update.
    let (_, view) = s.get(BOB, &format!("/v1/spaces/{a}/runner")).await;
    assert_eq!(view["runner"]["presence"]["version"], "0.5.0");
    assert_eq!(view["runner"]["update"]["pending"]["target"], "0.5.1");
    // Only Space A's runner gets the header.
    let (_, h_b, _) = s.claim(&token_b, &presence("0.5.0", "managed", true)).await;
    assert_eq!(update_header(&h_b), None);
    let (_, h_a, _) = s.claim(&token, &presence("0.5.0", "managed", true)).await;
    assert_eq!(update_header(&h_a).as_deref(), Some("0.5.1"));
    // Protocol 1 (old dedicated runner, no body) still claims fine.
    let r = s
        .http
        .post(format!("{}/v1/runner/claim", s.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn server_update_is_admin_only() {
    let s = Server::start().await;
    assert_eq!(
        s.get(BOB, "/v1/server/update").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.get(CAROL, "/v1/server/update").await.0,
        StatusCode::NOT_FOUND
    );
    let (st, b) = s.get(ALICE, "/v1/server/update").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["current"], env!("CARGO_PKG_VERSION"));
    assert_eq!(b["channel"], "source");
    assert_eq!(b["check_enabled"], false, "source builds never check");
    assert_eq!(b["latest"], Value::Null);
    assert_eq!(b["result"], "never");
}

/// The daily check against a loopback fixture GitHub (test verifier).
#[tokio::test]
async fn check_once_records_verified_latest_and_images() {
    use std::io::{BufRead, BufReader, Write};
    let s = Server::start().await;
    let rel = FixtureRelease::new("9.1.0", BTreeMap::new());
    let files = rel.files.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap_or("");
            let name = path.rsplit('/').next().unwrap_or("");
            let body = files.get(name).cloned();
            let (status, body) = match body {
                Some(b) => (200, b),
                None => (404, b"no".to_vec()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    let fetcher = AsyncFetcher::new(
        Source::loopback_fixture(&base).unwrap(),
        Duration::from_secs(10),
    )
    .unwrap();
    metro_server::updates::check_once(&s.state, &fetcher, &TestVerifier).await;
    let latest = s.state.updates.latest().expect("verified");
    assert_eq!(latest.version.to_string(), "9.1.0");
    assert!(
        latest
            .images
            .runner
            .starts_with("ghcr.io/wnzn/metro-runner:v9.1.0@sha256:")
    );
    let (_, b) = s.get(ALICE, "/v1/server/update").await;
    assert_eq!(b["result"], "ok");
    assert_eq!(b["latest"]["version"], "9.1.0");
    assert_eq!(
        b["latest"]["release_url"],
        "https://github.com/wnzn/metro/releases/tag/v9.1.0"
    );
    // A failing check keeps the last verified release and reports the error.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_base = format!("http://{}", dead.local_addr().unwrap());
    drop(dead);
    let fetcher = AsyncFetcher::new(
        Source::loopback_fixture(&dead_base).unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    metro_server::updates::check_once(&s.state, &fetcher, &TestVerifier).await;
    let (_, b) = s.get(ALICE, "/v1/server/update").await;
    assert_eq!(
        (b["result"].as_str(), b["error"].as_str()),
        (Some("error"), Some("offline"))
    );
    assert_eq!(b["latest"]["version"], "9.1.0");
}

/// `METRO_UPDATE_CHECK=off`: the loop starts nothing and the fixture
/// server sees no request; on, it does check.
#[tokio::test]
async fn check_off_makes_no_outbound_request() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let s = Server::start().await;
    let rel = FixtureRelease::new("9.1.0", BTreeMap::new());
    let files = rel.files.clone();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
            }
            let name = line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string();
            let body = files.get(&name).cloned().unwrap_or_default();
            let _ = write!(
                stream,
                "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    let fetcher = || {
        AsyncFetcher::new(
            Source::loopback_fixture(&base).unwrap(),
            Duration::from_secs(5),
        )
        .unwrap()
    };
    let tick = Duration::from_millis(50);

    let mut off = s.state.clone();
    off.updates = metro_server::updates::Updates::new(false);
    let handle = metro_server::updates::spawn_checks(
        off.clone(),
        fetcher(),
        Arc::new(TestVerifier),
        None,
        Duration::ZERO,
        tick,
    );
    assert!(handle.is_none());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "a request was made with the check off"
    );
    assert!(off.updates.latest().is_none());

    let mut on = s.state.clone();
    on.updates = metro_server::updates::Updates::new(true);
    let handle = metro_server::updates::spawn_checks(
        on.clone(),
        fetcher(),
        Arc::new(TestVerifier),
        None,
        Duration::ZERO,
        tick,
    )
    .unwrap();
    for _ in 0..100 {
        if on.updates.latest().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    handle.abort();
    assert!(hits.load(Ordering::SeqCst) > 0);
    assert_eq!(on.updates.latest().unwrap().version.to_string(), "9.1.0");
}

/// Serve a fixture release (optionally with its IMAGES.txt commit changed).
fn fixture_github(version: &str, images_commit: Option<&str>) -> String {
    fixture_github_on_images(version, images_commit, || {})
}

fn fixture_github_on_images(
    version: &str,
    images_commit: Option<&str>,
    before_images: impl FnOnce() + Send + 'static,
) -> String {
    use std::io::{BufRead, BufReader, Write};
    let mut overrides = BTreeMap::new();
    if let Some(c) = images_commit {
        let v = Version::parse(version).unwrap();
        let text = metro_release::testing::fixture_images(&v)
            .replace(metro_release::testing::TEST_COMMIT, c);
        overrides.insert("IMAGES.txt".to_string(), text.into_bytes());
    }
    let files = FixtureRelease::new(version, overrides).files;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut before_images = Some(before_images);
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
            }
            let name = line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string();
            if name == "IMAGES.txt"
                && let Some(before_images) = before_images.take()
            {
                before_images();
            }
            let body = files.get(&name).cloned().unwrap_or_default();
            let _ = write!(
                stream,
                "HTTP/1.1 200 X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    base
}

#[tokio::test]
async fn publication_rechecks_the_shared_floor_after_fetching_images() {
    let dir = tempfile::tempdir().unwrap();
    let floor = dir.path().join("floor");
    let low = Server::start_with_floor(Some(floor.clone())).await;
    let high = Server::start_with_floor(Some(floor.clone())).await;
    let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let first = fixture_github_on_images("9.1.0", None, move || {
        reached_tx.send(()).unwrap();
        resume_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    });
    let low_state = low.state.clone();
    let check = tokio::spawn(async move {
        let f = AsyncFetcher::new(
            Source::loopback_fixture(&first).unwrap(),
            Duration::from_secs(15),
        )
        .unwrap();
        metro_server::updates::check_once(&low_state, &f, &TestVerifier).await;
    });
    tokio::time::timeout(Duration::from_secs(10), reached_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read_to_string(&floor).unwrap(), "9.1.0");
    let latest = fixture_github("9.2.0", Some(&"f".repeat(40)));
    let f = AsyncFetcher::new(
        Source::loopback_fixture(&latest).unwrap(),
        Duration::from_secs(10),
    )
    .unwrap();
    metro_server::updates::check_once(&high.state, &f, &TestVerifier).await;
    assert_eq!(std::fs::read_to_string(&floor).unwrap(), "9.2.0");
    resume_tx.send(()).unwrap();
    check.await.unwrap();
    assert!(
        low.state.updates.latest().is_none(),
        "9.1.0 must not publish after the other process saw 9.2.0"
    );
}

#[tokio::test]
async fn historical_notice_cannot_authorize_or_dispatch_after_another_checker_raises_the_floor() {
    let dir = tempfile::tempdir().unwrap();
    let floor = dir.path().join("floor");
    let s = Server::start_with_floor(Some(floor.clone())).await;
    let other = Server::start_with_floor(Some(floor)).await;
    let (rid, token) = s.runner(ALICE, "personal").await;
    s.claim(&token, &presence("9.0.0", "managed", true)).await;
    s.set_latest("9.1.0");
    let path = format!("/v1/runners/{rid}/update");
    assert_eq!(
        s.post(ALICE, &path, json!({"version":"9.1.0"})).await.0,
        StatusCode::ACCEPTED
    );
    let latest = fixture_github("9.2.0", Some(&"f".repeat(40)));
    let f = AsyncFetcher::new(
        Source::loopback_fixture(&latest).unwrap(),
        Duration::from_secs(10),
    )
    .unwrap();
    metro_server::updates::check_once(&other.state, &f, &TestVerifier).await;
    assert_eq!(
        s.state.updates.latest().unwrap().version,
        Version::parse("9.1.0").unwrap()
    );
    let (_, headers, _) = s.claim(&token, &presence("9.0.0", "managed", true)).await;
    assert!(!headers.contains("x-metro-runner-update:"), "{headers}");
    let (status, body) = s.post(ALICE, &path, json!({"version":"9.1.0"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "not_latest");
    let (_, body) = s.get(ALICE, "/v1/runners").await;
    assert_eq!(body["runners"][0]["update"]["available"], false);
    let (_, body) = s.get(ALICE, "/v1/server/update").await;
    assert_eq!(body["available"], false);
    assert_eq!(body["latest"]["version"], "9.1.0");
    assert_eq!(body["latest"]["historical"], true);
    assert!(body["latest"]["images"].is_null() && body["latest"]["release_url"].is_null());
}

/// A persisted floor: after a restart, an older verified release served as
/// latest neither replaces nor hides the newer one; it's reported apart.
#[tokio::test]
async fn the_release_floor_survives_restarts_and_never_hides_an_offer() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let floor = dir.path().join("metro.release-floor");
    let check = |state: &AppState, base: &str| {
        let fetcher = AsyncFetcher::new(
            Source::loopback_fixture(base).unwrap(),
            Duration::from_secs(10),
        )
        .unwrap();
        let state = state.clone();
        async move { metro_server::updates::check_once(&state, &fetcher, &TestVerifier).await }
    };
    let mut a = s.state.clone();
    a.updates = metro_server::updates::Updates::new(true).with_floor_file(floor.clone());
    // A second process may have loaded the file before the first ever wrote.
    let mut stale = s.state.clone();
    stale.updates = metro_server::updates::Updates::new(true).with_floor_file(floor.clone());
    check(&a, &fixture_github("9.2.0", None)).await;
    assert_eq!(a.updates.latest().unwrap().version.to_string(), "9.2.0");
    assert_eq!(std::fs::read_to_string(&floor).unwrap(), "9.2.0");
    check(&stale, &fixture_github("9.1.0", None)).await;
    assert!(stale.updates.latest().is_none());
    assert_eq!(std::fs::read_to_string(&floor).unwrap(), "9.2.0");
    // "Restart": a fresh state with the same floor file sees an older release.
    let mut b = s.state.clone();
    b.updates = metro_server::updates::Updates::new(true).with_floor_file(floor.clone());
    check(&b, &fixture_github("9.1.0", None)).await;
    assert!(
        b.updates.latest().is_none(),
        "an older release is never taken as latest"
    );
    // In the same process, the newer offer stays and the anomaly is reported.
    check(&a, &fixture_github("9.1.0", None)).await;
    assert_eq!(a.updates.latest().unwrap().version.to_string(), "9.2.0");
    // A malformed floor must not reset the floor and allow an older offer.
    std::fs::write(&floor, "not a version").unwrap();
    let mut c = s.state.clone();
    c.updates = metro_server::updates::Updates::new(true).with_floor_file(floor.clone());
    assert_eq!(c.updates.floor(), None);
    check(&c, &fixture_github("9.1.0", None)).await;
    assert!(c.updates.latest().is_none());
    assert_eq!(std::fs::read_to_string(&floor).unwrap(), "not a version");
}

#[tokio::test]
async fn an_incomplete_newer_release_still_establishes_the_floor() {
    let s = Server::start().await;
    let dir = tempfile::tempdir().unwrap();
    let floor = dir.path().join("metro.release-floor");
    let mut state = s.state.clone();
    state.updates = metro_server::updates::Updates::new(true).with_floor_file(floor.clone());
    for (version, bad_commit) in [("9.2.0", Some("f".repeat(40))), ("9.1.0", None)] {
        let base = fixture_github(version, bad_commit.as_deref());
        let fetcher = AsyncFetcher::new(
            Source::loopback_fixture(&base).unwrap(),
            Duration::from_secs(10),
        )
        .unwrap();
        metro_server::updates::check_once(&state, &fetcher, &TestVerifier).await;
        assert!(state.updates.latest().is_none());
        assert_eq!(std::fs::read_to_string(&floor).unwrap(), "9.2.0");
    }
}

#[tokio::test]
async fn images_must_name_the_signed_commit() {
    let s = Server::start().await;
    let base = fixture_github("9.1.0", Some(&"f".repeat(40)));
    let fetcher = AsyncFetcher::new(
        Source::loopback_fixture(&base).unwrap(),
        Duration::from_secs(10),
    )
    .unwrap();
    metro_server::updates::check_once(&s.state, &fetcher, &TestVerifier).await;
    assert!(s.state.updates.latest().is_none());
    let (_, b) = s.get(ALICE, "/v1/server/update").await;
    assert_eq!(
        (b["result"].as_str(), b["error"].as_str()),
        (Some("error"), Some("inconsistent"))
    );
}

#[tokio::test]
async fn an_older_latest_is_reported_as_an_anomaly() {
    let s = Server::start().await;
    s.set_latest("9.2.0");
    let base = fixture_github("9.1.0", None);
    let fetcher = AsyncFetcher::new(
        Source::loopback_fixture(&base).unwrap(),
        Duration::from_secs(10),
    )
    .unwrap();
    metro_server::updates::check_once(&s.state, &fetcher, &TestVerifier).await;
    let (_, b) = s.get(ALICE, "/v1/server/update").await;
    assert_eq!(b["latest"]["version"], "9.2.0");
    assert_eq!(b["anomaly"], "older_release_served");
}
