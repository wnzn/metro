//! Each connection's own model list over real HTTP, against loopback
//! fixtures only: models added by hand with names and thinking options,
//! listed models added enabled (opt-out) and marked unavailable when they
//! vanish, the connection's default model kept enabled, per-turn model and
//! effort checked against the list and sent on the model's own wire,
//! automatic defaults, and the grouped picker with the operator's model.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::routing::{get, post};
use base64::Engine;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::providers::ProviderService;
use metro_server::providers::secret::MasterKey;
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";

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

/// An OpenAI-compatible endpoint: records chat bodies and lists `listed`.
struct Endpoint {
    base_url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
    listed: Arc<Mutex<Vec<Value>>>,
}

impl Endpoint {
    async fn start() -> Self {
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let listed = Arc::new(Mutex::new(Vec::<Value>::new()));
        let (b, l) = (bodies.clone(), listed.clone());
        let app = axum::Router::new()
            .route(
                "/v1/chat/completions",
                post(move |Json(body): Json<Value>| {
                    let b = b.clone();
                    async move {
                        b.lock().unwrap().push(body.clone());
                        Json(json!({
                            "id": "c", "object": "chat.completion", "created": 0,
                            "model": body["model"],
                            "choices": [{"index": 0, "finish_reason": "stop",
                                         "message": {"role": "assistant", "content": "ok"}}],
                            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                        }))
                    }
                }),
            )
            .route(
                "/v1/models",
                get(move || {
                    let l = l.clone();
                    async move { Json(json!({"object": "list", "data": l.lock().unwrap().clone()})) }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base_url: format!("http://{addr}/v1"),
            bodies,
            listed,
        }
    }

    fn list(&self, models: Value) {
        *self.listed.lock().unwrap() = models.as_array().unwrap().clone();
    }

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }
}

struct Server {
    base: String,
    http: reqwest::Client,
}

impl Server {
    async fn start(allowed: Vec<String>, operator: Option<&str>) -> Self {
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: operator.map(|base| ModelConfig {
                base_url: base.to_owned(),
                model: "operator-model".into(),
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
            MasterKey::parse(&base64::engine::general_purpose::STANDARD.encode([5u8; 32])).unwrap();
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
        method: Method,
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
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::POST, token, path, Some(body)).await
    }

    async fn patch(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::PATCH, token, path, Some(body)).await
    }

    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.send(Method::GET, token, path, None).await
    }

