//! Personal model routing over real HTTP, with a fake System One and fake
//! connection grants behind `AppState::routing`, and a real OpenAI-compatible
//! mock endpoint answering as whichever model was chosen.
//!
//! Covers: defaults keep the old behaviour; people-only messages never reach
//! System One; explicit `@agent` skips the addressing Noul and an explicit
//! model skips routing; the opt-in addressing threshold and abstention on
//! uncertain, malformed and failed answers; use-case thresholds and keys
//! that are never ids; a routed connection that is not usable here fails
//! without a fallback; Space-only shares stay in their Space; settings
//! validation; and the scheduled-prompt path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::routing::post;
use metro_model_protocol::Capability;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::model_routing::{BoxFuture, Routing, RoutingBackend, submit_scheduled};
use metro_server::providers::{SelectedModel, UseError};
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

struct FakeAuth;

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            match bearer {
                "alice" | "bob" | "carol" => Ok(UserIdentity {
                    id: bearer.to_owned(),
                    actor: bearer.to_owned(),
                }),
                _ => Err(AuthFailure::Rejected("unknown token".to_owned())),
            }
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

fn id(n: char) -> String {
    format!("pc_{}", n.to_string().repeat(32))
}

const CLASSIFIER: char = '1';
const CODE: char = '2';
const WRITING: char = '3';
const OVERRIDE: char = '4';
const EMBED: char = '5';
const SPACE_ONLY: char = '6';
const BOBS: char = '7';

/// Who may use a fake connection.
#[derive(Clone)]
enum Grant {
    Owner(&'static str),
    /// Shared with this Space id (the user must still be a member).
    Space(String),
}

#[derive(Clone)]
struct FakeConn {
    capability: Capability,
    grants: Vec<Grant>,
    /// The model name the mock endpoint answers as.
    model: String,
}

/// (user, space, connection, state, questions) of one System One call.
type Call = (String, Option<String>, String, Value, Value);

#[derive(Default)]
struct Fake {
    conns: Mutex<HashMap<String, FakeConn>>,
    /// Every System One call.
    calls: Mutex<Vec<Call>>,
    base_url: Mutex<String>,
    members: Mutex<Vec<(String, String)>>,
}

impl Fake {
    fn add(&self, n: char, capability: Capability, grants: Vec<Grant>, model: &str) {
        self.conns.lock().unwrap().insert(
            id(n),
            FakeConn {
                capability,
                grants,
                model: model.to_owned(),
            },
        );
    }

    fn usable(&self, user: &str, space: Option<&str>, conn: &FakeConn) -> bool {
        let members = self.members.lock().unwrap();
        conn.grants.iter().any(|g| match g {
            Grant::Owner(o) => *o == user,
            Grant::Space(sid) => {
                space == Some(sid.as_str()) && members.iter().any(|(s, u)| s == sid && u == user)
            }
        })
    }

    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn last_call(&self) -> Call {
        self.calls.lock().unwrap().last().cloned().unwrap()
    }

