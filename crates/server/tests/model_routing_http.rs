//! Model routing through the REAL provider path: the default
//! `model_routing::ProviderBackend`, `providers::decide` and the common
//! `metro_model_protocol` HTTP client, against a local System One endpoint
//! that, like some real ones, answers 422 to a request with more than one
//! question. Chat turns go to real `custom` connections on a local
//! OpenAI-compatible endpoint. Nothing in routing is faked.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::http::StatusCode as AxumStatus;
use axum::response::IntoResponse;
use axum::routing::post;
use base64::Engine;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::providers::ProviderService;
use metro_server::providers::secret::MasterKey;
use metro_server::{AppState, Config, Limits, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

const KEY: &str = "sk-fixture-key-0000";

struct FakeAuth;

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            match bearer {
                "alice" | "bob" => Ok(UserIdentity {
                    id: bearer.to_owned(),
                    actor: bearer.to_owned(),
                }),
                _ => Err(AuthFailure::Rejected("unknown".to_owned())),
            }
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

#[derive(Default)]
struct Seen {
    /// Bodies of every `/v1/systemone` request.
    systemone: Vec<Value>,
    /// The `model` of every chat completion.
    chat: Vec<String>,
}

/// A strict one-question System One plus a chat endpoint.
async fn fixture(seen: Arc<Mutex<Seen>>) -> String {
    let (s1, s2) = (seen.clone(), seen);
    let app = axum::Router::new()
        .route(
            "/v1/systemone",
            post(move |Json(body): Json<Value>| {
                let seen = s1.clone();
                async move {
                    seen.lock().unwrap().systemone.push(body.clone());
                    let questions = body["questions"].as_object().cloned().unwrap_or_default();
                    if questions.len() != 1 {
                        return (
                            AxumStatus::UNPROCESSABLE_ENTITY,
                            Json(json!({"detail": "exactly one question per request"})),
                        )
                            .into_response();
                    }
                    let message = body["state"]["message"].as_str().unwrap_or_default();
                    let (id, q) = questions.into_iter().next().unwrap();
                    let answer = match q["type"].as_str() {
                        Some("noul") => {
                            let p = if message.contains("[yes]") { 0.95 } else { 0.1 };
                            json!({"type": "noul", "noul": p})
                        }
                        Some("choice") => {
                            let options: Vec<String> =
                                q["criteria"].as_object().unwrap().keys().cloned().collect();
                            let pick = if message.contains("code") {
                                "code"
                            } else if message.contains("prose") {
                                "writing"
                            } else {
                                "no_match"
                            };
                            let rest = (options.len() - 1) as f64;
                            let probabilities: serde_json::Map<String, Value> = options
                                .iter()
                                .map(|o| {
                                    let p = if o == pick { 0.91 } else { 0.09 / rest };
                                    (o.clone(), json!(p))
                                })
                                .collect();
                            json!({"type": "choice", "choice": pick, "confidence": 0.8,
                                   "probabilities": probabilities})
                        }
                        _ => json!({}),
                    };
                    Json(json!({"model": "jev-1.14.0", "answers": {id: answer},
                                "usage": {"input_tokens": 1, "output_tokens": 1}}))
                    .into_response()
                }
            }),
        )
        .route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let seen = s2.clone();
                async move {
                    let model = body["model"].as_str().unwrap_or_default().to_owned();
                    seen.lock().unwrap().chat.push(model.clone());
                    Json(json!({
                        "id": "c", "object": "chat.completion", "created": 0, "model": model,
                        "choices": [{"index": 0, "finish_reason": "stop",
                                     "message": {"role": "assistant", "content": format!("answered by {model}")}}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/v1")
}

struct H {
    base: String,
    http: reqwest::Client,
    fixture: String,
    seen: Arc<Mutex<Seen>>,
}

impl H {
    async fn start() -> Self {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let fixture = fixture(seen.clone()).await;
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
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
            MasterKey::parse(&base64::engine::general_purpose::STANDARD.encode([3u8; 32])).unwrap();
        state.providers = ProviderService::new(Some(master), vec![fixture.clone()]).unwrap();
        // `state.routing` stays the default: the real provider backend.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            fixture,
            seen,
        }
    }

    async fn req(
        &self,
        m: reqwest::Method,
        who: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut r = self
            .http
            .request(m, format!("{}{path}", self.base))
            .bearer_auth(who);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.unwrap();
        let s = r.status();
        let text = r.text().await.unwrap();
        assert!(!text.contains(KEY), "key echoed");
        (s, serde_json::from_str(&text).unwrap_or(json!(text)))
    }

    async fn connect(&self, provider: &str, model: &str, default: bool) -> String {
        let (s, v) = self
            .req(
                reqwest::Method::POST,
                "alice",
                "/v1/providers",
                Some(
                    json!({"name": model, "provider_id": provider, "base_url": self.fixture,
                            "api_key": KEY, "model": model, "is_default": default}),
                ),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    async fn events(&self) -> Vec<Value> {
        let (_, v) = self
            .req(reqwest::Method::GET, "alice", "/v1/chat/events", None)
            .await;
        v["events"].as_array().unwrap().clone()
    }

    async fn say(&self, body: Value) -> i64 {
        let (s, v) = self
            .req(
                reqwest::Method::POST,
                "alice",
                "/v1/chat/messages",
                Some(body),
            )
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        v["id"].as_i64().unwrap()
    }

    async fn answer_after(&self, after: i64) -> (Value, Value) {
        for _ in 0..400 {
            let events = self.events().await;
            if let Some(a) = events.iter().find(|e| {
                e["id"].as_i64().unwrap() > after && (e["kind"] == "agent" || e["kind"] == "error")
            }) {
                let route = events
                    .iter()
                    .find(|e| {
                        e["id"].as_i64().unwrap() > after && e["activity"]["phase"] == "started"
                    })
                    .map(|e| e["activity"]["route"].clone())
                    .unwrap_or(Value::Null);
                return (a.clone(), route);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no answer after {after}");
    }

    fn systemone_since(&self, n: usize) -> Vec<Value> {
        self.seen.lock().unwrap().systemone[n..].to_vec()
    }

    fn counts(&self) -> (usize, usize) {
        let s = self.seen.lock().unwrap();
        (s.systemone.len(), s.chat.len())
    }
}

fn keys(body: &Value) -> Vec<String> {
    body["questions"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

#[tokio::test]
async fn one_question_per_request_through_the_real_client() {
    let h = H::start().await;
    let classifier = h.connect("custom-systemone", "jev-latest", true).await;
    let _default = h.connect("custom", "default-model", true).await;
    let code = h.connect("custom", "code-model", false).await;
    let writing = h.connect("custom", "writing-model", false).await;
    let (s, v) = h
        .req(
            reqwest::Method::PUT,
            "alice",
            "/v1/model-routing",
            Some(json!({
                "enabled": true, "infer_addressing": true, "classifier_id": classifier,
                "cases": [
                    {"key": "code", "label": "Coding", "description": "Programming", "connection_id": code},
                    {"key": "writing", "label": "Writing", "description": "Prose", "connection_id": writing},
                ],
            })),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // Unaddressed and clearly for the agent: a lone Noul, then a lone Choice.
    let (n, _) = h.counts();
    let e = h.say(json!({"text": "[yes] please fix my code"})).await;
    let (a, route) = h.answer_after(e).await;
    assert_eq!(a["text"], "answered by code-model", "{a}");
    assert_eq!(route["source"], "case");
    assert_eq!(route["case_key"], "code");
    assert_eq!(route["addressed_by"], "classifier");
    assert_eq!(route["model_id"], "code-model");
    assert!(
        !route.to_string().contains(&h.fixture),
        "no endpoint in the route"
    );
    let calls = h.systemone_since(n);
    assert_eq!(
        calls.iter().map(keys).collect::<Vec<_>>(),
        vec![vec!["directed"], vec!["use_case"]]
    );
    assert_eq!(calls[0]["model"], "jev-latest");
    assert_eq!(calls[0]["questions"]["directed"]["type"], "noul");
    let options: Vec<String> = calls[1]["questions"]["use_case"]["criteria"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(options, vec!["code", "no_match", "writing"]);
    for c in &calls {
        let state = c["state"].as_object().unwrap();
        assert_eq!(state.len(), 2, "message and where only: {state:?}");
        assert_eq!(state["message"], "[yes] please fix my code");
    }

    // Not for the agent: exactly one Noul, no Choice, no chat call.
    let (n, chats) = h.counts();
    let e = h.say(json!({"text": "lunch at noon, code later"})).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        h.events()
            .await
            .iter()
            .all(|x| x["id"].as_i64().unwrap() <= e)
    );
    let calls = h.systemone_since(n);
    assert_eq!(
        calls.iter().map(keys).collect::<Vec<_>>(),
        vec![vec!["directed"]]
    );
    assert_eq!(h.counts().1, chats);

    // Explicit agent turn: no Noul, a lone Choice.
    let (n, _) = h.counts();
    let e = h.say(json!({"text": "@agent tighten this prose"})).await;
    let (a, route) = h.answer_after(e).await;
    assert_eq!(a["text"], "answered by writing-model", "{a}");
    assert_eq!(route["addressed_by"], "explicit");
    assert_eq!(
        h.systemone_since(n).iter().map(keys).collect::<Vec<_>>(),
        vec![vec!["use_case"]]
    );

    // The reserved no-match option: the person's default chat connection.
    let (n, _) = h.counts();
    let e = h.say(json!({"text": "@agent what's the weather"})).await;
    let (a, route) = h.answer_after(e).await;
    assert_eq!(a["text"], "answered by default-model", "{a}");
    assert_eq!(route["source"], "default");
    assert_eq!(h.systemone_since(n).len(), 1);

    // A chosen model asks System One nothing.
    let (n, _) = h.counts();
    let e = h
        .say(json!({"text": "@agent fix code", "provider_connection_id": writing}))
        .await;
    let (a, route) = h.answer_after(e).await;
    assert_eq!(a["text"], "answered by writing-model", "{a}");
    assert_eq!(route["source"], "override");
    assert_eq!(h.systemone_since(n).len(), 0);

    // The System One connection is gone: an explicit routed turn fails
    // visibly, and neither System One nor any chat model (not the default)
    // is called.
    let (st, _) = h
        .req(
            reqwest::Method::DELETE,
            "alice",
            &format!("/v1/providers/{classifier}"),
            None,
        )
        .await;
    assert!(st.is_success(), "{st}");
    let before = h.counts();
    let e = h.say(json!({"text": "@agent fix my code"})).await;
    let (a, _) = h.answer_after(e).await;
    assert_eq!(a["kind"], "error", "{a}");
    assert!(
        a["text"]
            .as_str()
            .unwrap()
            .contains("could not choose a model"),
        "{a}"
    );
    assert_eq!(h.counts(), before, "no System One or chat call");
    // And opted-in unaddressed text no longer wakes the agent at all.
    let e = h.say(json!({"text": "[yes] fix my code"})).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        h.events()
            .await
            .iter()
            .all(|x| x["id"].as_i64().unwrap() <= e)
    );
    assert_eq!(h.counts(), before);

    // Every request the server sent had exactly one question; the endpoint
    // never had to refuse one.
    assert!(
        h.seen
            .lock()
            .unwrap()
            .systemone
            .iter()
            .all(|b| keys(b).len() == 1)
    );
}