    /// Send to personal Chat and wait for the agent's answer (or error).
    async fn ask(&self, token: &str, body: Value) -> Value {
        let text = body["text"].clone();
        let (status, sent) = self.post(token, "/v1/chat/messages", body).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{sent}");
        for _ in 0..400 {
            let events = self.get(token, "/v1/chat/events").await.1["events"]
                .as_array()
                .unwrap()
                .clone();
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

fn ids(list: &Value) -> Vec<String> {
    list["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["model_id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn custom_models_with_names_and_thinking_drive_each_turn() {
    let ep = Endpoint::start().await;
    let s = Server::start(vec![ep.base_url.clone()], None).await;
    let (status, c) = s
        .post(
            ALICE,
            "/v1/providers",
            json!({
                "name": "Local", "provider_id": "custom", "base_url": ep.base_url,
                "models": [
                    {"model_id": "qwen3:8b", "display_name": "Qwen 3 8B",
                     "thinking": {"efforts": ["low", "high"], "wire": "reasoning_effort"}},
                    {"model_id": "deep", "thinking": {"efforts": ["max"], "wire": "reasoning"}},
                    {"model_id": "plain"}
                ]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{c}");
    // The first model and, as the first chat connection, the default.
    assert_eq!(c["model"], "qwen3:8b");
    assert_eq!(c["is_default"], true);
    assert_eq!(c["custom_thinking"], true);
    assert_eq!(c["models"][0]["display_name"], "Qwen 3 8B");
    assert_eq!(c["models"][0]["is_default"], true);
    assert_eq!(c["models"][0]["reasoning_efforts"], json!(["low", "high"]));
    assert_eq!(c["models"][2]["display_name"], "plain");
    let id = c["id"].as_str().unwrap().to_owned();
    let list = format!("/v1/providers/{id}/model-list");

    // Bad thinking options and a duplicate are refused.
    for bad in [
        json!({"model_id": "x", "thinking": {"efforts": ["Low"], "wire": "reasoning"}}),
        json!({"model_id": "x", "thinking": {"efforts": ["low"], "wire": "none"}}),
        json!({"model_id": "x", "thinking": {"efforts": ["low"], "wire": "header"}}),
        json!({"model_id": "x y"}),
    ] {
        let (status, _) = s.post(ALICE, &list, bad.clone()).await;
        assert!(status.is_client_error(), "{bad} -> {status}");
    }
    assert_eq!(
        s.post(ALICE, &list, json!({"model_id": "plain"})).await.0,
        StatusCode::CONFLICT
    );
    // Only the owner sees or changes the list.
    assert_eq!(s.get(BOB, &list).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        s.post(BOB, &list, json!({"model_id": "z"})).await.0,
        StatusCode::NOT_FOUND
    );

    // Each turn's effort goes on the chosen model's own wire.
    for (model, effort, expect) in [
        (
            "qwen3:8b",
            Some("high"),
            json!({"reasoning_effort": "high"}),
        ),
        ("deep", Some("max"), json!({"reasoning": {"effort": "max"}})),
        ("plain", None, json!({})),
    ] {
        let mut body = json!({"text": format!("turn {model}"), "invoke_agent": true,
                              "provider_connection_id": id, "model_id": model});
        if let Some(e) = effort {
            body["reasoning_effort"] = json!(e);
        }
        let answer = s.ask(ALICE, body).await;
        assert_eq!(answer["kind"], "agent", "{answer}");
        let sent = ep.bodies().last().unwrap().clone();
        assert_eq!(sent["model"], model);
        match model {
            "qwen3:8b" => {
                assert_eq!(sent["reasoning_effort"], expect["reasoning_effort"]);
                assert!(sent.get("reasoning").is_none());
            }
            "deep" => {
                assert_eq!(sent["reasoning"], expect["reasoning"]);
                assert!(sent.get("reasoning_effort").is_none());
            }
            _ => assert!(sent.get("reasoning_effort").is_none() && sent.get("reasoning").is_none()),
        }
    }
    // An effort the model doesn't offer, a model not on the list, or a
    // disabled one is refused before anything is stored.
    let n = ep.bodies().len();
    for (model, effort) in [
        ("plain", Some("low")),
        ("qwen3:8b", Some("max")),
        ("other", None),
    ] {
        let mut body = json!({"text": "no", "invoke_agent": true,
                              "provider_connection_id": id, "model_id": model});
        if let Some(e) = effort {
            body["reasoning_effort"] = json!(e);
        }
        assert_eq!(
            s.post(ALICE, "/v1/chat/messages", body).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let (status, v) = s
        .patch(ALICE, &list, json!({"model_id": "deep", "enabled": false}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        s.post(ALICE, "/v1/chat/messages",
            json!({"text": "off", "invoke_agent": true, "provider_connection_id": id, "model_id": "deep"}))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(ep.bodies().len(), n);

    // The default model can't be disabled or removed; another becomes it.
    assert_eq!(
        s.patch(
            ALICE,
            &list,
            json!({"model_id": "qwen3:8b", "enabled": false})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        s.send(
            Method::DELETE,
            ALICE,
            &format!("{list}?model_id=qwen3:8b"),
            None
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        s.patch(
            ALICE,
            &list,
            json!({"model_id": "deep", "default": true, "enabled": false})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, v) = s
        .patch(
            ALICE,
            &list,
            json!({"model_id": "plain", "default": true, "display_name": "Plain one"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["default_model"], "plain");
    let (status, v) = s
        .send(
            Method::DELETE,
            ALICE,
            &format!("{list}?model_id=qwen3:8b"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(ids(&v), ["deep", "plain"]);
    // Rename back to the id.
    let (_, v) = s
        .patch(
            ALICE,
            &list,
            json!({"model_id": "plain", "display_name": null}),
        )
        .await;
    assert_eq!(v["models"][1]["display_name"], "plain");
    assert_eq!(v["models"][1]["custom_name"], Value::Null);
}

#[tokio::test]
async fn listed_models_are_opt_out_and_vanished_ones_stay_unavailable() {
    let ep = Endpoint::start().await;
    ep.list(json!([{"id": "a", "name": "Model A"}, {"id": "b"}, {"id": "whisper-1"}]));
    let s = Server::start(vec![ep.base_url.clone()], None).await;
    let (status, c) = s
        .post(
            ALICE,
            "/v1/providers",
            json!({"name": "Gw", "provider_id": "custom", "base_url": ep.base_url,
                   "fetch_models": true}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{c}");
    // Every listed chat model is added enabled; the first is the default.
    assert_eq!(ids(&c), ["a", "b"]);
    assert_eq!(c["model"], "a");
    assert_eq!(c["is_default"], true);
    assert_eq!(c["models"][0]["display_name"], "Model A");
    assert_eq!(c["models"][0]["source"], "listed");
    let id = c["id"].as_str().unwrap().to_owned();
    let list = format!("/v1/providers/{id}/model-list");
    s.patch(
        ALICE,
        &list,
        json!({"model_id": "b", "display_name": "Mine", "enabled": false}),
    )
    .await;

    ep.list(json!([{"id": "a"}, {"id": "c"}]));
    let (status, r) = s.post(ALICE, &format!("{list}/refresh"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["models"], json!(["a", "c"]));
    assert_eq!(
        (r["added"].as_u64(), r["unavailable"].as_u64()),
        (Some(1), Some(1))
    );
    let (_, v) = s.get(ALICE, &list).await;
    assert_eq!(ids(&v), ["a", "b", "c"]);
    let b = &v["models"][1];
    assert_eq!(
        (b["available"].clone(), b["enabled"].clone()),
        (json!(false), json!(false))
    );
    assert_eq!(b["display_name"], "Mine", "the owner's name survives");
    assert_eq!(
        v["models"][0]["display_name"], "Model A",
        "the catalog name stays"
    );
    assert_eq!(v["models"][2]["enabled"], true);

    // A second listed connection gets models but not the default.
    let (status, c2) = s
        .post(
            ALICE,
            "/v1/providers",
            json!({"name": "Gw2", "provider_id": "custom",
                   "base_url": ep.base_url, "api_key": "k", "fetch_models": true}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{c2}");
    assert!(c2.get("models_error").is_none());
    assert_eq!(c2["model"], "a");
    assert_eq!(c2["is_default"], false);
    // Thinking options on an official provider are refused.
    let (status, oa) = s
        .post(
            ALICE,
            "/v1/providers",
            json!({"name": "oa", "provider_id": "openai", "api_key": "sk-x",
                   "models": [{"model_id": "gpt-5.5", "thinking": {"efforts": ["low"], "wire": "reasoning"}}]}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{oa}");
}

#[tokio::test]
async fn defaults_are_automatic_and_promoted_on_delete() {
    let ep = Endpoint::start().await;
    let s = Server::start(vec![ep.base_url.clone()], None).await;
    let make = |name: &'static str, model: Option<&'static str>| {
        let mut body = json!({"name": name, "provider_id": "custom", "base_url": ep.base_url});
        if let Some(m) = model {
            body["model"] = json!(m);
        }
        body
    };
    let (_, first) = s
        .post(ALICE, "/v1/providers", make("first", Some("m1")))
        .await;
    assert_eq!(first["is_default"], true);
    let (_, empty) = s.post(ALICE, "/v1/providers", make("empty", None)).await;
    assert_eq!(empty["is_default"], false);
    let (_, second) = s
        .post(ALICE, "/v1/providers", make("second", Some("m2")))
        .await;
    assert_eq!(second["is_default"], false);
    let (_, third) = s
        .post(ALICE, "/v1/providers", make("third", Some("m3")))
        .await;
    // Deleting the default promotes the oldest remaining one with a model.
    let first_id = first["id"].as_str().unwrap();
    assert_eq!(
        s.send(
            Method::DELETE,
            ALICE,
            &format!("/v1/providers/{first_id}"),
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (_, all) = s.get(ALICE, "/v1/providers").await;
    let default: Vec<&Value> = all["connections"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["is_default"] == true)
        .collect();
    assert_eq!(default.len(), 1);
    assert_eq!(default[0]["id"], second["id"]);
    // Deleting a non-default changes nothing.
    let third_id = third["id"].as_str().unwrap();
    s.send(
        Method::DELETE,
        ALICE,
        &format!("/v1/providers/{third_id}"),
        None,
    )
    .await;
    let (_, all) = s.get(ALICE, "/v1/providers").await;
    assert_eq!(
        all["connections"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["is_default"] == true)
            .count(),
        1
    );
    // A model-less connection that gets its first model (and its owner has
    // no default) becomes the default.
    let (_, bob_c) = s.post(BOB, "/v1/providers", make("bob", None)).await;
    let bob_list = format!("/v1/providers/{}/model-list", bob_c["id"].as_str().unwrap());
    let (status, v) = s
        .post(
            BOB,
            &bob_list,
            json!({"model_id": "bm", "display_name": "Bob M"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    assert_eq!(v["default_model"], "bm");
    let (_, all) = s.get(BOB, "/v1/providers").await;
    assert_eq!(all["connections"][0]["is_default"], true);
}

#[tokio::test]
async fn picker_groups_by_provider_and_shows_the_operator_model_only_without_a_default() {
    let ep = Endpoint::start().await;
    let s = Server::start(vec![ep.base_url.clone()], Some(&ep.base_url)).await;
    let (_, picker) = s.get(ALICE, "/v1/models/available").await;
    assert_eq!(
        picker["operator_model"],
        json!({"model_id": "operator-model", "label": "Server model (operator)"})
    );
    let groups = picker["chat_providers"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["provider_id"], "operator");
    assert_eq!(groups[0]["provider_name"], "Server model (operator)");
    assert_eq!(groups[0]["connections"][0]["connection_id"], Value::Null);
    assert_eq!(
        groups[0]["connections"][0]["models"][0]["id"],
        "operator-model"
    );
    assert!(
        !picker.to_string().contains(&ep.base_url),
        "no endpoint is shown"
    );

    let (_, c) = s
        .post(
            ALICE,
            "/v1/providers",
            json!({"name": "Local", "provider_id": "custom", "base_url": ep.base_url,
                   "models": [{"model_id": "a", "display_name": "A"}, {"model_id": "b"}]}),
        )
        .await;
    let id = c["id"].as_str().unwrap();
    s.patch(
        ALICE,
        &format!("/v1/providers/{id}/model-list"),
        json!({"model_id": "b", "enabled": false}),
    )
    .await;
    let (_, picker) = s.get(ALICE, "/v1/models/available").await;
    assert_eq!(picker["operator_model"], Value::Null);
    let groups = picker["chat_providers"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["provider_id"], "custom");
    assert_eq!(
        groups[0]["provider_name"],
        "Custom OpenAI-compatible endpoint"
    );
    let conn = &groups[0]["connections"][0];
    assert_eq!(conn["connection_id"], id);
    assert_eq!(conn["owned"], true);
    assert_eq!(conn["is_default"], true);
    assert_eq!(
        conn["models"],
        json!([{"id": "a", "display_name": "A", "reasoning_efforts": [], "is_default": true,
                "available": true}])
    );

    // Shared with the organization: Bob sees it as shared, with its
    // enabled models, and no operator entry is offered once he has a
    // default of his own.
    let (status, _) = s
        .send(
            Method::PUT,
            ALICE,
            &format!("/v1/providers/{id}/sharing"),
            Some(json!({"visibility": "organization"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, bob) = s.get(BOB, "/v1/models/available").await;
    let groups = bob["chat_providers"].as_array().unwrap();
    assert_eq!(groups[0]["connections"][0]["owned"], false);
    assert_eq!(groups[0]["connections"][0]["billing"], "owner");
    assert_eq!(groups[0]["connections"][0]["models"][0]["id"], "a");
    assert_eq!(groups.last().unwrap()["provider_id"], "operator");
    // A borrowed turn may pick only an enabled model.
    let answer = s
        .ask(
            BOB,
            json!({"text": "borrow", "invoke_agent": true,
                         "provider_connection_id": id, "model_id": "a"}),
        )
        .await;
    assert_eq!(answer["kind"], "agent", "{answer}");
    assert_eq!(
        s.post(BOB, "/v1/chat/messages",
            json!({"text": "nope", "invoke_agent": true, "provider_connection_id": id, "model_id": "b"}))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Bob can't manage Alice's list.
    assert_eq!(
        s.patch(
            BOB,
            &format!("/v1/providers/{id}/model-list"),
            json!({"model_id": "b", "enabled": true})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