    /// Scripted System One: the message text says what to answer.
    fn answer(message: &str, questions: &Value) -> Result<Value, String> {
        if message.contains("[err]") {
            return Err("upstream 529".to_owned());
        }
        if message.contains("[bad]") {
            return Ok(json!({"answers": "nope"}));
        }
        let mut answers = serde_json::Map::new();
        let asked = questions.as_object().unwrap();
        // Like System One endpoints that take exactly one typed question.
        if asked.len() != 1 {
            return Err("HTTP 422: one question per request".to_owned());
        }
        if asked.contains_key("directed") {
            let p = if message.contains("[yes]") {
                0.95
            } else if message.contains("[maybe]") {
                0.6
            } else {
                0.05
            };
            answers.insert("directed".into(), json!({"type": "noul", "noul": p}));
        }
        if asked.contains_key("use_case") {
            let (choice, p) = if message.contains("[evil]") {
                (id(OVERRIDE), 0.99)
            } else if message.contains("[weak]") {
                ("code".to_owned(), 0.5)
            } else if message.contains("[nofit]") {
                ("no_match".to_owned(), 0.9)
            } else if message.contains("code") {
                ("code".to_owned(), 0.9)
            } else {
                ("writing".to_owned(), 0.9)
            };
            answers.insert(
                "use_case".into(),
                json!({"type": "choice", "choice": choice, "confidence": 0.8,
                       "probabilities": {choice.clone(): p}}),
            );
        }
        Ok(json!({"model": "jev-test", "answers": answers, "usage": {}}))
    }
}

struct Backend(Arc<Fake>);

impl RoutingBackend for Backend {
    fn system_one<'a>(
        &'a self,
        _state: &'a AppState,
        user: &'a str,
        space: Option<&'a str>,
        id: &'a str,
        state_payload: Value,
        questions: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            self.0.calls.lock().unwrap().push((
                user.to_owned(),
                space.map(str::to_owned),
                id.to_owned(),
                state_payload.clone(),
                questions.clone(),
            ));
            let conn = self.0.conns.lock().unwrap().get(id).cloned();
            match conn {
                Some(c)
                    if c.capability == Capability::SystemOne && self.0.usable(user, space, &c) => {}
                _ => anyhow::bail!("not found"),
            }
            let message = state_payload["message"].as_str().unwrap_or_default();
            Fake::answer(message, &questions).map_err(anyhow::Error::msg)
        })
    }

    fn visible_capability<'a>(
        &'a self,
        _state: &'a AppState,
        user: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Capability>>> {
        Box::pin(async move {
            let conn = self.0.conns.lock().unwrap().get(id).cloned();
            Ok(conn.and_then(|c| {
                let members = self.0.members.lock().unwrap().clone();
                let visible = c.grants.iter().any(|g| match g {
                    Grant::Owner(o) => *o == user,
                    Grant::Space(sid) => members.iter().any(|(s, u)| s == sid && u == user),
                });
                visible.then_some(c.capability)
            }))
        })
    }

    fn chat_model<'a>(
        &'a self,
        _state: &'a AppState,
        user: &'a str,
        space: Option<&'a str>,
        id: &'a str,
    ) -> BoxFuture<'a, Result<SelectedModel, UseError>> {
        Box::pin(async move {
            let conn = self.0.conns.lock().unwrap().get(id).cloned();
            let Some(conn) = conn.filter(|c| self.0.usable(user, space, c)) else {
                return Err(UseError::NotFound);
            };
            if conn.capability != Capability::Chat {
                return Err(UseError::NotFound);
            }
            let base_url = self.0.base_url.lock().unwrap().clone();
            Ok(SelectedModel {
                connection_id: id.to_owned(),
                provider_id: "fake".to_owned(),
                model_id: conn.model.clone(),
                model: metro_server::agent::openai_compatible_model(&ModelConfig {
                    base_url,
                    model: conn.model.clone(),
                    api_key: String::new(),
                }),
                max_tokens: None,
            })
        })
    }
}

/// An OpenAI-compatible endpoint that answers as the model it was asked for,
/// and records which models were called.
async fn endpoint(calls: Arc<Mutex<Vec<String>>>) -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let calls = calls.clone();
            async move {
                let model = body["model"].as_str().unwrap_or_default().to_owned();
                calls.lock().unwrap().push(model.clone());
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
    state: AppState,
    fake: Arc<Fake>,
    models: Arc<Mutex<Vec<String>>>,
}

impl H {
    async fn start() -> Self {
        let models = Arc::new(Mutex::new(Vec::new()));
        let base_url = endpoint(models.clone()).await;
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: Some(ModelConfig {
                base_url: base_url.clone(),
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
        let fake = Arc::new(Fake::default());
        *fake.base_url.lock().unwrap() = base_url;
        fake.add(
            CLASSIFIER,
            Capability::SystemOne,
            vec![Grant::Owner("alice")],
            "jev",
        );
        fake.add(
            CODE,
            Capability::Chat,
            vec![Grant::Owner("alice")],
            "code-model",
        );
        fake.add(
            WRITING,
            Capability::Chat,
            vec![Grant::Owner("alice")],
            "writing-model",
        );
        fake.add(
            OVERRIDE,
            Capability::Chat,
            vec![Grant::Owner("alice")],
            "override-model",
        );
        fake.add(
            EMBED,
            Capability::Embeddings,
            vec![Grant::Owner("alice")],
            "embed",
        );
        fake.add(
            BOBS,
            Capability::Chat,
            vec![Grant::Owner("bob")],
            "bobs-model",
        );
        state.routing = Routing::new(Arc::new(Backend(fake.clone())));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = state.clone();
        tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            state,
            fake,
            models,
        }
    }

    async fn req(
        &self,
        method: reqwest::Method,
        who: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(who);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.unwrap();
        let status = r.status();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(json!(text)))
    }

