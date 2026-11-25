//! Model connection capabilities and sharing over real HTTP, against a
//! loopback fixture that speaks System One, embeddings and Chat
//! Completions: per-connection private defaults, owner-only share
//! management, use in a Space only while both people are members, borrowed
//! use on the owner's endpoint without ever showing its key or URL,
//! revocation that stops the next request, subscription presets that never
//! run on someone else's account, fixed capabilities and per-capability
//! defaults, and fingerprints that follow model edits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use base64::Engine;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::providers::secret::MasterKey;
use metro_server::providers::{self, ProviderError, ProviderService, UseError};
use metro_server::{AppState, Config, Limits, router};
use reqwest::StatusCode;
use rig_agent::prelude::*;
use serde_json::{Value, json};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";
const CAROL: &str = "carol-token";
const ALICE_KEY: &str = "sk-alice-very-secret-0123456789";
/// Text only a provider's own error carries: it must never reach a room, a
/// run's private failure, a personal Chat or a log line.
const SENTINEL: &str = "SENTINEL-provider-text-7f3a91";

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
    ])))
}

fn master() -> MasterKey {
    MasterKey::parse(&base64::engine::general_purpose::STANDARD.encode([5u8; 32])).unwrap()
}

// ---- Fixture endpoint ----

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    authorization: Option<String>,
    body: Value,
}

struct Fixture {
    base_url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Typed answers wait for a read lock: a test holding the write lock
    /// keeps them pending after the request arrived.
    gate: Arc<tokio::sync::RwLock<()>>,
}

impl Fixture {
    async fn start() -> Self {
        let seen = Arc::new(Mutex::new(Vec::<Seen>::new()));
        let gate = Arc::new(tokio::sync::RwLock::new(()));
        let (g1, g2, g3) = (gate.clone(), gate.clone(), gate.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let record = |seen: &Arc<Mutex<Vec<Seen>>>, path: &str, h: &HeaderMap, body: Value| {
            seen.lock().unwrap().push(Seen {
                path: path.to_owned(),
                authorization: h
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_owned()),
                body,
            });
        };
        let (s1, s2, s3, s4) = (seen.clone(), seen.clone(), seen.clone(), seen.clone());
        let app = axum::Router::new()
            .route(
                "/v1/models",
                get(move |h: HeaderMap| {
                    let seen = s1.clone();
                    async move {
                        record(&seen, "/v1/models", &h, Value::Null);
                        Json(json!({
                            "models": [{"name": "jev-latest", "description": "d",
                                        "release_date": "2026-09-15"}],
                            "data": [{"id": "emb-small"}, {"id": "emb-large"}],
                        }))
                    }
                }),
            )
            .route(
                "/v1/systemone",
                post(move |h: HeaderMap, Json(body): Json<Value>| {
                    let (seen, gate) = (s2.clone(), g1.clone());
                    async move {
                        record(&seen, "/v1/systemone", &h, body.clone());
                        let _open = gate.read().await;
                        let answers: serde_json::Map<String, Value> = body["questions"]
                            .as_object()
                            .unwrap()
                            .keys()
                            .map(|k| (k.clone(), json!({"type": "noul", "noul": 0.9})))
                            .collect();
                        Json(json!({
                            "model": "jev-1.14.0",
                            "answers": answers,
                            "usage": {"input_tokens": 3, "output_tokens": 1},
                        }))
                    }
                }),
            )
            .route(
                "/v1/embeddings",
                post(move |h: HeaderMap, Json(body): Json<Value>| {
                    let (seen, gate) = (s3.clone(), g2.clone());
                    async move {
                        record(&seen, "/v1/embeddings", &h, body.clone());
                        let _open = gate.read().await;
                        let n = body["input"].as_array().unwrap().len();
                        // Out of order on purpose: the client sorts by index.
                        let data: Vec<Value> = (0..n)
                            .rev()
                            .map(|i| {
                                json!({"object": "embedding", "index": i,
                                       "embedding": [i as f64 + 1.0, 0.5]})
                            })
                            .collect();
                        Json(json!({"object": "list", "model": body["model"], "data": data}))
                    }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(move |h: HeaderMap, Json(body): Json<Value>| {
                    let (seen, gate) = (s4.clone(), g3.clone());
                    async move {
                        record(&seen, "/v1/chat/completions", &h, body.clone());
                        let messages = body["messages"].as_array().cloned().unwrap_or_default();
                        let asked = messages
                            .iter()
                            .rev()
                            .find(|m| m["role"] == "user")
                            .map(|m| m["content"].to_string())
                            .unwrap_or_default();
                        let replied = messages.iter().any(|m| m["role"] == "tool");
                        // A two-request run: a tool call first (held at the
                        // gate), so a share can end between the requests.
                        if asked.contains("[tool]") && !replied {
                            let _open = gate.read().await;
                            let tool = body["tools"][0]["function"]["name"].clone();
                            return Json(json!({
                                "id": "chatcmpl-1", "object": "chat.completion", "created": 0,
                                "model": body["model"],
                                "choices": [{"index": 0, "finish_reason": "tool_calls",
                                             "message": {"role": "assistant", "content": null,
                                                         "tool_calls": [{"id": "call_1", "type": "function",
                                                             "function": {"name": tool, "arguments": "{}"}}]}}],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1,
                                          "total_tokens": 2}
                            }))
                            .into_response();
                        }
                        // An upstream failure whose text names the account
                        // and echoes the credential it was sent.
                        if asked.contains("[fail]") && !asked.contains("[tool]") && !asked.contains("[garbage]") {
                            let echoed = h
                                .get("authorization")
                                .map(|v| v.to_str().unwrap().to_owned())
                                .unwrap_or_default();
                            return (
                                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                                Json(json!({"error": {"message":
                                    format!("{SENTINEL}: quota exceeded; you sent {echoed}")}})),
                            )
                                .into_response();
                        }
                        // A malformed reply.
                        if asked.contains("[garbage]") && !asked.contains("[tool]") {
                            return format!("not json {SENTINEL}").into_response();
                        }
                        Json(json!({
                            "id": "chatcmpl-1", "object": "chat.completion", "created": 0,
                            "model": body["model"],
                            "choices": [{"index": 0, "finish_reason": "stop",
                                         "message": {"role": "assistant", "content": "hello"}}],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1,
                                      "total_tokens": 2}
                        }))
                        .into_response()
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base_url: format!("http://{addr}/v1"),
            seen,
            gate,
        }
    }

    fn seen(&self, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.path == path)
            .cloned()
            .collect()
    }
}

// ---- Server ----

struct Server {
    base: String,
    http: reqwest::Client,
    state: AppState,
    fixture: Fixture,
}

impl Server {
    async fn start() -> Self {
        Self::start_with(false).await
    }

    /// With `operator`, the server's own `METRO_MODEL` is the fixture too.
    async fn start_with(operator: bool) -> Self {
        let fixture = Fixture::start().await;
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: operator.then(|| metro_server::ModelConfig {
                base_url: fixture.base_url.clone(),
                model: "operator-model".into(),
                api_key: "sk-operator-0123456789".into(),
            }),
            limits: Limits {
                agent_timeout: Duration::from_secs(20),
                ..Limits::default()
            },
            // Bob is an org admin: that gives him no say over Alice's
            // connections.
            org_admins: vec!["bob".to_owned()],
        };
        let mut state = AppState::new(config, Db::open_in_memory().unwrap(), auth())
            .await
            .unwrap();
        state.providers =
            ProviderService::new(Some(master()), vec![fixture.base_url.clone()]).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            state,
            fixture,
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
        if let Some(body) = body {
            req = req.json(&body);
        }
        let r = req.send().await.unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }
    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.send(reqwest::Method::GET, token, path, None).await
    }
    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::POST, token, path, Some(body))
            .await
    }
    async fn put(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::PUT, token, path, Some(body))
            .await
    }
    async fn patch(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::PATCH, token, path, Some(body))
            .await
    }

    async fn connect(&self, token: &str, provider: &str, model: &str, key: bool) -> String {
        let mut body = json!({"name": provider, "provider_id": provider, "model": model});
        if provider.starts_with("custom") {
            body["base_url"] = json!(self.fixture.base_url);
        }
        if key {
            body["api_key"] = json!(ALICE_KEY);
        }
        let (status, c) = self.post(token, "/v1/providers", body).await;
        assert_eq!(status, StatusCode::CREATED, "{c}");
        c["id"].as_str().unwrap().to_owned()
    }

    async fn share(&self, token: &str, id: &str, body: Value) -> (StatusCode, Value) {
        self.put(token, &format!("/v1/providers/{id}/sharing"), body)
            .await
    }

    async fn space(&self, token: &str, name: &str, members: &[&str]) -> String {
        let (status, space) = self.post(token, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(status, StatusCode::CREATED, "{space}");
        let sid = space["id"].as_str().unwrap().to_owned();
        for m in members {
            let (status, body) = self
                .post(
                    token,
                    &format!("/v1/spaces/{sid}/members"),
                    json!({"user_id": m}),
                )
                .await;
            assert!(status.is_success(), "{status} {body}");
        }
        sid
    }

    async fn remove_member(&self, token: &str, sid: &str, user: &str) {
        let (status, body) = self
            .send(
                reqwest::Method::DELETE,
                token,
                &format!("/v1/spaces/{sid}/members"),
                Some(json!({"user_id": user})),
            )
            .await;
        assert!(status.is_success(), "{status} {body}");
    }

    async fn available(&self, token: &str, query: &str) -> (StatusCode, Vec<Value>) {
        let (status, body) = self
            .get(token, &format!("/v1/models/available{query}"))
            .await;
        let models = body["models"].as_array().cloned().unwrap_or_default();
        (status, models)
    }

    async fn decide(
        &self,
        user: &str,
        space: Option<&str>,
        id: &str,
    ) -> Result<Value, ProviderError> {
        providers::decide(
            &self.state,
            user,
            space,
            id,
            json!("Our API is down"),
            json!({"urgent": {"type": "noul", "instructions": "Is this urgent?"}}),
        )
        .await
    }
}

