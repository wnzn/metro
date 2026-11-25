//! Personal Chat `provider_connection_id` over real HTTP: absent keeps the
//! sender's default, present uses exactly that connection of the sender's,
//! and a foreign, missing or malformed choice is refused before anything is
//! stored or any model is called. Nothing falls back.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::http::HeaderMap;
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
const OVERRIDE_KEY: &str = "sk-override-secret-1234";

struct FakeAuth;

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            let id = match bearer {
                ALICE => "alice",
                BOB => "bob",
                _ => return Err(AuthFailure::Rejected("unknown token".to_owned())),
            };
            Ok(UserIdentity {
                id: id.to_owned(),
                actor: id.to_owned(),
            })
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

/// An OpenAI-compatible endpoint that always answers `reply` and records
/// the `Authorization` header of each chat call.
struct Endpoint {
    base_url: String,
    calls: Arc<Mutex<Vec<Option<String>>>>,
    model_ids: Arc<Mutex<Vec<String>>>,
    listings: Arc<AtomicUsize>,
}

impl Endpoint {
    async fn start(reply: &'static str) -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let model_ids = Arc::new(Mutex::new(Vec::new()));
        let ids = model_ids.clone();
        let listings = Arc::new(AtomicUsize::new(0));
        let listed = listings.clone();
        let app = axum::Router::new()
            .route(
                "/v1/chat/completions",
                post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    let seen = seen.clone();
                    let ids = ids.clone();
                    async move {
                        ids.lock()
                            .unwrap()
                            .push(body["model"].as_str().unwrap().to_owned());
                        seen.lock().unwrap().push(
                            headers
                                .get("authorization")
                                .map(|v| v.to_str().unwrap().to_owned()),
                        );
                        Json(json!({
                            "id": "c", "object": "chat.completion", "created": 0,
                            "model": body["model"],
                            "choices": [{"index": 0, "finish_reason": "stop",
                                         "message": {"role": "assistant", "content": reply}}],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        }))
                    }
                }),
            )
            .route(
                "/v1/models",
                get(move || {
                    let listed = listed.clone();
                    async move {
                        listed.fetch_add(1, Ordering::Relaxed);
                        Json(json!({"object":"list","data":[{"id":"m-other","object":"model"}]}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base_url: format!("http://{addr}/v1"),
            calls,
            model_ids,
            listings,
        }
    }

    fn calls(&self) -> Vec<Option<String>> {
        self.calls.lock().unwrap().clone()
    }
}

struct Server {
    base: String,
    http: reqwest::Client,
}

impl Server {
    async fn start(env: &Endpoint, allowed: Vec<String>) -> Self {
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            // A server model exists, so a silent fallback would be visible.
            model: Some(ModelConfig {
                base_url: env.base_url.clone(),
                model: "env-model".into(),
                api_key: String::new(),
            }),
            limits: Limits {
                agent_timeout: Duration::from_secs(20),
                ..Limits::default()
            },
            org_admins: Vec::new(),
        };
        let mut state = AppState::new(config, Db::open_in_memory().unwrap(), Arc::new(FakeAuth))
            .await
            .unwrap();
        let master =
            MasterKey::parse(&base64::engine::general_purpose::STANDARD.encode([9u8; 32])).unwrap();
        state.providers = ProviderService::new(Some(master), allowed).unwrap();
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
        method: reqwest::Method,
        token: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, String) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let r = req.send().await.unwrap();
        (r.status(), r.text().await.unwrap())
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let (status, text) = self
            .send(reqwest::Method::POST, token, path, Some(body))
            .await;
        assert!(!text.contains(OVERRIDE_KEY), "key echoed: {text}");
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// A ready connection to `base_url`, optionally the default; its id.
    async fn connection(
        &self,
        token: &str,
        base_url: &str,
        key: Option<&str>,
        default: bool,
    ) -> String {
        let mut body = json!({"name": "c", "provider_id": "custom", "base_url": base_url});
        if let Some(key) = key {
            body["api_key"] = json!(key);
        }
        let (status, c) = self.post(token, "/v1/providers", body).await;
        assert_eq!(status, StatusCode::CREATED, "{c}");
        let id = c["id"].as_str().unwrap().to_owned();
        let (status, text) = self
            .send(
                reqwest::Method::PATCH,
                token,
                &format!("/v1/providers/{id}"),
                Some(json!({"model": "m", "is_default": default})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(!text.contains(OVERRIDE_KEY));
        id
    }

    async fn events(&self, token: &str) -> Vec<Value> {
        let (_, text) = self
            .send(reqwest::Method::GET, token, "/v1/chat/events", None)
            .await;
        assert!(!text.contains(OVERRIDE_KEY));
        serde_json::from_str::<Value>(&text).unwrap()["events"]
            .as_array()
            .unwrap()
            .clone()
    }

    /// Send `body` to personal Chat and wait for the agent's answer (or error).
    async fn ask(&self, token: &str, body: Value) -> Value {
        let text = body["text"].clone();
        let (status, sent) = self.post(token, "/v1/chat/messages", body).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{sent}");
        for _ in 0..400 {
            let events = self.events(token).await;
            if let Some(i) = events.iter().rposition(|e| e["text"] == text)
                && let Some(answer) = events[i + 1..]
                    .iter()
                    .find(|e| e["kind"] == "agent" || e["kind"] == "error")
            {
                return answer.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no answer to {text}");
    }
}

#[tokio::test]
async fn chat_uses_the_chosen_connection_or_the_default_never_another() {
    let env = Endpoint::start("from env").await;
    let default = Endpoint::start("from default").await;
    let chosen = Endpoint::start("from override").await;
    let s = Server::start(
        &env,
        vec![default.base_url.clone(), chosen.base_url.clone()],
    )
    .await;
    let _alice_default = s.connection(ALICE, &default.base_url, None, true).await;
    let alice_override = s
        .connection(ALICE, &chosen.base_url, Some(OVERRIDE_KEY), false)
        .await;
    let bobs = s.connection(BOB, &chosen.base_url, None, true).await;

    // Absent: the sender's default, as before.
    let answer = s
        .ask(ALICE, json!({"text": "plain", "invoke_agent": true}))
        .await;
    assert_eq!(answer["text"], "from default", "{answer}");
    assert_eq!(default.calls().len(), 1);
    assert!(chosen.calls().is_empty());

    // Present: exactly that connection, with its own key.
    let answer = s
        .ask(
            ALICE,
            json!({"text": "@agent chosen", "provider_connection_id": alice_override}),
        )
        .await;
    assert_eq!(answer["kind"], "agent", "{answer}");
    assert_eq!(answer["text"], "from override");
    assert_eq!(default.calls().len(), 1, "the default was not used");
    assert_eq!(chosen.calls(), vec![Some(format!("Bearer {OVERRIDE_KEY}"))]);

    let (status, catalog) = s
        .send(reqwest::Method::GET, ALICE, "/v1/models/available", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!catalog.contains(OVERRIDE_KEY) && !catalog.contains(&chosen.base_url));
    let catalog: Value = serde_json::from_str(&catalog).unwrap();
    let offered = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["connection_id"] == alice_override)
        .unwrap();
    assert_eq!(
        offered["chat_models"],
        json!([{"id": "m", "display_name": "m", "reasoning_efforts": [], "is_default": true, "available": true}])
    );
    let answer = s
        .ask(
            ALICE,
            json!({
                "text":"explicit pinned model","invoke_agent":true,
                "provider_connection_id":alice_override,"model_id":"m"
            }),
        )
        .await;
    assert_eq!(answer["text"], "from override");
    assert_eq!(chosen.calls().len(), 2);

    // Refused before anything is stored or any model is called.
    let before = s.events(ALICE).await.len();
    let missing = format!("pc_{}", "0".repeat(32));
    let refusals = [
        (
            json!({"text": "@agent x", "provider_connection_id": bobs}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"text": "@agent x", "provider_connection_id": missing}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"text": "@agent x", "provider_connection_id": ""}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text": "@agent x", "provider_connection_id": "   "}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text": "@agent x", "provider_connection_id": "pc_".to_owned() + &"a".repeat(5000)}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text": "@agent x", "provider_connection_id": 7}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        // A choice on a message the agent will not answer is not ignored.
        (
            json!({"text": "just a note", "provider_connection_id": alice_override, "invoke_agent": false}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text": "@agent x", "provider_connection_id": bobs, "model_id": "m"}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"text": "@agent x", "provider_connection_id": alice_override, "model_id": "m-other"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"text": "@agent x", "provider_connection_id": alice_override, "reasoning_effort": "high"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ];
    for (body, want) in refusals {
        let (status, reply) = s.post(ALICE, "/v1/chat/messages", body.clone()).await;
        assert_eq!(status, want, "{body} -> {reply}");
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.events(ALICE).await.len(), before, "nothing stored");
    assert_eq!(default.calls().len(), 1);
    assert_eq!(chosen.calls().len(), 2);
    assert!(
        env.calls().is_empty(),
        "never fell back to the server model"
    );

    assert_eq!(chosen.listings.load(Ordering::Relaxed), 0);
    let list_path = format!("/v1/providers/{alice_override}/models");
    assert_eq!(
        s.post(BOB, &list_path, json!({})).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, listing) = s.post(ALICE, &list_path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{listing}");
    assert_eq!(listing["models"], json!(["m-other"]));
    let (status, catalog) = s
        .send(reqwest::Method::GET, ALICE, "/v1/models/available", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let catalog: Value = serde_json::from_str(&catalog).unwrap();
    let offered = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["connection_id"] == alice_override)
        .unwrap();
    assert_eq!(
        offered["chat_models"],
        json!([
            {"id":"m","display_name":"m","reasoning_efforts":[],"is_default":true,"available":true},
            {"id":"m-other","display_name":"m-other","reasoning_efforts":[],"is_default":false,"available":true}
        ])
    );
    let answer = s
        .ask(
            ALICE,
            json!({
                "text":"listed alt","invoke_agent":true,
                "provider_connection_id":alice_override,"model_id":"m-other"
            }),
        )
        .await;
    assert_eq!(answer["text"], "from override");
    assert_eq!(*chosen.model_ids.lock().unwrap(), ["m", "m", "m-other"]);
    assert_eq!(
        chosen.listings.load(Ordering::Relaxed),
        1,
        "only explicit listing reaches server"
    );
}
