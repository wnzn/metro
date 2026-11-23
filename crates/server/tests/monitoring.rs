//! Remote monitoring over real HTTP: `GET /v1/monitoring/events`.
//!
//! Real failures (a dead model endpoint, a runner that never claims, no
//! usable model connection) are recorded through the run and runner
//! lifecycle; only the current Space owner reads a Space's events, only org
//! admins read instance events, nothing personal is recorded, no raw error
//! text leaves the server, and retention by count and age holds across
//! restarts.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";
const CAROL: &str = "carol-token";
/// The organization admin; a member of no Space here.
const DAVE: &str = "dave-token";

struct FakeAuth(HashMap<String, UserIdentity>);

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            self.0
                .get(bearer)
                .cloned()
                .ok_or_else(|| AuthFailure::Rejected("unknown token".to_owned()))
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

fn auth() -> Arc<FakeAuth> {
    let user = |id: &str| UserIdentity {
        id: id.to_owned(),
        actor: id.to_owned(),
    };
    Arc::new(FakeAuth(HashMap::from([
        (ALICE.to_owned(), user("alice")),
        (BOB.to_owned(), user("bob")),
        (CAROL.to_owned(), user("carol")),
        (DAVE.to_owned(), user("dave")),
    ])))
}

/// A model endpoint nobody listens on.
fn dead_model() -> ModelConfig {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    ModelConfig {
        base_url: format!("http://{}/v1", l.local_addr().unwrap()),
        model: "mock".into(),
        api_key: "sk-test-monitoring-secret".into(),
    }
}

fn config(model: Option<ModelConfig>, per_scope: usize, max_age: Duration) -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        db_path: ":memory:".into(),
        model,
        limits: Limits {
            claim_timeout: Duration::from_millis(150),
            run_timeout: Duration::from_secs(5),
            agent_timeout: Duration::from_secs(30),
            monitoring_events_per_scope: per_scope,
            monitoring_max_age: max_age,
            ..Limits::default()
        },
        org_admins: vec!["dave".to_owned()],
    }
}

struct Server {
    base: String,
    http: reqwest::Client,
}

