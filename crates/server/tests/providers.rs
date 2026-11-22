//! Personal model connections over real HTTP, against loopback fixture
//! endpoints only: the catalog, private CRUD with write-only keys, exact
//! base-URL allowlisting, bounded model listing without redirects, and the
//! rule that every run (personal Chat, Space rooms and their threads) uses
//! the default connection of the person who started it, with the
//! environment model only for people without a default.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::http::{HeaderMap, StatusCode as AxumStatus};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use base64::Engine;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::providers::ProviderService;
use metro_server::providers::secret::MasterKey;
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";

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
    ])))
}

fn master(byte: u8) -> MasterKey {
    MasterKey::parse(&base64::engine::general_purpose::STANDARD.encode([byte; 32])).unwrap()
}

// ---- Fixture model endpoint ----

#[derive(Clone, Copy, PartialEq)]
enum Listing {
    Ok,
    Missing,
    Redirect,
}

#[derive(Clone, Debug)]
struct Seen {
    method: &'static str,
    path: String,
    authorization: Option<String>,
    body: Value,
}

struct Fixture {
    base_url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Fixture {
    async fn start(models: &[&str], listing: Listing, reply: &str) -> Self {
        let seen = Arc::new(Mutex::new(Vec::<Seen>::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let models: Vec<String> = models.iter().map(|m| m.to_string()).collect();
        let reply = reply.to_owned();
        let auth = |h: &HeaderMap| {
            h.get("authorization")
                .map(|v| v.to_str().unwrap().to_owned())
        };
        let (s1, s2, s3) = (seen.clone(), seen.clone(), seen.clone());
        let app = axum::Router::new()
            .route(
                "/v1/models",
                get(move |headers: HeaderMap| {
                    let (seen, models) = (s1.clone(), models.clone());
                    async move {
                        seen.lock().unwrap().push(Seen {
                            method: "GET",
                            path: "/v1/models".into(),
                            authorization: auth(&headers),
                            body: Value::Null,
                        });
                        match listing {
                            Listing::Ok => Json(json!({
                                "object": "list",
                                "data": models.iter().map(|m| json!({"id": m, "object": "model"})).collect::<Vec<_>>(),
                            }))
                            .into_response(),
                            Listing::Missing => (AxumStatus::NOT_FOUND, Json(json!({"error": {"message": "no listing"}}))).into_response(),
                            Listing::Redirect => (
                                AxumStatus::TEMPORARY_REDIRECT,
                                [("location", format!("http://{addr}/elsewhere/models"))],
                            )
                                .into_response(),
                        }
                    }
                }),
            )
            .route(
                "/elsewhere/models",
                get(move |headers: HeaderMap| {
                    let seen = s3.clone();
                    async move {
                        seen.lock().unwrap().push(Seen {
                            method: "GET",
                            path: "/elsewhere/models".into(),
                            authorization: auth(&headers),
                            body: Value::Null,
                        });
                        Json(json!({"object": "list", "data": [{"id": "redirected"}]}))
                    }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    let (seen, reply) = (s2.clone(), reply.clone());
                    async move {
                        let model = body["model"].clone();
                        seen.lock().unwrap().push(Seen {
                            method: "POST",
                            path: "/v1/chat/completions".into(),
                            authorization: auth(&headers),
                            body,
                        });
                        Json(json!({
                            "id": "chatcmpl-1", "object": "chat.completion", "created": 0,
                            "model": model,
                            "choices": [{"index": 0, "finish_reason": "stop",
                                         "message": {"role": "assistant", "content": reply}}],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        }))
                    }
                }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base_url: format!("http://{addr}/v1"),
            seen,
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn chats(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.method == "POST")
            .collect()
    }

    fn env_config(&self) -> ModelConfig {
        ModelConfig {
            base_url: self.base_url.clone(),
            model: "env-model".into(),
            api_key: "operator-env-key".into(),
        }
    }
}

// ---- Server ----

struct Server {
    base: String,
    http: reqwest::Client,
    state: AppState,
}

impl Server {
    async fn start(
        model: Option<ModelConfig>,
        db: Db,
        master: Option<MasterKey>,
        allowed: Vec<String>,
    ) -> Self {
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model,
            limits: Limits {
                agent_timeout: Duration::from_secs(20),
                ..Limits::default()
            },
            org_admins: Vec::new(),
        };
        let mut state = AppState::new(config, db, auth()).await.unwrap();
        state.providers = ProviderService::new(master, allowed).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
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
    async fn patch(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::PATCH, token, path, Some(body))
            .await
    }
    async fn delete(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.send(reqwest::Method::DELETE, token, path, None).await
    }

    async fn connect(&self, token: &str, body: Value) -> Value {
        let (status, c) = self.post(token, "/v1/providers", body).await;
        assert_eq!(status, StatusCode::CREATED, "{c}");
        c
    }

    async fn connections(&self, token: &str) -> Vec<Value> {
        let (status, body) = self.get(token, "/v1/providers").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["connections"].as_array().unwrap().clone()
    }

    async fn chat_and_wait(&self, token: &str, text: &str) -> Value {
        let (status, body) = self
            .post(
                token,
                "/v1/chat/messages",
                json!({"text": text, "invoke_agent": true}),
            )
            .await;
        assert!(status.is_success(), "{status} {body}");
        for _ in 0..400 {
            let (_, events) = self.get(token, "/v1/chat/events").await;
            let events = events["events"].as_array().unwrap().clone();
            let asked = events.iter().rposition(|e| e["text"] == text);
            if let Some(answer) = asked.and_then(|i| {
                events[i + 1..]
                    .iter()
                    .find(|e| ["agent", "error"].contains(&e["kind"].as_str().unwrap_or("")))
            }) {
                return answer.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no answer to {text}");
    }
}

fn assert_no_secret(value: &Value, secret: &str) {
    assert!(
        !value.to_string().contains(secret),
        "secret leaked in {value}"
    );
}

// ---- Catalog ----

#[tokio::test]
async fn the_catalog_needs_a_login_and_contacts_nobody() {
    let fixture = Fixture::start(&["m"], Listing::Ok, "hi").await;
    let s = Server::start(None, Db::open_in_memory().unwrap(), None, vec![]).await;
    let (status, _) = s.get("nobody", "/v1/provider-catalog").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, catalog) = s.get(ALICE, "/v1/provider-catalog").await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = catalog["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    for want in ["openai", "anthropic", "gcp.gemini", "device-claude-code"] {
        assert!(ids.contains(&want), "{want} missing: {ids:?}");
    }
    // No allowlist: nothing that needs an operator-approved endpoint.
    assert!(!ids.contains(&"custom") && !ids.contains(&"ollama"));
    assert_eq!(catalog["allowed_base_urls"], json!([]));

    let s = Server::start(
        None,
        Db::open_in_memory().unwrap(),
        None,
        vec![fixture.base_url.clone()],
    )
    .await;
    let (_, catalog) = s.get(BOB, "/v1/provider-catalog").await;
    assert_eq!(catalog["allowed_base_urls"], json!([fixture.base_url]));
    let custom = catalog["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "custom")
        .unwrap();
    assert_eq!(custom["allow_custom_url"], true);
    assert_eq!(custom["requires_key"], false);
    assert!(
        fixture.seen().is_empty(),
        "reading the catalog contacted an endpoint"
    );
}

// ---- Connections ----

#[tokio::test]
async fn connections_are_private_and_keys_are_write_only() {
    let fixture = Fixture::start(&["fixture/alice"], Listing::Ok, "hi").await;
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("metro.sqlite3");
    let s = Server::start(
        None,
        Db::open(&db_path).unwrap(),
        Some(master(1)),
        vec![fixture.base_url.clone()],
    )
    .await;
    let secret = "sk-alice-very-secret-0123456789";
    let c = s
        .connect(
            ALICE,
            json!({"name": "Mine", "provider_id": "custom", "base_url": fixture.base_url, "api_key": secret}),
        )
        .await;
    assert_no_secret(&c, secret);
    assert_eq!(c["has_key"], true);
    assert_eq!(c["model"], "");
    assert_eq!(c["is_default"], false, "no model, so not a default");
    assert_eq!(c["execution"], "server");
    assert!(c.get("api_key").is_none());
    let id = c["id"].as_str().unwrap();

    assert_eq!(s.connections(ALICE).await.len(), 1);
    assert_no_secret(&json!(s.connections(ALICE).await), secret);
    assert!(s.connections(BOB).await.is_empty());
    let path = format!("/v1/providers/{id}");
    assert_eq!(
        s.patch(BOB, &path, json!({"name": "x"})).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(s.delete(BOB, &path).await.0, StatusCode::NOT_FOUND);
    let (status, body) = s.post(BOB, &format!("{path}/models"), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "no such model connection"}));
    assert_eq!(
        s.patch(
            ALICE,
            "/v1/providers/pc_ffffffffffffffffffffffffffffffff",
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert!(
        fixture.seen().is_empty(),
        "saving or foreign requests contacted the endpoint"
    );

    // Typos never pass silently as "saved".
    let (status, _) = s
        .post(ALICE, "/v1/providers", json!({"name": "t", "provider_id": "custom", "base_url": fixture.base_url, "apiKey": "k"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Rename leaves the key; an omitted key is unchanged.
    let (status, renamed) = s.patch(ALICE, &path, json!({"name": "Renamed"})).await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["has_key"], true);
    // Replace the key, then list with it.
    let replacement = "sk-alice-replacement-key-98765";
    let (status, _) = s.patch(ALICE, &path, json!({"api_key": replacement})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, models) = s.post(ALICE, &format!("{path}/models"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{models}");
    assert_eq!(models, json!({"models": ["fixture/alice"]}));
    let listing = fixture.seen();
    assert_eq!(listing.len(), 1);
    assert_eq!(
        listing[0].authorization.as_deref(),
        Some(&*format!("Bearer {replacement}"))
    );

    // Neither key is in the database file in plaintext.
    for file in ["metro.sqlite3", "metro.sqlite3-wal"] {
        if let Ok(bytes) = std::fs::read(dir.path().join(file)) {
            for k in [secret, replacement] {
                assert!(
                    !bytes.windows(k.len()).any(|w| w == k.as_bytes()),
                    "{k} in {file}"
                );
            }
        }
    }

    assert_eq!(s.delete(ALICE, &path).await.0, StatusCode::NO_CONTENT);
    assert!(s.connections(ALICE).await.is_empty());
    assert_eq!(
        s.post(ALICE, &format!("{path}/models"), json!({})).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn base_urls_must_match_the_allowlist_exactly() {
    let fixture = Fixture::start(&[], Listing::Ok, "hi").await;
    let s = Server::start(
        None,
        Db::open_in_memory().unwrap(),
        Some(master(1)),
        vec![fixture.base_url.clone()],
    )
    .await;
    for (body, label) in [
        (
            json!({"name": "x", "provider_id": "custom", "base_url": format!("{}/other", fixture.base_url)}),
            "other path",
        ),
        (
            json!({"name": "x", "provider_id": "custom", "base_url": "http://169.254.169.254/v1"}),
            "metadata host",
        ),
        (
            json!({"name": "x", "provider_id": "custom"}),
            "custom without a base_url",
        ),
        (
            json!({"name": "x", "provider_id": "openai", "base_url": "https://evil.example/v1", "api_key": "k"}),
            "override not allowed",
        ),
        (
            json!({"name": "x", "provider_id": "openai"}),
            "openai needs a key",
        ),
        (
            json!({"name": "x", "provider_id": "no-such-provider"}),
            "unknown provider",
        ),
        (
            json!({"name": "x", "provider_id": "chatgpt", "api_key": "k"}),
            "not offered",
        ),
        (
            json!({"name": "x", "provider_id": "device-claude-code", "api_key": "k"}),
            "device with a key",
        ),
        (
            json!({"name": "", "provider_id": "custom", "base_url": fixture.base_url}),
            "blank name",
        ),
    ] {
        let (status, body) = s.post(ALICE, "/v1/providers", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {body}");
    }
    // A trailing slash is the same URL.
    s.connect(ALICE, json!({"name": "ok", "provider_id": "custom", "base_url": format!("{}/", fixture.base_url)})).await;
    // A registered provider may use an approved override.
    let c = s
        .connect(ALICE, json!({"name": "ollama", "provider_id": "ollama", "base_url": fixture.base_url, "model": "llama3"}))
        .await;
    assert_eq!(c["base_url"], fixture.base_url);
    // A public provider uses its own HTTPS endpoint; nothing is contacted.
    let c = s
        .connect(
            ALICE,
            json!({"name": "oa", "provider_id": "openai", "api_key": "sk-x"}),
        )
        .await;
    assert!(c.get("base_url").is_none());
    assert!(fixture.seen().is_empty());
}

#[tokio::test]
async fn keys_need_a_master_key_and_keyless_endpoints_send_no_auth() {
    let fixture = Fixture::start(&["local-a", "local-b", "local-a"], Listing::Ok, "hi").await;
    let s = Server::start(
        None,
        Db::open_in_memory().unwrap(),
        None,
        vec![fixture.base_url.clone()],
    )
    .await;
    let (status, body) = s
        .post(ALICE, "/v1/providers", json!({"name": "k", "provider_id": "custom", "base_url": fixture.base_url, "api_key": "sk-1"}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body["error"].as_str().unwrap().contains("master key"),
        "{body}"
    );
    let c = s
        .connect(
            ALICE,
            json!({"name": "local", "provider_id": "custom", "base_url": fixture.base_url}),
        )
        .await;
    assert_eq!(c["has_key"], false);
    let id = c["id"].as_str().unwrap();
    let (status, models) = s
        .post(ALICE, &format!("/v1/providers/{id}/models"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models, json!({"models": ["local-a", "local-b"]}));
    assert_eq!(
        fixture.seen()[0].authorization,
        None,
        "a keyless listing sent credentials"
    );
    let (status, _) = s
        .post(
            ALICE,
            &format!("/v1/providers/{id}/models"),
            json!({"x": 1}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn listing_failures_are_clear_and_redirects_are_not_followed() {
    let missing = Fixture::start(&[], Listing::Missing, "hi").await;
    let redirect = Fixture::start(&["x"], Listing::Redirect, "hi").await;
    let s = Server::start(
        None,
        Db::open_in_memory().unwrap(),
        Some(master(1)),
        vec![missing.base_url.clone(), redirect.base_url.clone()],
    )
    .await;
    let a = s
        .connect(
            ALICE,
            json!({"name": "m", "provider_id": "custom", "base_url": missing.base_url}),
        )
        .await;
    let (status, body) = s
        .post(
            ALICE,
            &format!("/v1/providers/{}/models", a["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("model id yourself")
    );
    // The model can still be entered by hand and made the default.
    let (status, c) = s
        .patch(
            ALICE,
            &format!("/v1/providers/{}", a["id"].as_str().unwrap()),
            json!({"model": "typed-by-hand", "is_default": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{c}");
    assert_eq!(
        (c["model"].clone(), c["is_default"].clone()),
        (json!("typed-by-hand"), json!(true))
    );

    let key = "sk-must-not-follow-redirects";
    let b = s
        .connect(ALICE, json!({"name": "r", "provider_id": "custom", "base_url": redirect.base_url, "api_key": key}))
        .await;
    let (status, body) = s
        .post(
            ALICE,
            &format!("/v1/providers/{}/models", b["id"].as_str().unwrap()),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_no_secret(&body, key);
    let seen = redirect.seen();
    assert_eq!(seen.len(), 1, "the redirect was followed: {seen:?}");
    assert_eq!(seen[0].path, "/v1/models");
}

#[tokio::test]
async fn defaults_need_a_model_and_there_is_at_most_one() {
    let fixture = Fixture::start(&[], Listing::Ok, "hi").await;
    let s = Server::start(
        None,
        Db::open_in_memory().unwrap(),
        None,
        vec![fixture.base_url.clone()],
    )
    .await;
    let new = |name: &str, model: Option<&str>| {
        let mut body = json!({"name": name, "provider_id": "custom", "base_url": fixture.base_url});
        if let Some(m) = model {
            body["model"] = json!(m);
        }
        body
    };
    let (status, _) = s
        .post(ALICE, "/v1/providers", json!({"name": "d", "provider_id": "custom", "base_url": fixture.base_url, "is_default": true}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "a default without a model");
    let blank = s.connect(ALICE, new("blank", None)).await;
    assert_eq!(blank["is_default"], false);
    let blank_path = format!("/v1/providers/{}", blank["id"].as_str().unwrap());
    assert_eq!(
        s.patch(ALICE, &blank_path, json!({"is_default": true}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    // The first connection created with a model becomes the default.
    let first = s.connect(ALICE, new("first", Some("m1"))).await;
    assert_eq!(first["is_default"], true);
    let second = s.connect(ALICE, new("second", Some("m2"))).await;
    assert_eq!(second["is_default"], false);
    // Choosing another default moves it.
    let (status, _) = s
        .patch(
            ALICE,
            &format!("/v1/providers/{}", second["id"].as_str().unwrap()),
            json!({"is_default": true}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let defaults: Vec<String> = s
        .connections(ALICE)
        .await
        .iter()
        .filter(|c| c["is_default"] == true)
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(defaults, ["second"]);
    // A default's model cannot be cleared.
    assert_eq!(
        s.patch(
            ALICE,
            &format!("/v1/providers/{}", second["id"].as_str().unwrap()),
            json!({"model": null})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // Bob's first connection is his own default, independent of Alice's.
    let bobs = s.connect(BOB, new("bob", Some("mb"))).await;
    assert_eq!(bobs["is_default"], true);
}

// ---- Runs use the initiator's connection ----

#[tokio::test]
async fn personal_chat_uses_the_senders_default_or_the_environment_model() {
    let env = Fixture::start(&[], Listing::Ok, "from env").await;
    let alice_model = Fixture::start(&[], Listing::Ok, "from alice's connection").await;
    let s = Server::start(
        Some(env.env_config()),
        Db::open_in_memory().unwrap(),
        Some(master(2)),
        vec![alice_model.base_url.clone()],
    )
    .await;
    let key = "sk-alice-chat-key-5555";
    let c = s
        .connect(ALICE, json!({"name": "a", "provider_id": "custom", "base_url": alice_model.base_url, "api_key": key, "model": "alice-model"}))
        .await;
    assert_eq!(c["is_default"], true);

    let answer = s.chat_and_wait(ALICE, "hello from alice").await;
    assert_eq!(answer["kind"], "agent", "{answer}");
    assert_eq!(answer["text"], "from alice's connection");
    let chats = alice_model.chats();
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].body["model"], "alice-model");
    assert_eq!(
        chats[0].authorization.as_deref(),
        Some(&*format!("Bearer {key}"))
    );
    assert!(
        env.chats().is_empty(),
        "alice's run used the environment model"
    );

    // Bob has no default: the environment model, with the operator's key.
    let answer = s.chat_and_wait(BOB, "hello from bob").await;
    assert_eq!(answer["text"], "from env");
    let env_chats = env.chats();
    assert_eq!(env_chats.len(), 1);
    assert_eq!(
        env_chats[0].authorization.as_deref(),
        Some("Bearer operator-env-key")
    );
    assert!(!env_chats[0].body.to_string().contains("hello from alice"));
    assert_eq!(
        alice_model.chats().len(),
        1,
        "bob's run used alice's connection"
    );

    // Deleting her default puts Alice back on the environment model.
    s.delete(
        ALICE,
        &format!("/v1/providers/{}", c["id"].as_str().unwrap()),
    )
    .await;
    let answer = s.chat_and_wait(ALICE, "again").await;
    assert_eq!(answer["text"], "from env");
}

#[tokio::test]
async fn an_unusable_default_fails_without_falling_back() {
    let env = Fixture::start(&[], Listing::Ok, "from env").await;
    let upstream = Fixture::start(&[], Listing::Ok, "from connection").await;
    let db = Db::open_in_memory().unwrap();
    let s = Server::start(
        Some(env.env_config()),
        db.clone(),
        Some(master(3)),
        vec![upstream.base_url.clone()],
    )
    .await;
    s.connect(ALICE, json!({"name": "a", "provider_id": "custom", "base_url": upstream.base_url, "api_key": "sk-old-master", "model": "m"}))
        .await;
    s.connect(
        BOB,
        json!({"name": "b", "provider_id": "device-claude-code", "model": "claude-x"}),
    )
    .await;

    // The operator replaced the master key: the saved key no longer opens.
    let s = Server::start(
        Some(env.env_config()),
        db.clone(),
        Some(master(4)),
        vec![upstream.base_url.clone()],
    )
    .await;
    let answer = s.chat_and_wait(ALICE, "rotated").await;
    assert_eq!(answer["kind"], "error", "{answer}");
    assert!(
        answer["text"].as_str().unwrap().contains("decrypted"),
        "{answer}"
    );
    // Or removed it.
    let s = Server::start(
        Some(env.env_config()),
        db.clone(),
        None,
        vec![upstream.base_url.clone()],
    )
    .await;
    let answer = s.chat_and_wait(ALICE, "no master").await;
    assert_eq!(answer["kind"], "error");
    assert!(
        answer["text"].as_str().unwrap().contains("master key"),
        "{answer}"
    );
    // Or took the endpoint off the allowlist.
    let s = Server::start(Some(env.env_config()), db.clone(), Some(master(3)), vec![]).await;
    let answer = s.chat_and_wait(ALICE, "not approved").await;
    assert_eq!(answer["kind"], "error");
    assert!(
        answer["text"].as_str().unwrap().contains("approved"),
        "{answer}"
    );
    // Bob's device is not connected.
    let answer = s.chat_and_wait(BOB, "device offline").await;
    assert_eq!(answer["kind"], "error", "{answer}");

    assert!(
        env.chats().is_empty(),
        "a failed default fell back to the environment model"
    );
    assert!(upstream.chats().is_empty());
}

#[tokio::test]
async fn space_runs_and_their_threads_use_the_initiators_connection() {
    // The parent asks for a thread once; the child just answers.
    let script = |name: &'static str| {
        move |body: &Value| -> Value {
            let tools: Vec<String> = body["tools"]
                .as_array()
                .map(|t| {
                    t.iter()
                        .map(|t| t["function"]["name"].as_str().unwrap().to_owned())
                        .collect()
                })
                .unwrap_or_default();
            let parent = tools.iter().any(|t| t == "thread_create");
            let replied = body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["role"] == "tool");
            if parent && !replied {
                json!({"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "thread_create",
                                 "arguments": json!({"name": format!("{name}-thread"), "task": "child task"}).to_string()}}]})
            } else {
                json!({"role": "assistant", "content": format!("{name} done")})
            }
        }
    };
    let alice_model = ScriptedModel::start(script("alice")).await;
    let bob_model = ScriptedModel::start(script("bob")).await;
    let env = Fixture::start(&[], Listing::Ok, "from env").await;
    let s = Server::start(
        Some(env.env_config()),
        Db::open_in_memory().unwrap(),
        Some(master(5)),
        vec![alice_model.base_url.clone(), bob_model.base_url.clone()],
    )
    .await;
    s.connect(ALICE, json!({"name": "a", "provider_id": "custom", "base_url": alice_model.base_url, "api_key": "sk-alice-space", "model": "am"})).await;
    s.connect(BOB, json!({"name": "b", "provider_id": "custom", "base_url": bob_model.base_url, "api_key": "sk-bob-space", "model": "bm"})).await;
    let (status, space) = s.post(ALICE, "/v1/spaces", json!({"name": "shared"})).await;
    assert_eq!(status, StatusCode::CREATED);
    let sid = space["id"].as_str().unwrap().to_owned();
    let (status, _) = s
        .post(
            ALICE,
            &format!("/v1/spaces/{sid}/members"),
            json!({"user_id": "bob"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // Bob asks in Alice's Space: Bob's connection for the run and its thread.
    let (status, body) = s
        .post(
            BOB,
            &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            json!({"text": "bob asks", "invoke_agent": true}),
        )
        .await;
    assert!(status.is_success(), "{body}");
    wait_until(|| bob_model.requests().len() >= 3).await;
    let bob_requests = bob_model.requests();
    assert!(
        bob_requests
            .iter()
            .all(|(auth, _)| auth.as_deref() == Some("Bearer sk-bob-space"))
    );
    assert!(
        bob_requests
            .iter()
            .any(|(_, b)| b.to_string().contains("child task")),
        "the thread did not use bob's model"
    );
    assert!(
        alice_model.requests().is_empty(),
        "bob's run used alice's key"
    );

    // Alice asks: Alice's connection, never Bob's.
    s.post(
        ALICE,
        &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
        json!({"text": "alice asks", "invoke_agent": true}),
    )
    .await;
    wait_until(|| alice_model.requests().len() >= 3).await;
    assert!(
        alice_model
            .requests()
            .iter()
            .all(|(auth, _)| auth.as_deref() == Some("Bearer sk-alice-space"))
    );
    assert_eq!(bob_model.requests().len(), bob_requests.len());
    assert!(env.chats().is_empty());
}

async fn wait_until(done: impl Fn() -> bool) {
    for _ in 0..600 {
        if done() {
            // Let any in-flight request land.
            tokio::time::sleep(Duration::from_millis(100)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out");
}

type Requests = Arc<Mutex<Vec<(Option<String>, Value)>>>;

struct ScriptedModel {
    base_url: String,
    requests: Requests,
}

impl ScriptedModel {
    async fn start(script: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self {
        let requests: Requests = Arc::default();
        let seen = requests.clone();
        let script = Arc::new(script);
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let (seen, script) = (seen.clone(), script.clone());
                async move {
                    let auth = headers
                        .get("authorization")
                        .map(|v| v.to_str().unwrap().to_owned());
                    let message = script(&body);
                    seen.lock().unwrap().push((auth, body));
                    let finish = if message.get("tool_calls").is_some() {
                        "tool_calls"
                    } else {
                        "stop"
                    };
                    Json(json!({
                        "id": "c", "object": "chat.completion", "created": 0, "model": "m",
                        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { base_url, requests }
    }

    fn requests(&self) -> Vec<(Option<String>, Value)> {
        self.requests.lock().unwrap().clone()
    }
}

// ---- Native Anthropic Messages and Gemini wires ----

type Native = Arc<Mutex<Vec<(String, String, HeaderMap, Value)>>>;

/// One loopback server speaking just enough of Anthropic's and Gemini's
/// native APIs: model listings and one text reply each.
async fn native_fixture() -> (String, Native) {
    let seen: Native = Arc::default();
    let log = seen.clone();
    let app = axum::Router::new().fallback(
        move |method: axum::http::Method, uri: axum::http::Uri, headers: HeaderMap, body: axum::body::Bytes| {
            let log = log.clone();
            async move {
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let path = uri.path().to_owned();
                log.lock().unwrap().push((method.to_string(), uri.to_string(), headers, body));
                let reply = match (method.as_str(), path.as_str()) {
                    ("GET", "/v1/models") => json!({
                        "data": [{"id": "claude-x", "type": "model", "display_name": "Claude X",
                                  "created_at": "2025-01-01T00:00:00Z"}],
                        "has_more": false, "first_id": "claude-x", "last_id": "claude-x"}),
                    ("GET", "/v1beta/models") => json!({
                        "models": [{"name": "models/gemini-x", "displayName": "Gemini X"}]}),
                    ("POST", "/v1/messages") => json!({
                        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-x",
                        "content": [{"type": "text", "text": "from anthropic"}],
                        "stop_reason": "end_turn", "stop_sequence": null,
                        "usage": {"input_tokens": 1, "output_tokens": 1}}),
                    ("POST", "/v1beta/models/gemini-x:generateContent") => json!({
                        "candidates": [{"content": {"role": "model", "parts": [{"text": "from gemini"}]},
                                        "finishReason": "STOP", "index": 0}],
                        "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1, "totalTokenCount": 2}}),
                    _ => return (AxumStatus::NOT_FOUND, Json(json!({"error": "unexpected"}))).into_response(),
                };
                Json(reply).into_response()
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

#[tokio::test]
async fn anthropic_and_gemini_connections_use_their_native_wires() {
    let (base, seen) = native_fixture().await;
    let env = Fixture::start(&[], Listing::Ok, "from env").await;
    let s = Server::start(
        Some(env.env_config()),
        Db::open_in_memory().unwrap(),
        Some(master(6)),
        vec![base.clone()],
    )
    .await;
    let a = s
        .connect(ALICE, json!({"name": "claude", "provider_id": "anthropic", "base_url": base, "api_key": "sk-ant-alice"}))
        .await;
    let g = s
        .connect(BOB, json!({"name": "gemini", "provider_id": "gcp.gemini", "base_url": base, "api_key": "gm-bob-key"}))
        .await;
    let list = |c: &Value| format!("/v1/providers/{}/models", c["id"].as_str().unwrap());
    assert_eq!(
        s.post(ALICE, &list(&a), json!({})).await.1,
        json!({"models": ["claude-x"]})
    );
    assert_eq!(
        s.post(BOB, &list(&g), json!({})).await.1,
        json!({"models": ["gemini-x"]})
    );
    for (token, c, model) in [(ALICE, &a, "claude-x"), (BOB, &g, "gemini-x")] {
        let (status, _) = s
            .patch(
                token,
                &format!("/v1/providers/{}", c["id"].as_str().unwrap()),
                json!({"model": model, "is_default": true}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(
        s.chat_and_wait(ALICE, "ask claude").await["text"],
        "from anthropic"
    );
    assert_eq!(
        s.chat_and_wait(BOB, "ask gemini").await["text"],
        "from gemini"
    );
    assert!(env.chats().is_empty());

    let seen = seen.lock().unwrap().clone();
    let header = |h: &HeaderMap, name: &str| h.get(name).map(|v| v.to_str().unwrap().to_owned());
    for (method, uri, headers, body) in &seen {
        let anthropic = uri.starts_with("/v1/");
        if anthropic {
            assert_eq!(
                header(headers, "x-api-key").as_deref(),
                Some("sk-ant-alice"),
                "{method} {uri}"
            );
            assert!(header(headers, "x-goog-api-key").is_none());
        } else {
            assert_eq!(
                header(headers, "x-goog-api-key").as_deref(),
                Some("gm-bob-key"),
                "{method} {uri}"
            );
            assert!(header(headers, "x-api-key").is_none());
            assert!(
                !uri.contains("gm-bob-key"),
                "the Gemini key was put in the URL"
            );
        }
        if method == "POST" {
            let text = body.to_string();
            assert!(
                text.contains(if anthropic {
                    "ask claude"
                } else {
                    "ask gemini"
                }),
                "{text}"
            );
        }
    }
    let posts: Vec<&str> = seen
        .iter()
        .filter(|s| s.0 == "POST")
        .map(|s| s.1.as_str())
        .collect();
    assert_eq!(
        posts,
        ["/v1/messages", "/v1beta/models/gemini-x:generateContent"]
    );
}

#[tokio::test]
async fn an_explicitly_chosen_connection_must_be_the_senders_own() {
    use metro_server::agent::respond_personal_using;
    use metro_server::db::{Addressed, Author};
    let alice_model = Fixture::start(&[], Listing::Ok, "alice's model").await;
    let bob_model = Fixture::start(&[], Listing::Ok, "bob's second model").await;
    let env = Fixture::start(&[], Listing::Ok, "from env").await;
    let s = Server::start(
        Some(env.env_config()),
        Db::open_in_memory().unwrap(),
        Some(master(7)),
        vec![alice_model.base_url.clone(), bob_model.base_url.clone()],
    )
    .await;
    let alices = s
        .connect(ALICE, json!({"name": "a", "provider_id": "custom", "base_url": alice_model.base_url, "api_key": "sk-alice-only", "model": "am"}))
        .await;
    s.connect(BOB, json!({"name": "b1", "provider_id": "custom", "base_url": bob_model.base_url, "model": "default-model"}))
        .await;
    let second = s
        .connect(BOB, json!({"name": "b2", "provider_id": "custom", "base_url": bob_model.base_url, "model": "second-model"}))
        .await;
    assert_eq!(second["is_default"], false);

    let ask = |text: &str, connection: &Value| {
        let (state, text, id) = (
            s.state.clone(),
            text.to_owned(),
            connection["id"].as_str().unwrap().to_owned(),
        );
        async move {
            let bob = Author {
                id: Some("bob".into()),
                name: "bob".into(),
            };
            let trigger = state
                .db
                .call(move |db| {
                    db.append_personal("bob", "message", &bob, &text, &Addressed::room())
                })
                .await
                .unwrap();
            respond_personal_using(state, "bob".into(), trigger, Some(id)).await;
        }
    };
    ask("use alice's", &alices).await;
    ask("use my second", &second).await;
    let (_, events) = s.get(BOB, "/v1/chat/events").await;
    let replies: Vec<(String, String)> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] != "message" && e["kind"] != "activity")
        .map(|e| {
            (
                e["kind"].as_str().unwrap().to_owned(),
                e["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(replies.len(), 2, "{replies:?}");
    // The refused choice never started a turn; the used one did, and ended.
    let phases: Vec<&str> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "activity")
        .map(|e| e["activity"]["phase"].as_str().unwrap())
        .collect();
    assert_eq!(phases, ["started", "completed"]);
    assert_eq!(replies[0].0, "error");
    assert!(
        replies[0].1.contains("no model connection with that id"),
        "{replies:?}"
    );
    assert_eq!(
        replies[1],
        ("agent".to_owned(), "bob's second model".to_owned())
    );
    assert!(
        alice_model.chats().is_empty(),
        "bob used alice's connection"
    );
    assert!(
        env.chats().is_empty(),
        "a refused choice fell back to the environment model"
    );
    assert_eq!(bob_model.chats()[0].body["model"], "second-model");
}