    async fn post(&self, who: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.req(reqwest::Method::POST, who, path, Some(body)).await
    }

    async fn put_routing(&self, who: &str, body: Value) -> (StatusCode, Value) {
        self.req(reqwest::Method::PUT, who, "/v1/model-routing", Some(body))
            .await
    }

    /// A Space owned by alice with bob as a member; its id.
    async fn space(&self, name: &str) -> String {
        let (s, v) = self
            .post("alice", "/v1/spaces", json!({"name": name}))
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let sid = v["id"].as_str().unwrap().to_owned();
        let (s, v) = self
            .post(
                "alice",
                &format!("/v1/spaces/{sid}/members"),
                json!({"user_id": "bob"}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let mut m = self.fake.members.lock().unwrap();
        m.push((sid.clone(), "alice".into()));
        m.push((sid.clone(), "bob".into()));
        sid
    }

    async fn events(&self, who: &str, sid: Option<&str>) -> Vec<Value> {
        let path = match sid {
            Some(sid) => format!("/v1/spaces/{sid}/rooms/lobby/events"),
            None => "/v1/chat/events".to_owned(),
        };
        let (_, v) = self.req(reqwest::Method::GET, who, &path, None).await;
        v["events"].as_array().unwrap().clone()
    }

    async fn say(&self, who: &str, sid: Option<&str>, body: Value) -> Value {
        let path = match sid {
            Some(sid) => format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            None => "/v1/chat/messages".to_owned(),
        };
        let (s, v) = self.post(who, &path, body).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        v
    }

    /// The agent's answer (or error) after event `after`, waiting for it.
    async fn answer_after(&self, who: &str, sid: Option<&str>, after: i64) -> Value {
        for _ in 0..400 {
            if let Some(e) = self.events(who, sid).await.into_iter().find(|e| {
                e["id"].as_i64().unwrap() > after && (e["kind"] == "agent" || e["kind"] == "error")
            }) {
                return e;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no answer after event {after}");
    }

    /// The `started` marker's route of the turn after `after`.
    async fn route_after(&self, who: &str, sid: Option<&str>, after: i64) -> Value {
        self.events(who, sid)
            .await
            .into_iter()
            .find(|e| {
                e["id"].as_i64().unwrap() > after
                    && e["kind"] == "activity"
                    && e["activity"]["phase"] == "started"
            })
            .map(|e| e["activity"]["route"].clone())
            .expect("a started marker")
    }

    /// Nothing but `event` happens for a moment: no turn, no error.
    async fn quiet_after(&self, who: &str, sid: Option<&str>, after: i64) {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let later: Vec<Value> = self
            .events(who, sid)
            .await
            .into_iter()
            .filter(|e| e["id"].as_i64().unwrap() > after)
            .collect();
        assert!(later.is_empty(), "unexpected events: {later:?}");
    }

    fn model_calls(&self) -> Vec<String> {
        self.models.lock().unwrap().clone()
    }
}

fn routing_on(infer: bool, enabled: bool) -> Value {
    json!({
        "enabled": enabled,
        "infer_addressing": infer,
        "classifier_id": id(CLASSIFIER),
        "cases": [
            {"key": "code", "label": "Coding", "description": "Programming and code review",
             "connection_id": id(CODE)},
            {"key": "writing", "label": "Writing", "description": "Prose and editing",
             "connection_id": id(WRITING)},
        ],
    })
}

#[tokio::test]
async fn defaults_are_off_and_keep_the_old_behaviour() {
    let h = H::start().await;
    let (s, v) = h
        .req(reqwest::Method::GET, "alice", "/v1/model-routing", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        v,
        json!({"enabled": false, "infer_addressing": false, "classifier_id": null,
               "embedding_id": null, "default_chat_id": null, "min_directedness": 0.85,
               "min_route_probability": 0.65, "cases": []})
    );
    let sid = h.space("defaults").await;
    // Unaddressed text is ordinary conversation.
    let e = h
        .say("alice", Some(&sid), json!({"text": "[yes] can you help?"}))
        .await;
    h.quiet_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    // People only: nothing.
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@bob [yes] can you help?"}),
        )
        .await;
    assert_eq!(e["audience"], "users");
    h.quiet_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    // @agent: the default (here the server model), as before.
    let e = h
        .say("alice", Some(&sid), json!({"text": "@agent hi"}))
        .await;
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["text"], "answered by env-model", "{a}");
    let route = h
        .route_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(route["source"], "environment");
    assert_eq!(route["addressed_by"], "explicit");
    assert_eq!(h.fake.calls(), 0, "System One is never asked by default");
}

#[tokio::test]
async fn addressing_inference_is_opt_in_thresholded_and_abstains() {
    let h = H::start().await;
    let sid = h.space("infer").await;
    let (s, v) = h.put_routing("alice", routing_on(true, false)).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // A clear yes starts a turn as alice, marked as the classifier's.
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "[yes] what does this error mean?"}),
        )
        .await;
    assert_eq!(e["audience"], "agent");
    assert_eq!(
        e["mentions"],
        json!([{"kind": "agent", "addressed_by": "classifier"}])
    );
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["kind"], "agent", "{a}");
    let route = h
        .route_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(route["addressed_by"], "classifier");
    // Only the new message went to System One: no history, no ids.
    let (user, space, conn, payload, questions) = h.fake.last_call();
    assert_eq!(
        (user.as_str(), space.as_deref()),
        ("alice", Some(sid.as_str()))
    );
    assert_eq!(conn, id(CLASSIFIER));
    assert_eq!(payload["message"], "[yes] what does this error mean?");
    assert_eq!(payload.as_object().unwrap().len(), 2, "{payload}");
    assert_eq!(questions["directed"]["type"], "noul");
    assert!(questions.get("use_case").is_none(), "routing is off");

    // Uncertain, malformed and failed answers start nothing.
    for text in ["[maybe] hmm", "[bad] hmm", "[err] hmm", "lunch?"] {
        let before = h.fake.calls();
        let e = h.say("alice", Some(&sid), json!({"text": text})).await;
        assert_eq!(e["audience"], "room", "{text}");
        assert_eq!(e["mentions"], json!([]));
        h.quiet_after("alice", Some(&sid), e["id"].as_i64().unwrap())
            .await;
        assert_eq!(h.fake.calls(), before + 1, "{text} was judged once");
    }

    // People-only messages and other people's messages never reach System One.
    let before = h.fake.calls();
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@bob [yes] can you look?"}),
        )
        .await;
    h.quiet_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    let e = h
        .say("bob", Some(&sid), json!({"text": "[yes] can you look?"}))
        .await;
    h.quiet_after("bob", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    // Explicit @agent skips the addressing question (routing is off: no call at all).
    let e = h
        .say("alice", Some(&sid), json!({"text": "@agent [no] hi"}))
        .await;
    h.answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(h.fake.calls(), before);

    // Personal Chat too, with the same opt-in.
    let e = h
        .say("alice", None, json!({"text": "[yes] summarize my day"}))
        .await;
    assert_eq!(e["mentions"][0]["addressed_by"], "classifier");
    let a = h
        .answer_after("alice", None, e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["kind"], "agent", "{a}");
    assert_eq!(h.fake.last_call().1, None);
    // A forged classifier mark is refused by the mention wire.
    let (s, _) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            json!({"text": "x", "mentions": [{"kind": "agent", "addressed_by": "classifier"}]}),
        )
        .await;
    assert_ne!(s, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn use_cases_route_explicit_turns_and_explicit_models_win() {
    let h = H::start().await;
    let sid = h.space("cases").await;
    let (s, v) = h.put_routing("alice", routing_on(false, true)).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@agent review this code"}),
        )
        .await;
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["text"], "answered by code-model", "{a}");
    let route = h
        .route_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(route["source"], "case");
    assert_eq!(route["case_key"], "code");
    assert_eq!(route["case_label"], "Coding");
    assert_eq!(route["connection_id"], id(CODE));
    assert_eq!(route["model_id"], "code-model");
    let questions = h.fake.last_call().4;
    assert!(
        questions.get("directed").is_none(),
        "explicit @agent is not judged"
    );
    assert_eq!(questions["use_case"]["type"], "choice");
    assert_eq!(
        questions.as_object().unwrap().len(),
        1,
        "one question per call"
    );
    let options = questions["use_case"]["criteria"].as_object().unwrap();
    assert!(options.contains_key("no_match") && options.contains_key("code"));

    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "polish this paragraph", "invoke_agent": true}),
        )
        .await;
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["text"], "answered by writing-model", "{a}");

    // An explicit model for the turn wins, and System One is not asked.
    let before = h.fake.calls();
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@agent review this code", "provider_connection_id": id(OVERRIDE)}),
        )
        .await;
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["text"], "answered by override-model", "{a}");
    assert_eq!(h.fake.calls(), before);
    let route = h
        .route_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(route["source"], "override");

    // Someone else's connection and a non-agent turn are refused before storing.
    let (s, _) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            json!({"text": "@agent x", "provider_connection_id": id(BOBS)}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            json!({"text": "x", "provider_connection_id": id(OVERRIDE)}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Addressing first (a lone Noul), then, only after a yes, the lone Choice.
    let (s, _) = h.put_routing("alice", routing_on(true, true)).await;
    assert_eq!(s, StatusCode::OK);
    let before = h.fake.calls();
    let e = h
        .say("alice", None, json!({"text": "[yes] fix my code"}))
        .await;
    let a = h
        .answer_after("alice", None, e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["text"], "answered by code-model", "{a}");
    let calls = h.fake.calls.lock().unwrap()[before..].to_vec();
    let asked: Vec<Vec<String>> = calls
        .iter()
        .map(|c| c.4.as_object().unwrap().keys().cloned().collect())
        .collect();
    assert_eq!(
        asked,
        vec![vec!["directed".to_owned()], vec!["use_case".to_owned()]]
    );

    // A rejected, uncertain or failed addressing Noul never leads to a Choice.
    for text in [
        "lunch, code later",
        "[maybe] code?",
        "[err] code",
        "[bad] code",
    ] {
        let before = h.fake.calls();
        let e = h.say("alice", None, json!({"text": text})).await;
        h.quiet_after("alice", None, e["id"].as_i64().unwrap())
            .await;
        let calls = h.fake.calls.lock().unwrap()[before..].to_vec();
        assert_eq!(calls.len(), 1, "{text}");
        assert!(calls[0].4.get("use_case").is_none(), "{text}: no Choice");
    }
    let route = h
        .route_after("alice", None, e["id"].as_i64().unwrap())
        .await;
    assert_eq!(
        (route["source"].as_str(), route["addressed_by"].as_str()),
        (Some("case"), Some("classifier"))
    );
}

/// `text` fails visibly because routing could not decide, before any
/// model is called or any turn starts.
async fn routing_fails(h: &H, text: &str, sid: &str) {
    let calls = h.model_calls().len();
    let e = h.say("alice", Some(sid), json!({"text": text})).await;
    let a = h
        .answer_after("alice", Some(sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["kind"], "error", "{text}: {a}");
    assert!(
        a["text"]
            .as_str()
            .unwrap()
            .contains("could not choose a model"),
        "{text}: {a}"
    );
    assert_eq!(h.model_calls().len(), calls, "{text}: no model was charged");
    let started =
        h.events("alice", Some(sid)).await.into_iter().any(|x| {
            x["id"].as_i64().unwrap() > e["id"].as_i64().unwrap() && x["kind"] == "activity"
        });
    assert!(!started, "{text}: no turn started");
}

#[tokio::test]
async fn weak_unknown_and_failed_routes_use_the_default_or_fail_clearly() {
    let h = H::start().await;
    let sid = h.space("weak").await;
    let mut settings = routing_on(false, true);
    settings["default_chat_id"] = json!(id(WRITING));
    let (s, v) = h.put_routing("alice", settings).await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // A valid answer that picks nothing: the authorized routing default.
    for text in ["@agent [weak] code", "@agent [nofit] code"] {
        let e = h.say("alice", Some(&sid), json!({"text": text})).await;
        let a = h
            .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
            .await;
        assert_eq!(a["text"], "answered by writing-model", "{text}: {a}");
        let route = h
            .route_after("alice", Some(&sid), e["id"].as_i64().unwrap())
            .await;
        assert_eq!(route["source"], "routing_default", "{text}");
    }

    // A failed decision is not a no-match: an option that was never offered
    // (an id), a malformed answer, a failed call, and a System One
    // connection that can no longer be used each fail the turn visibly
    // before any model is called.
    for text in [
        "@agent [evil] code",
        "@agent [bad] code",
        "@agent [err] code",
    ] {
        routing_fails(&h, text, &sid).await;
    }
    let grants = std::mem::take(
        &mut h
            .fake
            .conns
            .lock()
            .unwrap()
            .get_mut(&id(CLASSIFIER))
            .unwrap()
            .grants,
    );
    routing_fails(&h, "@agent review this code", &sid).await;
    h.fake
        .conns
        .lock()
        .unwrap()
        .get_mut(&id(CLASSIFIER))
        .unwrap()
        .grants = grants;
    assert!(!h.model_calls().iter().any(|m| m == "override-model"));

    // The routed connection is withdrawn: the turn fails and says why; no
    // other model is called.
    h.fake
        .conns
        .lock()
        .unwrap()
        .get_mut(&id(CODE))
        .unwrap()
        .grants
        .clear();
    let calls = h.model_calls().len();
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@agent review this code"}),
        )
        .await;
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["kind"], "error", "{a}");
    let text = a["text"].as_str().unwrap();
    assert!(
        text.contains("\"Coding\"") && text.contains("No other model was used"),
        "{text}"
    );
    assert_eq!(h.model_calls().len(), calls, "nothing fell back");
}