impl Server {
    async fn start(config: Config, db: Db) -> Self {
        let state = AppState::new(config, db, auth()).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
        }
    }

    async fn send(
        &self,
        m: reqwest::Method,
        token: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        let mut req = self
            .http
            .request(m, format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let r = req.send().await.unwrap();
        (r.status(), r.text().await.unwrap())
    }

    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        let (s, text) = self.send(reqwest::Method::GET, token, path, None).await;
        (s, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let (s, text) = self
            .send(reqwest::Method::POST, token, path, Some(body))
            .await;
        (s, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn space(&self, token: &str, name: &str) -> String {
        let (s, v) = self.post(token, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    async fn add(&self, owner: &str, space: &str, user: &str) {
        let (s, v) = self
            .post(
                owner,
                &format!("/v1/spaces/{space}/members"),
                json!({"user_id": user}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }

    async fn pair(&self, owner: &str, space: &str) -> String {
        let (s, v) = self
            .post(
                owner,
                &format!("/v1/spaces/{space}/runner-token"),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["token"].as_str().unwrap().to_owned()
    }

    async fn room_events(&self, token: &str, space: &str) -> Vec<Value> {
        let (s, v) = self
            .get(token, &format!("/v1/spaces/{space}/rooms/lobby/events"))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["events"].as_array().unwrap().clone()
    }

    /// Poll the lobby until `n` lifecycle events in `phase` exist.
    async fn wait_phase(&self, token: &str, space: &str, phase: &str, n: usize) {
        for _ in 0..400 {
            let count = self
                .room_events(token, space)
                .await
                .iter()
                .filter(|e| e["activity"]["phase"] == phase)
                .count();
            if count >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("no {phase} x{n} in {space}");
    }

    /// A Demo run against a runner that never claims: `runner/not_claimed`.
    async fn failed_demo(&self, token: &str, space: &str, nth: usize) {
        let (s, v) = self
            .post(
                token,
                &format!("/v1/spaces/{space}/rooms/lobby/demo"),
                json!({}),
            )
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        self.wait_phase(token, space, "failed", nth).await;
    }

    async fn monitoring(&self, token: &str, query: &str) -> (StatusCode, Value) {
        self.get(token, &format!("/v1/monitoring/events{query}"))
            .await
    }

    /// The `(component, code)` of each event `token` reads, newest first.
    async fn codes(&self, token: &str, query: &str) -> Vec<(String, String)> {
        let (s, v) = self.monitoring(token, query).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        let events = v["events"].as_array().unwrap();
        assert!(events.len() <= 100);
        let mut last: Option<String> = None;
        for e in events {
            // Exactly the contract's four fields.
            let keys: Vec<&str> = e.as_object().unwrap().keys().map(String::as_str).collect();
            assert_eq!(keys.len(), 4, "{e}");
            for k in ["timestamp", "component", "code", "summary"] {
                assert!(e[k].is_string(), "{k} in {e}");
            }
            let ts = e["timestamp"].as_str().unwrap();
            assert!(ts.ends_with('Z'), "{ts}");
            chrono::DateTime::parse_from_rfc3339(ts).unwrap();
            if let Some(prev) = &last {
                assert!(prev.as_str() >= ts, "newest first: {prev} then {ts}");
            }
            last = Some(ts.to_owned());
        }
        events
            .iter()
            .map(|e| {
                (
                    e["component"].as_str().unwrap().to_owned(),
                    e["code"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

fn pair(c: &str, k: &str) -> (String, String) {
    (c.to_owned(), k.to_owned())
}

#[tokio::test]
async fn space_events_are_owner_only_scoped_and_carry_no_raw_text() {
    let model = dead_model();
    let model_addr = model.base_url.clone();
    let s = Server::start(
        config(Some(model), 500, Duration::from_secs(3600)),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "alpha").await;
    s.add(ALICE, &a, "bob").await;
    let b = s.space(BOB, "beta").await;
    let runner = s.pair(ALICE, &a).await;

    // Real failures in Alice's Space: a runner that never claims, then an
    // agent turn whose model endpoint is dead.
    s.failed_demo(BOB, &a, 1).await;
    let (st, v) = s
        .post(
            BOB,
            &format!("/v1/spaces/{a}/rooms/lobby/messages"),
            json!({"text": "secret prompt about /home/bob/plans.txt?q=1", "invoke_agent": true}),
        )
        .await;
    assert!(st.is_success(), "{v}");
    s.wait_phase(ALICE, &a, "failed", 2).await;
    // Personal Chat failures are private and never recorded.
    s.post(
        ALICE,
        "/v1/chat/messages",
        json!({"text": "my private question", "invoke_agent": true}),
    )
    .await;
    for _ in 0..400 {
        let (_, v) = s.get(ALICE, "/v1/chat/events").await;
        if v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["activity"]["phase"] == "failed")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let space_query = format!("?space_id={a}");
    let codes = s.codes(ALICE, &space_query).await;
    assert_eq!(
        codes,
        [
            pair("provider", "unreachable"),
            pair("runner", "not_claimed")
        ]
    );
    // No raw text: not the prompt, path, query, endpoint, key or token.
    let (_, raw) = s.monitoring(ALICE, &space_query).await;
    let raw = raw.to_string();
    for secret in [
        "secret prompt",
        "/home/bob",
        "q=1",
        model_addr.as_str(),
        "127.0.0.1",
        "sk-test-monitoring-secret",
        runner.as_str(),
        "bob",
        "private question",
    ] {
        assert!(!raw.contains(secret), "{secret} leaked: {raw}");
    }

    // Members 403, outsiders and unknown ids 404, admins get no bypass.
    assert_eq!(
        s.monitoring(BOB, &space_query).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.monitoring(CAROL, &space_query).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.monitoring(DAVE, &space_query).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.monitoring(ALICE, "?space_id=nope").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.monitoring(ALICE, "?space_id=..%2Fetc").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.monitoring(ALICE, &format!("?space_id={b}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.monitoring(ALICE, &format!("{space_query}&scope=all"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    // Bob's own Space is empty: nothing crossed from Alice's.
    assert!(s.codes(BOB, &format!("?space_id={b}")).await.is_empty());
    let (_, unauth) = s.monitoring("nobody", &space_query).await;
    assert!(unauth.get("events").is_none());

    // Instance events: org admins only, operational only, no Space or
    // personal event folded in.
    for who in [ALICE, BOB, CAROL] {
        assert_eq!(s.monitoring(who, "").await.0, StatusCode::FORBIDDEN);
    }
    assert_eq!(s.codes(DAVE, "").await, [pair("server", "started")]);

    // Each read rechecks: a member removed is an outsider at once.
    let (st, _) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/spaces/{a}/members"),
            Some(json!({"user_id": "bob"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        s.monitoring(BOB, &space_query).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_run_without_a_usable_model_is_recorded_in_its_space() {
    let s = Server::start(
        config(None, 500, Duration::from_secs(3600)),
        Db::open_in_memory().unwrap(),
    )
    .await;
    let a = s.space(ALICE, "alpha").await;
    s.post(
        ALICE,
        &format!("/v1/spaces/{a}/rooms/lobby/messages"),
        json!({"text": "hi", "invoke_agent": true}),
    )
    .await;
    for _ in 0..200 {
        if !s.codes(ALICE, &format!("?space_id={a}")).await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        s.codes(ALICE, &format!("?space_id={a}")).await,
        [pair("provider", "unavailable")]
    );
}

async fn restart(path: &Path, per_scope: usize, max_age: Duration) -> Server {
    Server::start(config(None, per_scope, max_age), Db::open(path).unwrap()).await
}

#[tokio::test]
async fn retention_by_count_and_age_holds_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metro.sqlite3");
    let hour = Duration::from_secs(3600);

    let s = restart(&path, 3, hour).await;
    let a = s.space(ALICE, "alpha").await;
    s.pair(ALICE, &a).await;
    for n in 1..=5 {
        s.failed_demo(ALICE, &a, n).await;
    }
    let q = format!("?space_id={a}");
    // Count: only the newest three are kept.
    assert_eq!(
        s.codes(ALICE, &q).await,
        vec![pair("runner", "not_claimed"); 3]
    );

    // Restart with a smaller count: the startup prune applies it, and the
    // events persisted.
    let s = restart(&path, 2, hour).await;
    assert_eq!(
        s.codes(ALICE, &q).await,
        vec![pair("runner", "not_claimed"); 2]
    );
    assert_eq!(s.codes(DAVE, "").await, vec![pair("server", "started"); 2]);

    // Restart with a one-second age once they are older than that: gone,
    // and only this start is left for the instance.
    tokio::time::sleep(Duration::from_millis(1_300)).await;
    let s = restart(&path, 500, Duration::from_secs(1)).await;
    assert!(s.codes(ALICE, &q).await.is_empty());
    assert_eq!(s.codes(DAVE, "").await, [pair("server", "started")]);

    // A later restart with a long age does not bring them back: deleted.
    let s = restart(&path, 500, hour).await;
    assert!(s.codes(ALICE, &q).await.is_empty());
    assert_eq!(s.codes(DAVE, "").await, vec![pair("server", "started"); 2]);
}