fn ids(models: &[Value]) -> Vec<&str> {
    models
        .iter()
        .map(|m| m["connection_id"].as_str().unwrap())
        .collect()
}

fn assert_no_secret(value: &Value, server: &Server) {
    let text = value.to_string();
    assert!(!text.contains(ALICE_KEY), "key leaked in {text}");
    assert!(
        !text.contains(&server.fixture.base_url),
        "endpoint leaked in {text}"
    );
    assert!(
        !text.contains("has_key") && !text.contains("base_url"),
        "{text}"
    );
}

fn not_found(r: Result<impl std::fmt::Debug, ProviderError>) {
    match r {
        Err(ProviderError::Use(UseError::NotFound)) => {}
        other => panic!("expected not found, got {other:?}"),
    }
}

// ---- Tests ----

#[tokio::test]
async fn sharing_is_owner_only_and_grants_use_never_credentials() {
    let s = Server::start().await;
    let id = s
        .connect(ALICE, "custom-systemone", "jev-latest", true)
        .await;

    // Private by default.
    let (status, share) = s.get(ALICE, &format!("/v1/providers/{id}/sharing")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        share,
        json!({"visibility": "personal", "space_ids": [], "mode": "owner_connection",
               "capability": "system_one"})
    );
    let (_, bob) = s.available(BOB, "").await;
    assert!(!ids(&bob).contains(&id.as_str()));
    not_found(s.decide("bob", None, &id).await);

    // Nobody else can read or change the share, list models with the
    // connection, edit or delete it: an org admin neither.
    for (method, path, body) in [
        (
            reqwest::Method::GET,
            format!("/v1/providers/{id}/sharing"),
            None,
        ),
        (
            reqwest::Method::PUT,
            format!("/v1/providers/{id}/sharing"),
            Some(json!({"visibility": "organization"})),
        ),
        (
            reqwest::Method::POST,
            format!("/v1/providers/{id}/models"),
            Some(json!({})),
        ),
        (
            reqwest::Method::PATCH,
            format!("/v1/providers/{id}"),
            Some(json!({"name": "x"})),
        ),
        (reqwest::Method::DELETE, format!("/v1/providers/{id}"), None),
    ] {
        let (status, body) = s.send(method.clone(), BOB, &path, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}: {body}");
    }

    // Shared with the organization: Bob may use it, and sees no credential.
    let (status, share) = s
        .share(ALICE, &id, json!({"visibility": "organization"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{share}");
    assert_eq!(share["visibility"], "organization");
    let (status, bob) = s.available(BOB, "").await;
    assert_eq!(status, StatusCode::OK);
    let entry = bob
        .iter()
        .find(|m| m["connection_id"] == id.as_str())
        .unwrap();
    assert_eq!(entry["owned"], false);
    assert_eq!(entry["owner_id"], "alice");
    assert_eq!(entry["capability"], "system_one");
    assert_eq!(entry["visibility"], "organization");
    assert_eq!(entry["runs_on"], "owner_connection");
    assert_eq!(entry["billing"], "owner");
    assert_eq!(entry["model"], "jev-latest");
    assert_no_secret(&json!(bob), &s);
    // Bob's own management list never shows it.
    let (_, mine) = s.get(BOB, "/v1/providers").await;
    assert!(!mine.to_string().contains(&id));

    // Borrowed use runs on Alice's endpoint with her key and saved model.
    let answer = s.decide("bob", None, &id).await.unwrap();
    assert_eq!(answer["answers"]["urgent"]["noul"], 0.9);
    let calls = s.fixture.seen("/v1/systemone");
    let last = calls.last().unwrap();
    assert_eq!(
        last.authorization.as_deref(),
        Some(&*format!("Bearer {ALICE_KEY}"))
    );
    assert_eq!(last.body["model"], "jev-latest");
    assert_eq!(last.body["state"], "Our API is down");

    // Revoked: the next call is refused before anything is sent.
    let before = s.fixture.seen("/v1/systemone").len();
    let (status, _) = s.share(ALICE, &id, json!({"visibility": "personal"})).await;
    assert_eq!(status, StatusCode::OK);
    not_found(s.decide("bob", None, &id).await);
    assert_eq!(s.fixture.seen("/v1/systemone").len(), before);
    // The owner still can.
    s.decide("alice", None, &id).await.unwrap();

    // Bad share bodies.
    for bad in [
        json!({"visibility": "organization", "space_ids": ["s-1"]}),
        json!({"visibility": "spaces", "space_ids": []}),
        json!({"visibility": "everyone"}),
        json!({"visibility": "spaces", "space_ids": ["../x"]}),
        json!({"visibility": "personal", "extra": true}),
    ] {
        let (status, body) = s.share(ALICE, &id, bad.clone()).await;
        assert!(status.is_client_error(), "{bad}: {status} {body}");
    }
    // A connection without a model cannot be shared.
    let empty = s.connect(ALICE, "custom-systemone", "", false).await;
    let (status, _) = s
        .share(ALICE, &empty, json!({"visibility": "organization"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn space_shares_need_the_caller_and_the_owner_in_that_space_now() {
    let s = Server::start().await;
    let id = s
        .connect(ALICE, "custom-systemone", "jev-latest", true)
        .await;
    let shared = s.space(ALICE, "Shared", &["bob"]).await;
    let alone = s.space(ALICE, "Alone", &[]).await;
    let carols = s.space(CAROL, "Carol's", &[]).await;

    // Only Spaces the owner is in, reported without telling which.
    let (status, body) = s
        .share(
            ALICE,
            &id,
            json!({"visibility": "spaces", "space_ids": [carols]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, share) = s
        .share(
            ALICE,
            &id,
            json!({"visibility": "spaces", "space_ids": [shared, shared]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{share}");
    assert_eq!(share["space_ids"], json!([shared]));

    // Listed only where it can be used.
    let (_, personal) = s.available(BOB, "").await;
    assert!(!ids(&personal).contains(&id.as_str()));
    let (status, here) = s.available(BOB, &format!("?space_id={shared}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(ids(&here).contains(&id.as_str()));
    assert_eq!(
        here.iter()
            .find(|m| m["connection_id"] == id.as_str())
            .unwrap()["visibility"],
        "spaces"
    );
    let (_, any) = s.available(BOB, "?scope=any").await;
    assert!(ids(&any).contains(&id.as_str()));
    let (status, _) = s.available(BOB, &format!("?space_id={alone}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = s
        .available(BOB, &format!("?space_id={shared}&scope=any"))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = s.available(BOB, "?scope=all").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = s.available(CAROL, &format!("?space_id={shared}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, carol_any) = s.available(CAROL, "?scope=any").await;
    assert!(!ids(&carol_any).contains(&id.as_str()));

    // Runtime checks the actual scope, not the picker's.
    s.decide("bob", Some(&shared), &id).await.unwrap();
    not_found(s.decide("bob", None, &id).await);
    not_found(s.decide("bob", Some(&alone), &id).await);
    not_found(s.decide("carol", Some(&shared), &id).await);
    not_found(s.decide("carol", Some(&carols), &id).await);

    // Removing the borrower ends it at once.
    s.remove_member(ALICE, &shared, "bob").await;
    not_found(s.decide("bob", Some(&shared), &id).await);

    // So does the owner leaving a Space someone else owns.
    let third = s.space(CAROL, "Third", &["alice", "bob"]).await;
    let (status, _) = s
        .share(
            ALICE,
            &id,
            json!({"visibility": "spaces", "space_ids": [third]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    s.decide("bob", Some(&third), &id).await.unwrap();
    s.remove_member(CAROL, &third, "alice").await;
    not_found(s.decide("bob", Some(&third), &id).await);
    let (_, gone) = s.available(BOB, &format!("?space_id={third}")).await;
    assert!(!ids(&gone).contains(&id.as_str()));

    // Deleting the connection drops its share.
    let (status, _) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/providers/{id}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    not_found(s.decide("bob", Some(&third), &id).await);
}

#[tokio::test]
async fn capabilities_are_fixed_and_each_has_its_own_default() {
    let s = Server::start().await;
    let (_, catalog) = s.get(ALICE, "/v1/provider-catalog").await;
    let entry = |id: &str| {
        catalog["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} missing"))
    };
    assert_eq!(entry("systemone")["capability"], "system_one");
    assert_eq!(
        entry("systemone")["default_base_url"],
        "https://api.typesafe.ai/v1"
    );
    assert_eq!(entry("systemone")["requires_key"], true);
    assert_eq!(entry("openai-embeddings")["capability"], "embeddings");
    assert_eq!(entry("custom-systemone")["capability"], "system_one");
    assert_eq!(entry("custom-embeddings")["capability"], "embeddings");
    assert_eq!(entry("device-systemone")["capability"], "system_one");
    assert_eq!(entry("device-embeddings")["capability"], "embeddings");
    assert_eq!(entry("openai")["capability"], "chat");

    let chat = s.connect(ALICE, "custom", "chat-model", false).await;
    let s1 = s
        .connect(ALICE, "custom-systemone", "jev-latest", false)
        .await;
    let emb = s
        .connect(ALICE, "custom-embeddings", "emb-small", false)
        .await;
    let (_, list) = s.get(ALICE, "/v1/providers").await;
    let conns = list["connections"].as_array().unwrap();
    for (id, cap) in [(&chat, "chat"), (&s1, "system_one"), (&emb, "embeddings")] {
        let c = conns.iter().find(|c| c["id"] == id.as_str()).unwrap();
        assert_eq!(c["capability"], cap);
        // The first connection of each capability is its default.
        assert_eq!(c["is_default"], true, "{c}");
    }
    let default = providers::default_model(&s.state, "alice", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(default.connection_id, chat);
    assert_eq!(
        providers::default_connection(&s.state, "alice", providers::Capability::SystemOne)
            .await
            .unwrap(),
        Some(s1.clone())
    );
    // A second System One default replaces the first, not the chat one.
    let s2 = s
        .connect(ALICE, "custom-systemone", "jev-latest", false)
        .await;
    let (status, _) = s
        .patch(
            ALICE,
            &format!("/v1/providers/{s2}"),
            json!({"is_default": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = s.get(ALICE, "/v1/providers").await;
    let default_of = |id: &str| {
        list["connections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap()["is_default"]
            .clone()
    };
    assert_eq!(default_of(&s1), false);
    assert_eq!(default_of(&s2), true);
    assert_eq!(default_of(&chat), true);

    // Never used for another capability.
    let wrong = |r: Result<Value, ProviderError>| {
        assert!(
            matches!(r, Err(ProviderError::Use(UseError::WrongCapability { .. }))),
            "{r:?}"
        )
    };
    wrong(s.decide("alice", None, &chat).await);
    wrong(s.decide("alice", None, &emb).await);
    let r = providers::embed(&s.state, "alice", None, &s1, vec!["x".into()]).await;
    assert!(matches!(
        r,
        Err(ProviderError::Use(UseError::WrongCapability { .. }))
    ));
    let r = providers::connection_model_for(&s.state, "alice", None, &s1).await;
    assert!(matches!(r, Err(UseError::WrongCapability { .. })), "{r:?}");

    // Typed discovery, beyond the chat listing.
    let (status, listed) = s
        .post(ALICE, &format!("/v1/providers/{s1}/models"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["models"], json!(["jev-latest"]));
    let (_, listed) = s
        .post(ALICE, &format!("/v1/providers/{emb}/models"), json!({}))
        .await;
    assert_eq!(listed["models"], json!(["emb-large", "emb-small"]));
    // Keyless endpoints get no Authorization header.
    assert!(
        s.fixture
            .seen("/v1/models")
            .iter()
            .all(|c| c.authorization.is_none())
    );

    // Embeddings come back in input order, with the saved model.
    let vectors = providers::embed(&s.state, "alice", None, &emb, vec!["a".into(), "b".into()])
        .await
        .unwrap();
    assert_eq!(vectors, vec![vec![1.0, 0.5], vec![2.0, 0.5]]);
    let call = s.fixture.seen("/v1/embeddings").pop().unwrap();
    assert_eq!(call.body["model"], "emb-small");
    // Payloads outside the protocol are refused before anything is sent.
    let before = s.fixture.seen("/v1/systemone").len();
    let r = providers::decide(
        &s.state,
        "alice",
        None,
        &s1,
        json!("x"),
        json!({"where": {"type": "box", "instructions": "find it"}}),
    )
    .await;
    assert!(matches!(r, Err(ProviderError::InvalidInput(_))), "{r:?}");
    assert_eq!(s.fixture.seen("/v1/systemone").len(), before);
}

#[tokio::test]
async fn borrowed_chat_rechecks_the_share_before_every_request() {
    let s = Server::start().await;
    let id = s.connect(ALICE, "custom", "alice-model", true).await;
    let (status, _) = s
        .share(ALICE, &id, json!({"visibility": "organization"}))
        .await;
    assert_eq!(status, StatusCode::OK);

    let selected = providers::connection_model_for(&s.state, "carol", None, &id)
        .await
        .unwrap();
    assert_eq!(selected.connection_id, id);
    assert_eq!(selected.model_id, "alice-model");
    let agent = AgentBuilder::new(selected.model).build();
    let reply = agent.prompt("hi").await.unwrap();
    assert!(format!("{reply:?}").contains("hello"), "{reply:?}");
    let calls = s.fixture.seen("/v1/chat/completions");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some(&*format!("Bearer {ALICE_KEY}"))
    );
    assert_eq!(calls[0].body["model"], "alice-model");

    // The same built model stops at its next request once the share ends.
    let (status, _) = s.share(ALICE, &id, json!({"visibility": "personal"})).await;
    assert_eq!(status, StatusCode::OK);
    let err = agent.prompt("again").await.unwrap_err().to_string();
    assert!(err.contains("has ended"), "{err}");
    assert_eq!(s.fixture.seen("/v1/chat/completions").len(), 1);
    // A new run can't even start.
    let r = providers::connection_model_for(&s.state, "carol", None, &id).await;
    assert!(matches!(r, Err(UseError::NotFound)), "{r:?}");
    // The legacy personal entry point follows the same rule.
    let r = providers::connection_model(&s.state, "carol", &id).await;
    assert!(matches!(r, Err(UseError::NotFound)), "{r:?}");
    // The owner's own runs are unaffected.
    let own = providers::connection_model(&s.state, "alice", &id)
        .await
        .unwrap();
    AgentBuilder::new(own.model)
        .build()
        .prompt("mine")
        .await
        .unwrap();
}

#[tokio::test]
async fn subscription_sign_ins_are_shared_only_as_bring_your_own_presets() {
    let s = Server::start().await;
    let id = s
        .connect(ALICE, "device-openai-oauth", "gpt-5.5", false)
        .await;
    let (status, share) = s
        .share(ALICE, &id, json!({"visibility": "organization"}))
        .await;
    assert_eq!(status, StatusCode::OK, "{share}");
    assert_eq!(share["mode"], "preset");

    let (_, bob) = s.available(BOB, "").await;
    let entry = bob
        .iter()
        .find(|m| m["connection_id"] == id.as_str())
        .unwrap();
    assert_eq!(entry["runs_on"], "your_own_sign_in");
    assert_eq!(entry["billing"], "you");
    assert_eq!(entry["ready"], false);

    // Without a sign-in of his own, Bob gets a clear error: never Alice's
    // account.
    let r = providers::connection_model_for(&s.state, "bob", None, &id).await;
    match r {
        Err(e @ UseError::PresetNeedsOwnConnection(_)) => {
            let text = e.to_string();
            assert!(
                text.contains("gpt-5.5") && text.contains("your own"),
                "{text}"
            );
        }
        other => panic!("{other:?}"),
    }

    // With his own sign-in, the preset runs there, with the preset's model.
    let own = s.connect(BOB, "device-openai-oauth", "", false).await;
    let (_, bob) = s.available(BOB, "").await;
    assert_eq!(
        bob.iter()
            .find(|m| m["connection_id"] == id.as_str())
            .unwrap()["ready"],
        true
    );
    let selected = providers::connection_model_for(&s.state, "bob", None, &id)
        .await
        .unwrap();
    assert_eq!(selected.provider_id, "device-openai-oauth");
    assert_eq!(selected.model_id, "gpt-5.5");
    assert_ne!(own, id);
    // Bob's device relay never serves Alice's sign-in: her connection is not
    // usable through the relay by anyone else.
    let r = s
        .state
        .device
        .models_as(
            &s.state.db,
            &metro_server::device_relay::Caller::personal("bob"),
            &id,
        )
        .await;
    assert_eq!(r, Err(metro_server::device_relay::DeviceError::NotFound));
}

#[tokio::test]
async fn fingerprints_follow_model_edits_and_carry_no_secret() {
    let s = Server::start().await;
    let id = s
        .connect(ALICE, "custom-embeddings", "emb-small", true)
        .await;
    let fp = |user: &'static str| {
        let (state, id) = (s.state.clone(), id.clone());
        async move { providers::model_fingerprint(&state, user, None, &id).await }
    };
    let first = fp("alice").await.unwrap();
    assert_eq!(first.len(), 64);
    assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(!first.contains(ALICE_KEY));
    assert_eq!(fp("alice").await.unwrap(), first);
    // A rename or a new key changes nothing the index depends on.
    let (status, _) = s
        .patch(
            ALICE,
            &format!("/v1/providers/{id}"),
            json!({"name": "renamed"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fp("alice").await.unwrap(), first);
    // A model change does, even back and forth.
    for model in ["emb-large", "emb-small"] {
        let (status, _) = s
            .patch(
                ALICE,
                &format!("/v1/providers/{id}"),
                json!({"model": model}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_ne!(fp("alice").await.unwrap(), first);
    }
    // Only for people who may use it.
    not_found(fp("bob").await);
    let (status, _) = s
        .share(ALICE, &id, json!({"visibility": "organization"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(fp("bob").await.is_ok());
    let vectors = providers::embed(&s.state, "bob", None, &id, vec!["q".into()])
        .await
        .unwrap();
    assert_eq!(vectors.len(), 1);
    assert_eq!(
        s.fixture
            .seen("/v1/embeddings")
            .pop()
            .unwrap()
            .authorization
            .as_deref(),
        Some(&*format!("Bearer {ALICE_KEY}"))
    );
}

/// A fake `metro-connector`: claims with the device token and answers a
/// `decide` job.
async fn claim(s: &Server, token: &str) -> Option<Value> {
    let r = s
        .http
        .post(format!("{}/v1/device/claim", s.base))
        .bearer_auth(token)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    match r.status() {
        StatusCode::NO_CONTENT => None,
        StatusCode::OK => Some(r.json().await.unwrap()),
        other => panic!("claim answered {other}"),
    }
}

async fn claim_soon(s: &Server, token: &str) -> Value {
    for _ in 0..200 {
        if let Some(job) = claim(s, token).await {
            return job;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no job was queued");
}

async fn answer(s: &Server, token: &str, job: &Value) -> StatusCode {
    let answers: serde_json::Map<String, Value> = job["request"]["questions"]
        .as_object()
        .unwrap()
        .keys()
        .map(|k| (k.clone(), json!({"type": "noul", "noul": 0.25})))
        .collect();
    s.http
        .post(format!(
            "{}/v1/device/jobs/{}/result",
            s.base,
            job["id"].as_str().unwrap()
        ))
        .bearer_auth(token)
        .json(
            &json!({"ok": true, "response": {"model": "jev-local", "answers": answers,
                       "usage": {"input_tokens": 1, "output_tokens": 1}}}),
        )
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn borrowed_device_work_is_checked_at_dispatch_claim_and_result() {
    let s = Arc::new(Server::start().await);
    let id = s
        .connect(ALICE, "device-systemone", "jev-local", false)
        .await;
    let (status, issued) = s
        .post(
            ALICE,
            &format!("/v1/providers/{id}/device-token"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{issued}");
    let token = issued["token"].as_str().unwrap().to_owned();
    assert!(claim(&s, &token).await.is_none()); // online now
    let space = s.space(ALICE, "Team", &["bob"]).await;

    // Not shared: Bob's call never reaches Alice's device.
    not_found(s.decide("bob", Some(&space), &id).await);
    assert!(claim(&s, &token).await.is_none());

    // Shared with the Space: Bob's job runs on Alice's device, as Bob.
    let (status, _) = s
        .share(
            ALICE,
            &id,
            json!({"visibility": "spaces", "space_ids": [space]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let pending = {
        let (s, id, space) = (s.clone(), id.clone(), space.clone());
        tokio::spawn(async move { s.decide("bob", Some(&space), &id).await })
    };
    let job = claim_soon(&s, &token).await;
    assert_eq!(job["operation"], "decide");
    // The model is the saved one, whatever the caller sent.
    assert_eq!(job["request"]["model"], "jev-local");
    assert!(answer(&s, &token, &job).await.is_success());
    let answered = pending.await.unwrap().unwrap();
    assert_eq!(answered["answers"]["urgent"]["noul"], 0.25);
    // Outside that Space it is still refused.
    not_found(s.decide("bob", None, &id).await);

    // Revoked while claimed: the result is withheld from Bob.
    let pending = {
        let (s, id, space) = (s.clone(), id.clone(), space.clone());
        tokio::spawn(async move { s.decide("bob", Some(&space), &id).await })
    };
    let job = claim_soon(&s, &token).await;
    let (status, _) = s.share(ALICE, &id, json!({"visibility": "personal"})).await;
    assert_eq!(status, StatusCode::OK);
    let _ = answer(&s, &token, &job).await;
    match pending.await.unwrap() {
        Err(ProviderError::Device(metro_server::device_relay::DeviceError::AccessRevoked)) => {}
        other => panic!("expected the borrowed job to be revoked, got {other:?}"),
    }

    // Removed from the Space while queued: the device is never handed the job.
    let (status, _) = s
        .share(
            ALICE,
            &id,
            json!({"visibility": "spaces", "space_ids": [space]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let pending = {
        let (s, id, space) = (s.clone(), id.clone(), space.clone());
        tokio::spawn(async move { s.decide("bob", Some(&space), &id).await })
    };
    // Wait for the job to be queued, then remove Bob before any claim.
    tokio::time::sleep(Duration::from_millis(100)).await;
    s.remove_member(ALICE, &space, "bob").await;
    assert!(
        claim(&s, &token).await.is_none(),
        "a revoked job was handed out"
    );
    assert!(pending.await.unwrap().is_err());

    // The owner's own work is untouched.
    let pending = {
        let (s, id) = (s.clone(), id.clone());
        tokio::spawn(async move { s.decide("alice", None, &id).await })
    };
    let job = claim_soon(&s, &token).await;
    assert!(answer(&s, &token, &job).await.is_success());
    pending.await.unwrap().unwrap();
}

async fn reply_chat(s: &Server, token: &str, job: &Value) -> StatusCode {
    s.http
        .post(format!(
            "{}/v1/device/jobs/{}/result",
            s.base,
            job["id"].as_str().unwrap()
        ))
        .bearer_auth(token)
        .json(&json!({"ok": true, "response": {
            "id": "chatcmpl-d", "object": "chat.completion", "created": 0,
            "model": job["request"]["model"],
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "from the device"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}}))
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn borrowed_device_chat_is_rechecked_between_enqueue_claim_and_result() {
    let s = Arc::new(Server::start().await);
    let id = s
        .connect(ALICE, "device-openai-compatible", "local-model", false)
        .await;
    let (_, issued) = s
        .post(
            ALICE,
            &format!("/v1/providers/{id}/device-token"),
            json!({}),
        )
        .await;
    let token = issued["token"].as_str().unwrap().to_owned();
    assert!(claim(&s, &token).await.is_none());
    // Carol is an ordinary member, not an admin.
    let space = s.space(ALICE, "Team", &["carol"]).await;
    let share = json!({"visibility": "spaces", "space_ids": [space]});
    let (status, _) = s.share(ALICE, &id, share.clone()).await;
    assert_eq!(status, StatusCode::OK);

    // Not usable outside the Space it was shared with.
    let r = providers::connection_model_for(&s.state, "carol", None, &id).await;
    assert!(matches!(r, Err(UseError::NotFound)), "{r:?}");

    let selected = providers::connection_model_for(&s.state, "carol", Some(&space), &id)
        .await
        .unwrap();
    let agent = Arc::new(AgentBuilder::new(selected.model).build());
    let prompt = |text: &'static str| {
        let agent = agent.clone();
        tokio::spawn(async move { agent.prompt(text).await.map(|r| format!("{r:?}")) })
    };

    // Served by Alice's device, with the saved model, as Carol.
    let pending = prompt("first");
    let job = claim_soon(&s, &token).await;
    assert_eq!(job["operation"], "complete");
    assert_eq!(job["request"]["model"], "local-model");
    assert!(reply_chat(&s, &token, &job).await.is_success());
    let out = pending.await.unwrap().unwrap();
    assert!(out.contains("from the device"), "{out}");

    // Revoked between enqueue and claim: the device is never handed it.
    let pending = prompt("second");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (status, _) = s.share(ALICE, &id, json!({"visibility": "personal"})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        claim(&s, &token).await.is_none(),
        "a revoked chat job was handed out"
    );
    assert!(pending.await.unwrap().is_err());

    // Revoked between claim and result: the answer is withheld.
    let (status, _) = s.share(ALICE, &id, share.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let pending = prompt("third");
    let job = claim_soon(&s, &token).await;
    s.remove_member(ALICE, &space, "carol").await;
    let _ = reply_chat(&s, &token, &job).await;
    let err = pending.await.unwrap().unwrap_err().to_string();
    assert!(!err.contains("from the device"), "{err}");

    // A later turn on the same built model is refused before any dispatch.
    let pending = prompt("fourth");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(claim(&s, &token).await.is_none());
    assert!(pending.await.unwrap().is_err());

    // Nothing about the device's pairing reaches Carol.
    let (status, _) = s
        .get(CAROL, &format!("/v1/providers/{id}/device-token"))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = s
        .post(
            CAROL,
            &format!("/v1/providers/{id}/device-token"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn relay_and_grant_errors_map_to_meaningful_statuses() {
    use metro_server::device_relay::DeviceError;
    let status = |e: ProviderError| e.status().as_u16();
    assert_eq!(
        status(ProviderError::Device(DeviceError::AccessRevoked)),
        403
    );
    assert_eq!(
        status(ProviderError::Device(DeviceError::InvalidRequest(
            "x".into()
        ))),
        422
    );
    assert_eq!(status(ProviderError::Device(DeviceError::NotFound)), 404);
    assert_eq!(status(ProviderError::Use(UseError::NotFound)), 404);
    assert_eq!(status(ProviderError::Use(UseError::Revoked)), 403);
    assert_eq!(status(ProviderError::InvalidInput("x".into())), 422);
    assert_eq!(
        status(ProviderError::Use(UseError::WrongCapability {
            expected: providers::Capability::Chat,
            actual: providers::Capability::Embeddings,
        })),
        422
    );
    assert_eq!(
        status(ProviderError::Use(UseError::PresetNeedsOwnConnection(
            "m".into()
        ))),
        409
    );
}

#[tokio::test]
async fn presets_never_bill_an_api_key_and_stop_when_revoked() {
    let s = Arc::new(Server::start().await);
    let preset = s
        .connect(ALICE, "device-openai-oauth", "gpt-5.5", false)
        .await;
    let (status, _) = s
        .share(ALICE, &preset, json!({"visibility": "organization"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let calls = || {
        [
            "/v1/chat/completions",
            "/v1/models",
            "/v1/systemone",
            "/v1/embeddings",
        ]
        .iter()
        .map(|p| s.fixture.seen(p).len())
        .sum::<usize>()
    };

    // Bob has only a healthy, paid OpenAI API connection: the preset fails
    // before any call, and is not "ready" in his picker.
    let (status, body) = s
        .post(
            BOB,
            "/v1/providers",
            json!({"name": "My OpenAI", "provider_id": "openai", "model": "gpt-5.5",
                   "api_key": "sk-bob-paid-key"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let before = calls();
    let r = providers::connection_model_for(&s.state, "bob", None, &preset).await;
    match r {
        Err(e @ UseError::PresetNeedsOwnConnection(_)) => {
            assert!(
                e.to_string()
                    .contains("never switches to an API-billed key")
            )
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(calls(), before, "the preset made a call");
    let (_, bob) = s.available(BOB, "").await;
    let entry = bob
        .iter()
        .find(|m| m["connection_id"] == preset.as_str())
        .unwrap();
    assert_eq!(entry["ready"], false);

    // With his own Sign in with ChatGPT, the preset runs on his device.
    let own = s
        .connect(BOB, "device-openai-oauth", "gpt-5.5", false)
        .await;
    let (_, issued) = s
        .post(BOB, &format!("/v1/providers/{own}/device-token"), json!({}))
        .await;
    let token = issued["token"].as_str().unwrap().to_owned();
    assert!(claim(&s, &token).await.is_none());
    let selected = providers::connection_model_for(&s.state, "bob", None, &preset)
        .await
        .unwrap();
    assert_eq!(selected.provider_id, "device-openai-oauth");
    let agent = Arc::new(AgentBuilder::new(selected.model).build());
    let prompt = |text: &'static str| {
        let agent = agent.clone();
        tokio::spawn(async move { agent.prompt(text).await.map(|r| format!("{r:?}")) })
    };
    let pending = prompt("first");
    let job = claim_soon(&s, &token).await;
    assert_eq!(job["request"]["model"], "gpt-5.5");
    assert!(reply_chat(&s, &token, &job).await.is_success());
    assert!(pending.await.unwrap().unwrap().contains("from the device"));

    // Alice revokes the preset: the next round on the same model stops
    // before reaching Bob's device.
    let (status, _) = s
        .share(ALICE, &preset, json!({"visibility": "personal"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let pending = prompt("second");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        claim(&s, &token).await.is_none(),
        "a revoked preset was sent"
    );
    let err = pending.await.unwrap().unwrap_err().to_string();
    assert!(err.contains("has ended"), "{err}");

    // Revoked while his device answers: the answer is withheld.
    let (status, _) = s
        .share(ALICE, &preset, json!({"visibility": "organization"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let pending = prompt("third");
    let job = claim_soon(&s, &token).await;
    let (status, _) = s
        .share(ALICE, &preset, json!({"visibility": "personal"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let _ = reply_chat(&s, &token, &job).await;
    assert!(pending.await.unwrap().is_err());
    // No request ever went anywhere else.
    assert_eq!(calls(), before);
}

async fn arrived(s: &Server, path: &str, count: usize) {
    for _ in 0..300 {
        if s.fixture.seen(path).len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no request reached {path}");
}

fn revoked<T: std::fmt::Debug>(r: Result<T, ProviderError>) {
    assert!(
        matches!(r, Err(ProviderError::Use(UseError::Revoked))),
        "expected the answer to be withheld, got {r:?}"
    );
}

#[tokio::test]
async fn borrowed_server_typed_answers_are_withheld_after_a_revoke_mid_call() {
    let s = Arc::new(Server::start().await);
    let s1 = s
        .connect(ALICE, "custom-systemone", "jev-latest", true)
        .await;
    let emb = s
        .connect(ALICE, "custom-embeddings", "emb-small", true)
        .await;
    let space = s.space(ALICE, "Team", &["carol"]).await;
    for id in [&s1, &emb] {
        let (status, _) = s
            .share(
                ALICE,
                id,
                json!({"visibility": "spaces", "space_ids": [space]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let decide = |space: String| {
        let (s, id) = (s.clone(), s1.clone());
        tokio::spawn(async move { s.decide("carol", Some(&space), &id).await })
    };
    let embed = |space: String| {
        let (s, id) = (s.clone(), emb.clone());
        tokio::spawn(async move {
            providers::embed(&s.state, "carol", Some(&space), &id, vec!["q".into()]).await
        })
    };

    // Unchanged grant: the held answer is returned once released.
    let hold = s.fixture.gate.write().await;
    let pending = decide(space.clone());
    arrived(&s, "/v1/systemone", 1).await;
    drop(hold);
    pending.await.unwrap().unwrap();

    // Share revoked while the endpoint works: the decision is withheld.
    let hold = s.fixture.gate.write().await;
    let pending = decide(space.clone());
    arrived(&s, "/v1/systemone", 2).await;
    let (status, _) = s.share(ALICE, &s1, json!({"visibility": "personal"})).await;
    assert_eq!(status, StatusCode::OK);
    drop(hold);
    revoked(pending.await.unwrap());

    // The model changed mid-call: the decision is withheld too.
    let (status, _) = s
        .share(
            ALICE,
            &s1,
            json!({"visibility": "spaces", "space_ids": [space]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let hold = s.fixture.gate.write().await;
    let pending = decide(space.clone());
    arrived(&s, "/v1/systemone", 3).await;
    let (status, _) = s
        .patch(
            ALICE,
            &format!("/v1/providers/{s1}"),
            json!({"model": "jev-next"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    drop(hold);
    revoked(pending.await.unwrap());

    // The borrower removed from the Space mid-call: vectors are withheld.
    let hold = s.fixture.gate.write().await;
    let pending = embed(space.clone());
    arrived(&s, "/v1/embeddings", 1).await;
    s.remove_member(ALICE, &space, "carol").await;
    drop(hold);
    revoked(pending.await.unwrap());

    // The owner's own calls are unaffected by any of this.
    s.decide("alice", None, &s1).await.unwrap();
}

/// The first error event after event `after` in `sid`'s lobby, as `token`
/// reads it.
async fn error_after(s: &Server, token: &str, sid: &str, after: i64) -> Value {
    for _ in 0..400 {
        let (_, events) = s
            .get(token, &format!("/v1/spaces/{sid}/rooms/lobby/events"))
            .await;
        if let Some(e) = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"].as_i64().unwrap() > after && e["kind"] == "error")
        {
            return e.clone();
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no error after {after}");
}

/// The run of `token`'s view of `GET /v1/runs` whose task contains `task`.
async fn run_for(s: &Server, token: &str, task: &str) -> Value {
    let (status, runs) = s.get(token, "/v1/runs").await;
    assert_eq!(status, StatusCode::OK, "{runs}");
    runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["task"].as_str().unwrap().contains(task))
        .cloned()
        .unwrap_or_else(|| panic!("no run for {task}: {runs}"))
}

#[tokio::test]
async fn a_share_revoked_mid_run_reads_like_an_upstream_failure_to_the_room() {
    let s = Server::start().await;
    let id = s.connect(ALICE, "custom", "alice-model", true).await;
    let sid = s.space(ALICE, "mid-run", &["bob", "carol"]).await;
    let (status, body) = s
        .share(
            ALICE,
            &id,
            json!({"visibility": "spaces", "space_ids": [sid]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Bob answers with Alice's shared connection in this Space.
    let (status, body) = s
        .put(
            BOB,
            &format!("/v1/spaces/{sid}/prefs"),
            json!({"default_model": {"provider_connection_id": id}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lobby = format!("/v1/spaces/{sid}/rooms/lobby/messages");

    // An upstream failure.
    let (status, failed) = s
        .post(BOB, &lobby, json!({"text": "@agent [fail] first"}))
        .await;
    assert!(status.is_success(), "{failed}");
    let upstream = error_after(&s, CAROL, &sid, failed["id"].as_i64().unwrap()).await;
    let (status, garbled) = s
        .post(BOB, &lobby, json!({"text": "@agent [garbage] third"}))
        .await;
    assert!(status.is_success(), "{garbled}");
    let malformed = error_after(&s, CAROL, &sid, garbled["id"].as_i64().unwrap()).await;

    // The share ends between the run's two model requests.
    let held = s.fixture.gate.write().await;
    let (status, asked) = s
        .post(BOB, &lobby, json!({"text": "@agent [tool] second"}))
        .await;
    assert!(status.is_success(), "{asked}");
    for _ in 0..400 {
        if s.fixture
            .seen("/v1/chat/completions")
            .iter()
            .any(|c| c.body.to_string().contains("[tool] second"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let (status, _) = s.share(ALICE, &id, json!({"visibility": "personal"})).await;
    assert_eq!(status, StatusCode::OK);
    drop(held);
    let revoked = error_after(&s, CAROL, &sid, asked["id"].as_i64().unwrap()).await;
    let requests = s
        .fixture
        .seen("/v1/chat/completions")
        .iter()
        .filter(|c| c.body.to_string().contains("[tool] second"))
        .count();
    assert_eq!(requests, 1, "the request after the revoke was never sent");

    // Carol (and Alice, the Space owner) can't tell the three apart: every
    // error event and its fields read the same.
    let fields = |e: &Value| (e["kind"].clone(), e["actor"].clone(), e["text"].clone());
    assert_eq!(fields(&upstream), fields(&revoked));
    assert_eq!(fields(&upstream), fields(&malformed));
    assert_eq!(
        upstream["text"],
        "The agent stopped: the sender's model failed or is no longer available to this run."
    );
    for token in [CAROL, ALICE] {
        let (_, events) = s
            .get(token, &format!("/v1/spaces/{sid}/rooms/lobby/events"))
            .await;
        let errors: Vec<&Value> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "error")
            .collect();
        assert_eq!(errors.len(), 3, "{token}");
        assert!(
            errors.iter().all(|e| e["text"] == upstream["text"]),
            "{token}"
        );
        let transcript = events.to_string();
        for leak in [
            "has ended",
            SENTINEL,
            ALICE_KEY,
            s.fixture.base_url.as_str(),
            "not_allowed",
            "model_error",
        ] {
            assert!(!transcript.contains(leak), "{token}: {leak}");
        }
        // Their run views carry no reason.
        for task in ["[fail] first", "[tool] second", "[garbage] third"] {
            let run = run_for(&s, token, task).await;
            assert_eq!(run["status"], "failed", "{run}");
            assert!(run.get("failure").is_none(), "{token}: {run}");
            let (status, one) = s
                .get(token, &format!("/v1/runs/{}", run["id"].as_str().unwrap()))
                .await;
            assert_eq!(status, StatusCode::OK);
            assert!(one.get("failure").is_none(), "{token}: {one}");
        }
    }

    // Alice owns the Space but started none of these runs: her monitoring
    // view has one public code for all three.
    let (status, monitoring) = s
        .get(ALICE, &format!("/v1/monitoring/events?space_id={sid}"))
        .await;
    assert_eq!(status, StatusCode::OK, "{monitoring}");
    let codes: Vec<(String, String)> = monitoring["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["component"].as_str().unwrap().to_owned(),
                e["code"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(codes, vec![("provider".to_owned(), "failed".to_owned()); 3]);

    // Bob, who started the runs, learns each reason privately: Metro's own
    // refusal, or the provider failure's category, never its text.
    for (task, category) in [
        ("[fail] first", "server error"),
        ("[garbage] third", "reply"),
    ] {
        let run = run_for(&s, BOB, task).await;
        assert_eq!(run["failure"]["reason"], "model_error", "{run}");
        let message = run["failure"]["message"].as_str().unwrap();
        assert!(message.contains(category), "{message}");
        for leak in [SENTINEL, ALICE_KEY, s.fixture.base_url.as_str()] {
            assert!(!message.contains(leak), "{message}");
        }
    }
    let run = run_for(&s, BOB, "[tool] second").await;
    assert_eq!(run["failure"]["reason"], "not_allowed", "{run}");
    let message = run["failure"]["message"].as_str().unwrap();
    assert!(message.starts_with("The agent stopped."), "{message}");
    assert!(
        message.contains("default model for this Space"),
        "{message}"
    );
    let (status, one) = s
        .get(BOB, &format!("/v1/runs/{}", run["id"].as_str().unwrap()))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["failure"], run["failure"]);
    assert!(!message.contains(ALICE_KEY));

    // Someone outside the Space can't see the run at all.
    s.remove_member(ALICE, &sid, "carol").await;
    let (status, _) = s
        .get(CAROL, &format!("/v1/runs/{}", run["id"].as_str().unwrap()))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_borrowers_chat_never_shows_the_owners_provider_text() {
    let s = Server::start().await;
    let id = s.connect(ALICE, "custom", "alice-model", true).await;
    let (status, _) = s
        .share(ALICE, &id, json!({"visibility": "organization"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = s
        .put(
            CAROL,
            "/v1/model-routing",
            json!({"enabled": true, "default_chat_id": id}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let ask = |token: &'static str| {
        let s = &s;
        async move {
            let (status, sent) = s
                .post(
                    token,
                    "/v1/chat/messages",
                    json!({"text": "@agent [fail] hi"}),
                )
                .await;
            assert!(status.is_success(), "{sent}");
            for _ in 0..400 {
                let (_, events) = s.get(token, "/v1/chat/events").await;
                if let Some(e) = events["events"].as_array().unwrap().iter().find(|e| {
                    e["id"].as_i64().unwrap() > sent["id"].as_i64().unwrap() && e["kind"] == "error"
                }) {
                    return e["text"].as_str().unwrap().to_owned();
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            panic!("no error");
        }
    };
    // Neither the borrower's nor the owner's own Chat shows the provider's
    // text (which echoes the key): its category and status only.
    let carol = ask(CAROL).await;
    assert_eq!(
        carol,
        "The agent failed. The model service reported a server error. (HTTP 500)"
    );
    let (status, body) = s
        .put(
            ALICE,
            "/v1/model-routing",
            json!({"enabled": true, "default_chat_id": id}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let alice = ask(ALICE).await;
    assert_eq!(alice, carol);
    for text in [&carol, &alice] {
        for leak in [SENTINEL, ALICE_KEY, s.fixture.base_url.as_str()] {
            assert!(!text.contains(leak), "{text}");
        }
    }
}

/// Everything Metro logs while it is alive, on this thread.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_device_classifiers_own_text_reaches_no_log_response_or_room() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(tracing_subscriber::EnvFilter::new("metro_server=trace"))
        .with_writer(move || writer.clone())
        .finish();
    let _logs = tracing::subscriber::set_default(subscriber);

    let s = Arc::new(Server::start().await);
    let id = s
        .connect(ALICE, "device-systemone", "jev-local", false)
        .await;
    let (status, issued) = s
        .post(
            ALICE,
            &format!("/v1/providers/{id}/device-token"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{issued}");
    let token = issued["token"].as_str().unwrap().to_owned();
    assert!(claim(&s, &token).await.is_none()); // online now
    let sid = s.space(ALICE, "Device classifier", &["bob"]).await;
    let (status, body) = s
        .put(ALICE, "/v1/model-routing", json!({"classifier_id": id}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = s
        .put(
            ALICE,
            &format!("/v1/spaces/{sid}/prefs"),
            json!({"classify_unaddressed": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The device fails with its own text, then answers malformed with it.
    let results = [
        json!({"ok": false, "error": format!("{SENTINEL}: vendor said no")}),
        json!({"ok": true, "response": {"model": SENTINEL, "answers": {SENTINEL: SENTINEL}}}),
    ];
    for (n, result) in results.into_iter().enumerate() {
        let sent = {
            let (s, sid) = (s.clone(), sid.clone());
            tokio::spawn(async move {
                s.post(
                    ALICE,
                    &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
                    json!({"text": format!("lunch plans {n}?")}),
                )
                .await
            })
        };
        let job = claim_soon(&s, &token).await;
        s.http
            .post(format!(
                "{}/v1/device/jobs/{}/result",
                s.base,
                job["id"].as_str().unwrap()
            ))
            .bearer_auth(&token)
            .json(&result)
            .send()
            .await
            .unwrap();
        let (status, response) = sent.await.unwrap();
        assert_eq!(status, StatusCode::ACCEPTED, "{response}");
        let expected = ["unreachable", "invalid_answer"][n];
        assert_eq!(
            response["addressing_note"],
            json!({"kind": "classification_failed", "reason": expected})
        );
        assert!(!response.to_string().contains(SENTINEL), "{response}");
    }
    for token in [ALICE, BOB] {
        let (_, events) = s
            .get(token, &format!("/v1/spaces/{sid}/rooms/lobby/events"))
            .await;
        assert!(!events.to_string().contains(SENTINEL), "{events}");
    }
    let logs = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("typed model call failed"), "{logs}");
    assert!(logs.contains("device_error"), "{logs}");
    assert!(
        !logs.contains(SENTINEL),
        "a log line carries the device's text:\n{logs}"
    );
}

#[tokio::test]
async fn a_member_removed_mid_run_stops_its_next_request() {
    // The operator's model serves anyone without a saved default (Carol).
    let s = Server::start_with(true).await;
    // Bob's own default, and an organization-wide share of Alice's: neither
    // is tied to a Space, and neither run picks an explicit model.
    s.connect(BOB, "custom", "bob-model", true).await;
    let shared = s.connect(ALICE, "custom", "alice-model", true).await;
    let (status, _) = s
        .share(ALICE, &shared, json!({"visibility": "organization"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    for (label, sender, selection) in [
        ("own", BOB, None),
        ("shared", BOB, Some(shared.clone())),
        ("operator", CAROL, None),
    ] {
        let user = sender.trim_end_matches("-token");
        let sid = s.space(ALICE, &format!("removal {label}"), &[user]).await;
        if let Some(id) = selection {
            let (status, body) = s
                .put(
                    sender,
                    &format!("/v1/spaces/{sid}/prefs"),
                    json!({"default_model": {"provider_connection_id": id}}),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
        }
        let text = format!("@agent [tool] {label} run");
        let requests = || {
            s.fixture
                .seen("/v1/chat/completions")
                .iter()
                .filter(|c| c.body.to_string().contains(&text))
                .count()
        };
        let held = s.fixture.gate.write().await;
        let (status, sent) = s
            .post(
                sender,
                &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
                json!({"text": text}),
            )
            .await;
        assert!(status.is_success(), "{sent}");
        for _ in 0..400 {
            if requests() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(requests(), 1, "{label}: the first request arrived");
        if label == "operator" {
            let first = s.fixture.seen("/v1/chat/completions");
            let first = first
                .iter()
                .find(|c| c.body.to_string().contains(&text))
                .unwrap();
            assert_eq!(
                first.body["model"], "operator-model",
                "{label}: the operator's model"
            );
        }
        // The membership ends between the run's two requests. (Removal also
        // stops the person's runs, but only after it commits: this is that gap.)
        s.state.db.remove_member(&sid, user).unwrap();
        drop(held);
        let mut ended = false;
        for _ in 0..400 {
            if s.state
                .runs
                .list()
                .iter()
                .any(|r| r.space_id == sid && r.status.is_terminal())
            {
                ended = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ended, "{label}: the run ended");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(requests(), 1, "{label}: no request after the removal");
    }
}

/// Alice's run in `sid` for `text`, held at its first model request while
/// `between` runs, then released; her private failure for it.
async fn failure_after_change<F, Fut>(s: &Server, sid: &str, text: &str, between: F) -> Value
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let requests = || {
        s.fixture
            .seen("/v1/chat/completions")
            .iter()
            .filter(|c| c.body.to_string().contains(text))
            .count()
    };
    let held = s.fixture.gate.write().await;
    let (status, sent) = s
        .post(
            ALICE,
            &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            json!({"text": text}),
        )
        .await;
    assert!(status.is_success(), "{sent}");
    for _ in 0..400 {
        if requests() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(requests(), 1, "the first request arrived");
    between().await;
    drop(held);
    for _ in 0..400 {
        let (_, runs) = s.get(ALICE, "/v1/runs").await;
        if let Some(run) = runs["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["task"].as_str().unwrap().contains(text) && r["status"] == "failed")
        {
            assert_eq!(requests(), 1, "no request after the change");
            return run["failure"].clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the run did not fail");
}

#[tokio::test]
async fn an_explicit_models_changed_connection_is_reported_as_changed() {
    let s = Server::start().await;
    let id = s.connect(ALICE, "custom", "alice-model", true).await;
    let sid = s.space(ALICE, "explicit", &[]).await;
    // An explicit model (with its model id) as Alice's default in the Space.
    let (status, body) = s
        .put(
            ALICE,
            &format!("/v1/spaces/{sid}/prefs"),
            json!({"default_model": {"provider_connection_id": id, "model_id": "alice-model"}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let failure = failure_after_change(&s, &sid, "@agent [tool] explicit", || async {
        // A new key changes the connection the selection was checked against.
        let (status, body) = s
            .patch(
                ALICE,
                &format!("/v1/providers/{id}"),
                json!({"api_key": "sk-alice-rotated-0123456789"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    })
    .await;
    assert_eq!(failure["reason"], "connection_changed", "{failure}");
}

#[tokio::test]
async fn a_default_replaced_mid_run_is_reported_as_changed() {
    let s = Server::start().await;
    // Two connections at the same revision: only identity tells them apart.
    let first = s.connect(ALICE, "custom", "alice-model", true).await;
    let second = s.connect(ALICE, "custom", "alice-model-2", true).await;
    let sid = s.space(ALICE, "replaced", &[]).await;
    let failure = failure_after_change(&s, &sid, "@agent [tool] replaced", || async {
        // The default is deleted; the other connection becomes the default.
        let (status, body) = s
            .send(
                reqwest::Method::DELETE,
                ALICE,
                &format!("/v1/providers/{first}"),
                None,
            )
            .await;
        assert!(status.is_success(), "{body}");
    })
    .await;
    assert_eq!(failure["reason"], "connection_changed", "{failure}");
    assert!(
        failure["message"]
            .as_str()
            .unwrap()
            .contains("changed or was replaced"),
        "{failure}"
    );
    let _ = second;
}

/// A `device-systemone` connection of `token`'s person, paired and online:
/// its id and device token.
async fn online_device(s: &Server, token: &str) -> (String, String) {
    let id = s
        .connect(token, "device-systemone", "jev-local", false)
        .await;
    let (status, issued) = s
        .post(
            token,
            &format!("/v1/providers/{id}/device-token"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{issued}");
    let device = issued["token"].as_str().unwrap().to_owned();
    assert!(claim(s, &device).await.is_none()); // online now
    (id, device)
}

#[tokio::test]
async fn space_work_queued_for_a_device_is_withheld_after_the_caller_leaves() {
    let s = Arc::new(Server::start().await);
    // Bob's own device, and Alice's shared with the whole organization.
    let (own, own_device) = online_device(&s, BOB).await;
    let (shared, shared_device) = online_device(&s, ALICE).await;
    let (status, _) = s
        .share(ALICE, &shared, json!({"visibility": "organization"}))
        .await;
    assert_eq!(status, StatusCode::OK);
    for (label, id, device) in [("own", own, own_device), ("shared", shared, shared_device)] {
        let sid = s.space(ALICE, &format!("leaving {label}"), &["bob"]).await;
        let pending = {
            let (s, sid, id) = (s.clone(), sid.clone(), id.clone());
            tokio::spawn(async move { s.decide("bob", Some(&sid), &id).await })
        };
        // Queued (admitted while Bob was a member), not yet claimed.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !pending.is_finished(),
            "{label}: still waiting for the device"
        );
        s.state.db.remove_member(&sid, "bob").unwrap();
        // The device never receives the Space's payload.
        assert!(
            claim(&s, &device).await.is_none(),
            "{label}: released after removal"
        );
        let r = pending.await.unwrap();
        assert!(r.is_err(), "{label}: {r:?}");
    }
}