#[tokio::test]
async fn shared_connections_stay_in_their_space_and_settings_are_validated() {
    let h = H::start().await;
    let s1 = h.space("one").await;
    let s2 = h.space("two").await;
    // Bob's connection, shared with Space one only.
    h.fake.add(
        SPACE_ONLY,
        Capability::Chat,
        vec![Grant::Owner("bob"), Grant::Space(s1.clone())],
        "shared-model",
    );
    let mut settings = routing_on(false, true);
    settings["cases"][0]["connection_id"] = json!(id(SPACE_ONLY));
    let (s, v) = h.put_routing("alice", settings).await;
    assert_eq!(
        s,
        StatusCode::OK,
        "visible somewhere is enough to save: {v}"
    );

    let e = h
        .say("alice", Some(&s1), json!({"text": "@agent review code"}))
        .await;
    let a = h
        .answer_after("alice", Some(&s1), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["text"], "answered by shared-model", "{a}");
    for sid in [Some(s2.as_str()), None] {
        let calls = h.model_calls().len();
        let e = h
            .say("alice", sid, json!({"text": "@agent review code"}))
            .await;
        let a = h
            .answer_after("alice", sid, e["id"].as_i64().unwrap())
            .await;
        assert_eq!(a["kind"], "error", "{sid:?}: {a}");
        assert_eq!(h.model_calls().len(), calls, "{sid:?}: nothing fell back");
    }
    // Carol is in neither Space: she can't even save it.
    let mut carols = routing_on(false, false);
    carols["classifier_id"] = Value::Null;
    carols["cases"] = json!([]);
    carols["default_chat_id"] = json!(id(SPACE_ONLY));
    let (s, _) = h.put_routing("carol", carols).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Validation: foreign, missing, wrong capability, limits, ranges, fields.
    let bad = [
        json!({"default_chat_id": id(BOBS)}),
        json!({"default_chat_id": format!("pc_{}", "f".repeat(32))}),
        json!({"default_chat_id": "https://evil.example/v1"}),
        json!({"default_chat_id": id(CLASSIFIER)}),
        json!({"classifier_id": id(CODE), "infer_addressing": true}),
        json!({"embedding_id": id(CODE)}),
        json!({"infer_addressing": true}),
        json!({"enabled": true, "cases": routing_on(false, true)["cases"]}),
        json!({"min_directedness": 0.2}),
        json!({"min_route_probability": 1.5}),
        json!({"min_route_probability": "high"}),
        json!({"surprise": true}),
        json!({"enabled": true, "classifier_id": id(CLASSIFIER),
               "cases": (0..17).map(|i| json!({"key": format!("c{i}"), "label": "L",
                   "description": "D", "connection_id": id(CODE)})).collect::<Vec<_>>()}),
        json!({"enabled": true, "classifier_id": id(CLASSIFIER),
               "cases": [{"key": "Bad Key", "label": "L", "description": "D", "connection_id": id(CODE)}]}),
    ];
    for body in bad {
        let (s, v) = h.put_routing("alice", body.clone()).await;
        assert!(s.is_client_error(), "{body} -> {s} {v}");
    }
    let (s, v) = h
        .put_routing(
            "alice",
            json!({"embedding_id": id(EMBED), "default_chat_id": id(CODE)}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["embedding_id"], id(EMBED));
    let prefs = metro_server::model_routing::preferences(&h.state, "alice")
        .await
        .unwrap();
    assert_eq!(prefs.embedding_id.as_deref(), Some(id(EMBED).as_str()));
    assert!(!prefs.enabled);
}

#[tokio::test]
async fn scheduled_prompts_take_the_same_path_and_recheck_membership() {
    let h = H::start().await;
    let sid = h.space("sched").await;
    let (s, _) = h.put_routing("alice", routing_on(false, true)).await;
    assert_eq!(s, StatusCode::OK);

    let last = |events: Vec<Value>| {
        events
            .last()
            .map(|e| e["id"].as_i64().unwrap())
            .unwrap_or(0)
    };
    let before = last(h.events("alice", Some(&sid)).await);
    submit_scheduled(
        &h.state,
        "alice",
        Some((&sid, "lobby")),
        "daily code review",
    )
    .await
    .unwrap();
    let events = h.events("alice", Some(&sid)).await;
    let prompt = events
        .iter()
        .find(|e| e["id"].as_i64().unwrap() > before && e["kind"] == "message")
        .unwrap();
    assert_eq!(prompt["actor_id"], "alice");
    assert_eq!(prompt["audience"], "agent");
    assert_eq!(
        prompt["mentions"],
        json!([{"kind": "agent", "addressed_by": "schedule"}])
    );
    let a = h.answer_after("alice", Some(&sid), before).await;
    assert_eq!(a["text"], "answered by code-model", "{a}");
    let route = h.route_after("alice", Some(&sid), before).await;
    assert_eq!(
        (route["source"].as_str(), route["addressed_by"].as_str()),
        (Some("case"), Some("schedule"))
    );
    assert!(h.fake.last_call().4.get("directed").is_none());

    // Personal Chat.
    submit_scheduled(&h.state, "alice", None, "draft my weekly notes")
        .await
        .unwrap();
    let a = h.answer_after("alice", None, 0).await;
    assert_eq!(a["text"], "answered by writing-model", "{a}");

    // Not a member (any more): refused, nothing stored.
    let before = last(h.events("alice", Some(&sid)).await);
    assert!(
        submit_scheduled(&h.state, "carol", Some((&sid, "lobby")), "x")
            .await
            .is_err()
    );
    assert!(
        submit_scheduled(&h.state, "alice", Some((&sid, "nope")), "x")
            .await
            .is_err()
    );
    assert!(
        submit_scheduled(&h.state, "alice", Some(("nope", "lobby")), "x")
            .await
            .is_err()
    );
    h.quiet_after("alice", Some(&sid), before).await;

    // A revoked System One connection: the occurrence fails visibly and no
    // model (not even the default) is charged.
    let grants = std::mem::take(
        &mut h
            .fake
            .conns
            .lock()
            .unwrap()
            .get_mut(&id(CLASSIFIER))
            .unwrap()
            .grants,
    );
    let calls = h.model_calls().len();
    let before_err = last(h.events("alice", Some(&sid)).await);
    let err = submit_scheduled(&h.state, "alice", Some((&sid, "lobby")), "code please")
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("could not choose a model"),
        "{err}"
    );
    let a = h.answer_after("alice", Some(&sid), before_err).await;
    assert_eq!(a["kind"], "error");
    let err = submit_scheduled(&h.state, "alice", None, "code please")
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("could not choose a model"),
        "{err}"
    );
    assert_eq!(h.model_calls().len(), calls, "no surprise default call");
    h.fake
        .conns
        .lock()
        .unwrap()
        .get_mut(&id(CLASSIFIER))
        .unwrap()
        .grants = grants;

    // A routed connection that can't be used fails visibly, without fallback.
    h.fake
        .conns
        .lock()
        .unwrap()
        .get_mut(&id(CODE))
        .unwrap()
        .grants
        .clear();
    let calls = h.model_calls().len();
    let before = last(h.events("alice", Some(&sid)).await);
    let err = submit_scheduled(&h.state, "alice", Some((&sid, "lobby")), "code again")
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("No other model was used"),
        "{err}"
    );
    assert_eq!(h.model_calls().len(), calls);
    let a = h.answer_after("alice", Some(&sid), before).await;
    assert_eq!(a["kind"], "error");
    assert!(
        a["text"]
            .as_str()
            .unwrap()
            .contains("No other model was used"),
        "{a}"
    );
}
