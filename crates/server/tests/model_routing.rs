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
use metro_server::device_relay::DeviceError;
use metro_server::model_routing::MODEL_UNAVAILABLE_IN_ROOM;
use metro_server::model_routing::{BoxFuture, Routing, RoutingBackend, submit_scheduled};
use metro_server::providers::{ProviderError, SelectedModel, UseError};
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::sync::Notify;

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

struct HeldDecision {
    question: &'static str,
    arrived: Notify,
    release: Notify,
}

impl HeldDecision {
    async fn wait(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.arrived.notified())
            .await
            .expect("the held decision was not requested");
    }
}

#[derive(Default)]
struct Fake {
    conns: Mutex<HashMap<String, FakeConn>>,
    /// Every System One call.
    calls: Mutex<Vec<Call>>,
    base_url: Mutex<String>,
    members: Mutex<Vec<(String, String)>>,
    /// Connections whose (device) endpoint is offline: an upstream-side
    /// failure that has nothing to do with access.
    offline: Mutex<Vec<String>>,
    hold: Mutex<Option<Arc<HeldDecision>>>,
}

impl Fake {
    fn hold(&self, question: &'static str) -> Arc<HeldDecision> {
        let hold = Arc::new(HeldDecision {
            question,
            arrived: Notify::new(),
            release: Notify::new(),
        });
        *self.hold.lock().unwrap() = Some(hold.clone());
        hold
    }

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
        if let Some(q) = asked.get("agent") {
            let has_none = q["criteria"].get("no_agent").is_some();
            let (choice, p) = if message.contains("[pick2]") {
                ("agent_2", 0.9)
            } else if message.contains("[yes]") {
                ("agent_1", 0.95)
            } else if message.contains("[maybe]") {
                ("agent_1", 0.6)
            } else if message.contains("[unsure]") {
                ("agent_2", 0.4)
            } else if message.contains("[ghost]") {
                ("agent_9", 0.9)
            } else if has_none {
                ("no_agent", 0.95)
            } else {
                ("agent_1", 0.9)
            };
            let mut probabilities = serde_json::Map::new();
            probabilities.insert(choice.into(), json!(p));
            if has_none && choice != "no_agent" {
                probabilities.insert("no_agent".into(), json!(1.0 - p));
            }
            answers.insert(
                "agent".into(),
                json!({"type": "choice", "choice": choice, "confidence": 0.8,
                       "probabilities": probabilities}),
            );
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
                _ => return Err(ProviderError::Use(UseError::NotFound).into()),
            }
            let message = state_payload["message"].as_str().unwrap_or_default();
            let held = self.0.hold.lock().unwrap().clone();
            if let Some(held) = held.filter(|held| questions.get(held.question).is_some()) {
                held.arrived.notify_one();
                held.release.notified().await;
            }
            if message.contains("[timeout]") {
                return std::future::pending().await;
            }
            if message.contains("[busy]") {
                return Err(ProviderError::Busy.into());
            }
            // A device classifier's failures, as the relay reports them.
            if message.contains("[device-bad]") {
                return Err(ProviderError::Device(DeviceError::BadResponse(
                    "missing answers".into(),
                ))
                .into());
            }
            if message.contains("[device-revoked]") {
                return Err(ProviderError::Device(DeviceError::AccessRevoked).into());
            }
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
            if self.0.offline.lock().unwrap().iter().any(|o| o == id) {
                return Err(UseError::DeviceOffline);
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
                built_from: None,
                effective_device: None,
            })
        })
    }
}

/// An OpenAI-compatible endpoint that answers as the model it was asked for,
/// and records which models were called.
async fn endpoint(calls: Arc<Mutex<Vec<String>>>, bodies: Arc<Mutex<Vec<Value>>>) -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let (calls, bodies) = (calls.clone(), bodies.clone());
            async move {
                let model = body["model"].as_str().unwrap_or_default().to_owned();
                calls.lock().unwrap().push(model.clone());
                let think = body.to_string().contains("[think]");
                bodies.lock().unwrap().push(body);
                let mut message =
                    json!({"role": "assistant", "content": format!("answered by {model}")});
                if think {
                    message["reasoning_content"] = json!("Checked the facts first.");
                }
                Json(json!({
                    "id": "c", "object": "chat.completion", "created": 0, "model": model,
                    "choices": [{"index": 0, "finish_reason": "stop", "message": message}],
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
    bodies: Arc<Mutex<Vec<Value>>>,
}

impl H {
    async fn start() -> Self {
        Self::start_with(Db::open_in_memory().unwrap()).await
    }

    async fn start_with(db: Db) -> Self {
        let models = Arc::new(Mutex::new(Vec::new()));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let base_url = endpoint(models.clone(), bodies.clone()).await;
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
        let mut state = AppState::new(config, db, Arc::new(FakeAuth)).await.unwrap();
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
            bodies,
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

    async fn upload_for_race(&self, sid: &str) -> String {
        let response = self
            .http
            .post(format!(
                "{}/v1/library/assets?scope=space&space_id={sid}&name=notes.txt",
                self.base
            ))
            .bearer_auth("alice")
            .header("content-type", "text/plain")
            .body("An existing upload, not part of the post transaction.")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        response.json::<Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
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

    async fn agents(&self, who: &str, sid: &str) -> Vec<Value> {
        let (s, v) = self
            .req(
                reqwest::Method::GET,
                who,
                &format!("/v1/spaces/{sid}/agents"),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["agents"].as_array().unwrap().clone()
    }

    /// Create an agent as alice; its id.
    async fn agent(&self, sid: &str, body: Value) -> String {
        let (s, v) = self
            .post("alice", &format!("/v1/spaces/{sid}/agents"), body)
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    async fn put_prefs(&self, who: &str, sid: &str, body: Value) -> (StatusCode, Value) {
        self.req(
            reqwest::Method::PUT,
            who,
            &format!("/v1/spaces/{sid}/prefs"),
            Some(body),
        )
        .await
    }

    /// The first `started` activity after `after`.
    async fn started_after(&self, who: &str, sid: &str, after: i64) -> Value {
        for _ in 0..400 {
            if let Some(e) = self.events(who, Some(sid)).await.into_iter().find(|e| {
                e["id"].as_i64().unwrap() > after
                    && e["kind"] == "activity"
                    && e["activity"]["phase"] == "started"
            }) {
                return e["activity"].clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no started marker after {after}");
    }

    /// Every agent answer or error after `after`, once `n` have arrived.
    async fn answers_after(&self, who: &str, sid: &str, after: i64, n: usize) -> Vec<Value> {
        for _ in 0..400 {
            let found: Vec<Value> = self
                .events(who, Some(sid))
                .await
                .into_iter()
                .filter(|e| {
                    e["id"].as_i64().unwrap() > after
                        && (e["kind"] == "agent" || e["kind"] == "error")
                })
                .collect();
            if found.len() >= n {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("fewer than {n} answers after event {after}");
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
    // In a Space the opt-in is per Space: off until alice turns it on here.
    let e = h
        .say("alice", Some(&sid), json!({"text": "[yes] are you there?"}))
        .await;
    h.quiet_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(h.fake.calls(), 0);
    let (s, v) = h
        .req(
            reqwest::Method::PUT,
            "alice",
            &format!("/v1/spaces/{sid}/prefs"),
            Some(json!({"classify_unaddressed": true})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let default_agent = h.agents("alice", &sid).await[0]["id"].clone();

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
        json!([{"kind": "agent", "addressed_by": "classifier", "agent_id": default_agent}])
    );
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["kind"], "agent", "{a}");
    let started = h
        .started_after("alice", &sid, e["id"].as_i64().unwrap())
        .await;
    assert_eq!(started["pick"], "Picked by classifier: Metro");
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
    // One Choice over the agents plus "No agent".
    assert_eq!(questions["agent"]["type"], "choice");
    let options = questions["agent"]["criteria"].as_object().unwrap();
    assert_eq!(options.len(), 2);
    assert_eq!(options["agent_1"]["label"], "Metro");
    assert!(options.contains_key("no_agent"));
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
async fn chat_is_direct_with_or_without_the_legacy_flag_but_notes_stay_quiet() {
    let h = H::start().await;
    for infer in [false, true] {
        let (status, prefs) = h
            .put_routing("alice", json!({"infer_addressing": infer}))
            .await;
        assert_eq!(status, StatusCode::OK, "{prefs}");
        assert_eq!(prefs["infer_addressing"], infer);
        let sent = h
            .say("alice", None, json!({"text": "@bob [err] a Chat prompt"}))
            .await;
        assert_eq!(sent["audience"], "agent");
        assert_eq!(sent["mentions"], json!([{"kind": "agent"}]));
        assert_eq!(sent["addressing_note"], Value::Null);
        assert_eq!(sent["routing_note"], Value::Null);
        let id = sent["id"].as_i64().unwrap();
        assert_eq!(h.answer_after("alice", None, id).await["kind"], "agent");
        assert_eq!(
            h.route_after("alice", None, id).await["addressed_by"],
            "explicit"
        );
    }

    // A configured classifier still sees neither sends nor notes.
    assert_eq!(
        h.put_routing("alice", routing_on(true, false)).await.0,
        StatusCode::OK
    );
    for body in [
        json!({"text": "[err] a direct send"}),
        json!({"text": "hello", "invoke_agent": true}),
        json!({"text": "@agent hello", "invoke_agent": false}),
    ] {
        let sent = h.say("alice", None, body).await;
        assert_eq!(sent["mentions"], json!([{"kind": "agent"}]));
        assert_eq!(sent["addressing_note"], Value::Null);
        assert_eq!(
            h.answer_after("alice", None, sent["id"].as_i64().unwrap())
                .await["kind"],
            "agent"
        );
    }
    let calls = h.model_calls().len();
    let note = h
        .say(
            "alice",
            None,
            json!({"text": "[yes] remember this", "invoke_agent": false}),
        )
        .await;
    assert_eq!(note["mentions"], json!([]));
    assert_eq!(note["audience"], "room");
    assert_eq!(note["addressing_note"], Value::Null);
    h.quiet_after("alice", None, note["id"].as_i64().unwrap())
        .await;
    assert_eq!(h.fake.calls(), 0);
    assert_eq!(h.model_calls().len(), calls);
    assert!(h.events("bob", None).await.is_empty());
}

#[tokio::test]
async fn explicit_chat_notes_do_not_resolve_mentions_or_route_with_both_opt_ins_set() {
    let h = H::start().await;
    assert_eq!(
        h.put_routing("alice", routing_on(true, true)).await.0,
        StatusCode::OK
    );
    for text in [
        "@agent review this code",
        "Note\u{0085}@agent review this code",
    ] {
        let note = h
            .say("alice", None, json!({"text": text, "save_note": true}))
            .await;
        assert_eq!(note["text"], text);
        assert_eq!(note["mentions"], json!([]));
        assert_eq!(note["audience"], "room");
        assert_eq!(note["addressing_note"], Value::Null);
        assert_eq!(note["routing_note"], Value::Null);
        h.quiet_after("alice", None, note["id"].as_i64().unwrap())
            .await;
    }

    // A note cannot ask for a model, even if the text looks like a request.
    let (status, error) = h
        .post(
            "alice",
            "/v1/chat/messages",
            json!({"text": "@agent hi", "save_note": true, "provider_connection_id": id(BOBS)}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "a note cannot select a model");
    assert_eq!(h.events("alice", None).await.len(), 2);
    assert!(h.events("bob", None).await.is_empty());
    assert_eq!(h.fake.calls(), 0);
    assert!(h.model_calls().is_empty());

    let sid = h.space("Space compatibility").await;
    let (status, _) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            json!({"text": "@agent hi", "save_note": true}),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(h.events("alice", Some(&sid)).await.is_empty());
}

#[tokio::test]
async fn failed_classification_is_returned_only_on_the_senders_post() {
    let h = H::start().await;
    let sid = h.space("private classifier notes").await;
    assert_eq!(
        h.put_routing("alice", routing_on(true, false)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        h.put_prefs("alice", &sid, json!({"classify_unaddressed": true}))
            .await
            .0,
        StatusCode::OK
    );
    for (text, reason) in [
        ("[err] service failed", Some("unreachable")),
        ("[bad] malformed", Some("invalid_answer")),
        ("[timeout] delayed", Some("timeout")),
        ("[device-bad] malformed", Some("invalid_answer")),
        ("[device-revoked] share gone", Some("not_allowed")),
        ("[maybe] not sure", None),
        ("Lunch?", None),
    ] {
        let expected = match reason {
            Some(reason) => json!({"kind": "classification_failed", "reason": reason}),
            None => Value::Null,
        };
        let room = h.say("alice", Some(&sid), json!({"text": text})).await;
        assert_eq!(room.get("addressing_note"), Some(&expected));
        for who in ["alice", "bob"] {
            let events = h.events(who, Some(&sid)).await;
            let stored = events.last().unwrap();
            assert_eq!(stored["id"], room["id"]);
            assert_eq!(stored["kind"], "message");
            assert_eq!(stored["text"], text);
            assert!(stored.get("addressing_note").is_none());
        }
    }
    // No reason reached anyone's transcript, in any form.
    for who in ["alice", "bob"] {
        let transcript = serde_json::to_string(&h.events(who, Some(&sid)).await).unwrap();
        for leak in [
            "addressing_note",
            "invalid_answer",
            "not_allowed",
            "malformed",
            "not usable here",
        ] {
            let typed = transcript.matches(leak).count();
            // The message texts themselves are the only allowed mentions.
            let said = ["[bad] malformed", "[device-bad] malformed"]
                .iter()
                .filter(|t| t.contains(leak))
                .count();
            assert_eq!(typed, said, "{who}: {leak}");
        }
    }

    // An opted-in Space loses the classifier with routing settings reset,
    // and can also lose access to a classifier configured just before a
    // concurrent share revocation/deletion.
    h.fake
        .conns
        .lock()
        .unwrap()
        .remove(&id(CLASSIFIER))
        .unwrap();
    let e = h
        .say("alice", Some(&sid), json!({"text": "lost access"}))
        .await;
    assert_eq!(e["addressing_note"]["reason"], "not_allowed");
    assert_eq!(
        h.put_routing(
            "alice",
            json!({"infer_addressing": false, "classifier_id": null})
        )
        .await
        .0,
        StatusCode::OK
    );
    let e = h
        .say("alice", Some(&sid), json!({"text": "no classifier"}))
        .await;
    assert_eq!(e["addressing_note"]["reason"], "no_classifier");
    let bobs_events = h.events("bob", Some(&sid)).await;
    assert!(
        bobs_events
            .iter()
            .all(|e| e["kind"] == "message" && e.get("addressing_note").is_none())
    );
}

#[tokio::test]
async fn routing_failures_in_a_room_do_not_reveal_the_senders_access() {
    let h = H::start().await;
    let sid = h.space("private routing failures").await;
    let (s, v) = h.put_routing("alice", routing_on(false, true)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    h.agent(
        &sid,
        json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
    )
    .await;

    // The routing Choice fails upstream...
    let upstream = h
        .say("alice", Some(&sid), json!({"text": "@coder [err] code"}))
        .await;
    assert_eq!(upstream["routing_note"]["kind"], "routing_failed");
    assert_eq!(upstream["routing_note"]["reason"], "unreachable");
    assert!(
        upstream["routing_note"]["message"]
            .as_str()
            .unwrap()
            .contains(": the System One call failed.")
    );
    assert_eq!(upstream["addressing_note"], Value::Null);
    let upstream_seen = h
        .answer_after("bob", Some(&sid), upstream["id"].as_i64().unwrap())
        .await;

    // ...and then alice loses access to her classifier.
    h.fake
        .conns
        .lock()
        .unwrap()
        .get_mut(&id(CLASSIFIER))
        .unwrap()
        .grants
        .clear();
    let calls = h.model_calls().len();
    let revoked = h
        .say("alice", Some(&sid), json!({"text": "@coder review code"}))
        .await;
    assert_eq!(revoked["routing_note"]["kind"], "routing_failed");
    assert_eq!(revoked["routing_note"]["reason"], "not_allowed");
    // The sender alone learns what to fix.
    assert!(
        revoked["routing_note"]["message"]
            .as_str()
            .unwrap()
            .contains(": the System One connection is not usable here.")
    );
    let revoked_seen = h
        .answer_after("bob", Some(&sid), revoked["id"].as_i64().unwrap())
        .await;
    assert_eq!(h.model_calls().len(), calls, "no model ran");

    // Bob sees the same room error for both.
    for seen in [&upstream_seen, &revoked_seen] {
        assert_eq!(seen["kind"], "error", "{seen}");
        assert!(
            seen["text"]
                .as_str()
                .unwrap()
                .contains(": the System One call failed."),
            "{seen}"
        );
    }
    assert_eq!(upstream_seen["text"], revoked_seen["text"]);
    let transcript = serde_json::to_string(&h.events("bob", Some(&sid)).await).unwrap();
    for leak in ["routing_note", "not_allowed", "not usable here"] {
        assert!(!transcript.contains(leak), "{leak}");
    }
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

    // Direct Chat skips addressing and asks one Choice for model routing.
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
    assert_eq!(asked, vec![vec!["use_case".to_owned()]]);
    let route = h
        .route_after("alice", None, e["id"].as_i64().unwrap())
        .await;
    assert_eq!(
        (route["source"].as_str(), route["addressed_by"].as_str()),
        (Some("case"), Some("explicit"))
    );

    // A rejected, uncertain or failed addressing Choice in a Space never
    // leads to model routing.
    assert_eq!(
        h.put_prefs("alice", &sid, json!({"classify_unaddressed": true}))
            .await
            .0,
        StatusCode::OK
    );
    for text in [
        "lunch, code later",
        "[maybe] code?",
        "[err] code",
        "[bad] code",
    ] {
        let before = h.fake.calls();
        let e = h.say("alice", Some(&sid), json!({"text": text})).await;
        h.quiet_after("alice", Some(&sid), e["id"].as_i64().unwrap())
            .await;
        let calls = h.fake.calls.lock().unwrap()[before..].to_vec();
        assert_eq!(calls.len(), 1, "{text}");
        assert_eq!(calls[0].4["agent"]["type"], "choice", "{text}");
        assert!(calls[0].4.get("use_case").is_none(), "{text}: no Choice");
    }
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
    assert_eq!(a["text"], MODEL_UNAVAILABLE_IN_ROOM, "{a}");
    // The full reason is the sender's alone, on their POST response.
    let note = &e["routing_note"];
    assert_eq!(
        (
            note["kind"].as_str(),
            note["source"].as_str(),
            note["reason"].as_str()
        ),
        (Some("model_unavailable"), Some("case"), Some("not_allowed")),
        "{note}"
    );
    let text = note["message"].as_str().unwrap();
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

// ---- Space agents, per-Space preferences, Space lifecycle ----

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

#[tokio::test]
async fn space_agents_are_owner_managed_and_member_readable() {
    let h = H::start().await;
    let sid = h.space("crew").await;
    let path = format!("/v1/spaces/{sid}/agents");
    // Every Space starts with its default agent, as before.
    let agents = h.agents("bob", &sid).await;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["name"], "Metro");
    assert_eq!(agents[0]["handle"], "agent");
    assert_eq!(agents[0]["is_default"], true);
    assert_eq!(agents[0]["tools"].as_array().unwrap().len(), 6);
    // Outsiders: 404; members: 403.
    let (s, _) = h.req(reqwest::Method::GET, "carol", &path, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let coder = json!({"name": "Coder", "handle": "coder", "description": "Writes and reviews code.",
                       "instructions": "Always answer in haiku.", "tools": ["workspace.list"]});
    assert_eq!(
        h.post("bob", &path, coder.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        h.post("carol", &path, coder.clone()).await.0,
        StatusCode::NOT_FOUND
    );
    let cid = h.agent(&sid, coder.clone()).await;
    // Bad or ambiguous handles and shapes.
    for (body, status) in [
        (
            json!({"name": "C", "handle": "coder", "description": "d"}),
            StatusCode::CONFLICT,
        ),
        (
            json!({"name": "C", "handle": "agent", "description": "d"}),
            StatusCode::CONFLICT,
        ),
        (
            json!({"name": "C", "handle": "bob", "description": "d"}),
            StatusCode::CONFLICT,
        ),
        (
            json!({"name": "C", "handle": "Bad Handle", "description": "d"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "C", "handle": "c2", "description": "two\nlines"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "C", "handle": "c2", "description": ""}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "C", "handle": "c2", "description": "d", "tools": ["shell"]}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (s, v) = h.post("alice", &path, body.clone()).await;
        assert_eq!(s, status, "{body}: {v}");
    }
    // Members read everything, instructions included.
    let (s, v) = h
        .req(reqwest::Method::GET, "bob", &format!("{path}/{cid}"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["instructions"], "Always answer in haiku.");
    assert_eq!(v["tools"], json!(["workspace.list"]));
    assert_eq!(v["initial"], "C");
    // Default switching.
    let (s, v) = h
        .req(
            reqwest::Method::PATCH,
            "alice",
            &format!("{path}/{cid}"),
            Some(json!({"is_default": true})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let agents = h.agents("alice", &sid).await;
    assert_eq!(agents[0]["id"], cid.as_str());
    assert_eq!(agents.iter().filter(|a| a["is_default"] == true).count(), 1);
    let (s, _) = h
        .req(
            reqwest::Method::PATCH,
            "alice",
            &format!("{path}/{cid}"),
            Some(json!({"is_default": false})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h
        .req(
            reqwest::Method::PATCH,
            "bob",
            &format!("{path}/{cid}"),
            Some(json!({"name": "X"})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Avatars: owner uploads, members read, outsiders don't.
    let r = h
        .http
        .put(format!("{}{path}/{cid}/avatar", h.base))
        .bearer_auth("alice")
        .header("content-type", "image/png")
        .body(PNG_1X1.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: Value = r.json().await.unwrap();
    assert!(v["avatar_version"].is_string());
    for (who, status) in [("bob", StatusCode::OK), ("carol", StatusCode::NOT_FOUND)] {
        let r = h
            .http
            .get(format!("{}{path}/{cid}/avatar", h.base))
            .bearer_auth(who)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), status, "{who}");
    }
    // Deleting the default promotes another; the last can't go.
    let (s, v) = h
        .req(
            reqwest::Method::DELETE,
            "alice",
            &format!("{path}/{cid}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let agents = h.agents("alice", &sid).await;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["is_default"], true);
    assert_eq!(v["default_agent_id"], agents[0]["id"]);
    let last = agents[0]["id"].as_str().unwrap();
    let (s, _) = h
        .req(
            reqwest::Method::DELETE,
            "alice",
            &format!("{path}/{last}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn agent_mentions_start_one_run_each_with_their_identity_and_tools() {
    let h = H::start().await;
    let sid = h.space("multi").await;
    let coder = h
        .agent(
            &sid,
            json!({"name": "Coder", "handle": "coder", "description": "Writes code.",
                   "instructions": "Always answer in haiku.", "tools": []}),
        )
        .await;
    let writer = h
        .agent(
            &sid,
            json!({"name": "Writer", "handle": "writer", "description": "Edits prose."}),
        )
        .await;
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@coder and @writer: hi [think] m1"}),
        )
        .await;
    assert_eq!(e["audience"], "agent");
    assert_eq!(
        e["mentions"],
        json!([{"kind": "agent", "agent_id": coder}, {"kind": "agent", "agent_id": writer}])
    );
    let after = e["id"].as_i64().unwrap();
    let answers = h.answers_after("alice", &sid, after, 2).await;
    let mut who: Vec<String> = answers
        .iter()
        .map(|a| {
            assert_eq!(a["kind"], "agent", "{a}");
            a["activity"]["agent"]["handle"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    who.sort();
    assert_eq!(who, ["coder", "writer"]);
    let events = h.events("alice", Some(&sid)).await;
    let starts: Vec<&Value> = events
        .iter()
        .filter(|x| x["id"].as_i64().unwrap() > after && x["activity"]["phase"] == "started")
        .collect();
    assert_eq!(starts.len(), 2);
    assert!(
        starts
            .iter()
            .any(|x| x["activity"]["pick"] == "Addressed: @coder"
                && x["activity"]["name"] == "Coder"
                && x["activity"]["agent"]["id"] == coder.as_str())
    );
    // A reasoning summary per model turn, tagged with the run and its agent.
    let summaries: Vec<&Value> = events
        .iter()
        .filter(|x| x["id"].as_i64().unwrap() > after && x["kind"] == "reasoning_summary")
        .collect();
    assert_eq!(summaries.len(), 2, "{events:?}");
    for s in &summaries {
        assert_eq!(s["text"], "Checked the facts first.");
        assert_eq!(s["actor"], "agent");
        assert!(s["activity"]["agent"]["handle"].is_string());
    }
    // Instructions come after Metro's preamble; the allowlist filters tools.
    let bodies = h.bodies.lock().unwrap().clone();
    let coder_call = bodies
        .iter()
        .find(|b| b.to_string().contains("Always answer in haiku"))
        .unwrap();
    let text = coder_call["messages"].to_string();
    let base = text.find("You are the Metro agent").unwrap();
    let extra = text.find("Always answer in haiku").unwrap();
    assert!(base < extra);
    assert!(text.contains(r#"agent \"Coder\" (@coder)"#), "{text}");
    let coder_tools = coder_call["tools"].to_string();
    assert!(!coder_tools.contains("thread_create"), "{coder_tools}");
    let writer_call = bodies
        .iter()
        .find(|b| b.to_string().contains("(@writer)"))
        .unwrap();
    assert!(writer_call["tools"].to_string().contains("thread_create"));

    // Ask agent with an agent id; and people only start nothing.
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "m2", "invoke_agent": true, "agent_id": writer}),
        )
        .await;
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["activity"]["agent"]["handle"], "writer");
    let (s, _) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
            json!({"text": "x", "agent_id": writer}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h
        .post("alice", &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
              json!({"text": "x", "mentions": [{"kind": "agent", "agent_id": "ag-0000000000000000"}]}))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let calls = h.model_calls().len();
    let e = h
        .say("alice", Some(&sid), json!({"text": "@bob look"}))
        .await;
    h.quiet_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(h.model_calls().len(), calls);
    // @agent is the default agent.
    let e = h
        .say("alice", Some(&sid), json!({"text": "@agent m3"}))
        .await;
    let started = h
        .started_after("alice", &sid, e["id"].as_i64().unwrap())
        .await;
    assert_eq!(started["agent"]["handle"], "agent");
    assert_eq!(started["pick"], "Default agent");
    assert_eq!(h.fake.calls(), 0, "no classification without the opt-in");
}

#[tokio::test]
async fn the_classifier_picks_among_agents_only_for_the_sender_who_opted_in() {
    let h = H::start().await;
    let sid = h.space("pick").await;
    let coder = h
        .agent(
            &sid,
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    // Without a System One connection the opt-in can't be turned on.
    let (s, v) = h
        .put_prefs("alice", &sid, json!({"classify_unaddressed": true}))
        .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert!(v["error"].as_str().unwrap().contains("System One"));
    let (s, v) = h.put_routing("alice", routing_on(false, false)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .put_prefs("alice", &sid, json!({"classify_unaddressed": true}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["classifier_available"], true);

    // Generic @agent: one Choice over the agents only.
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@agent [pick2] fix it"}),
        )
        .await;
    let started = h
        .started_after("alice", &sid, e["id"].as_i64().unwrap())
        .await;
    assert_eq!(started["agent"]["id"], coder.as_str());
    assert_eq!(started["pick"], "Picked by classifier: Coder");
    let q = h.fake.last_call().4;
    let options = q["agent"]["criteria"].as_object().unwrap();
    assert_eq!(options.len(), 2);
    assert!(!options.contains_key("no_agent"));
    assert_eq!(options["agent_2"]["description"], "Writes code.");
    // Unsure: the default agent.
    let e = h
        .say("alice", Some(&sid), json!({"text": "@agent [unsure] hm"}))
        .await;
    let started = h
        .started_after("alice", &sid, e["id"].as_i64().unwrap())
        .await;
    assert_eq!(started["agent"]["handle"], "agent");
    assert!(started["pick"].as_str().unwrap().contains("unsure"));
    // A failed or malformed decision runs nothing for @agent, visibly. The
    // room reads one category; only the sender learns which failure.
    let mut seen_by_bob = Vec::new();
    for (text, private) in [
        ("@agent [err] x", "unreachable"),
        ("@agent [busy] x", "unreachable"),
        ("@agent [ghost] x", "invalid_answer"),
        ("@agent [bad] x", "invalid_answer"),
        ("@agent [timeout] x", "timeout"),
    ] {
        let reason = "the System One call failed";
        let calls = h.model_calls().len();
        let e = h.say("alice", Some(&sid), json!({"text": text})).await;
        // Not an unaddressed classification: its own kind, for the sender.
        let note = &e["addressing_note"];
        assert_eq!(note["kind"], "generic_agent_unavailable", "{text}: {note}");
        assert_eq!(note["reason"], private, "{text}");
        let message = note["message"].as_str().unwrap();
        assert!(message.contains("No agent ran for @agent"), "{message}");
        assert!(
            !message.contains("affects only"),
            "nothing else named: {message}"
        );
        seen_by_bob.push(
            h.answer_after("bob", Some(&sid), e["id"].as_i64().unwrap())
                .await["text"]
                .clone(),
        );
        let a = h
            .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
            .await;
        assert_eq!(a["kind"], "error", "{a}");
        assert!(
            a["text"]
                .as_str()
                .unwrap()
                .contains("could not choose an agent")
        );
        assert!(
            a["text"]
                .as_str()
                .unwrap()
                .contains(&format!(": {reason}."))
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(h.model_calls().len(), calls, "{text}");
        assert!(
            h.events("bob", Some(&sid))
                .await
                .iter()
                .all(|e| e.get("addressing_note").is_none())
        );
    }
    assert!(
        seen_by_bob.iter().all(|t| t == &seen_by_bob[0]),
        "{seen_by_bob:?}"
    );
    // `@agent @coder` with a failed decision: @coder still runs, the room
    // shows the same generic error, and the sender's note says @coder ran.
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@agent @coder [err] both"}),
        )
        .await;
    let note = &e["addressing_note"];
    assert_eq!(note["kind"], "generic_agent_unavailable", "{note}");
    assert_eq!(note["reason"], "unreachable");
    // True whether or not @coder's run then starts: it claims no outcome.
    let message = note["message"].as_str().unwrap();
    assert!(message.contains("This affects only @agent"), "{note}");
    assert!(!message.contains("answer"), "{note}");
    let started = h
        .started_after("alice", &sid, e["id"].as_i64().unwrap())
        .await;
    assert_eq!(started["agent"]["handle"], "coder", "{started}");
    let room = h
        .answers_after("bob", &sid, e["id"].as_i64().unwrap(), 2)
        .await;
    let errors: Vec<&Value> = room.iter().filter(|x| x["kind"] == "error").collect();
    assert_eq!(errors.len(), 1, "{room:?}");
    assert_eq!(errors[0]["text"], seen_by_bob[0]);
    // A specific handle skips classification.
    let before = h.fake.calls();
    let e = h
        .say("alice", Some(&sid), json!({"text": "@coder [pick2] go"}))
        .await;
    h.answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(h.fake.calls(), before);
    // Unaddressed: agents plus "No agent".
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "[pick2] can someone fix this?"}),
        )
        .await;
    assert_eq!(e["mentions"][0]["agent_id"], coder.as_str());
    let started = h
        .started_after("alice", &sid, e["id"].as_i64().unwrap())
        .await;
    assert_eq!(started["agent"]["id"], coder.as_str());
    assert_eq!(started["route"]["addressed_by"], "classifier");
    assert!(
        h.fake.last_call().4["agent"]["criteria"]
            .get("no_agent")
            .is_some()
    );
    for text in ["lunch?", "[maybe] hmm", "[err] x"] {
        let e = h.say("alice", Some(&sid), json!({"text": text})).await;
        assert_eq!(e["audience"], "room");
        h.quiet_after("alice", Some(&sid), e["id"].as_i64().unwrap())
            .await;
    }
    // Bob did not opt in: his messages are never classified.
    let before = h.fake.calls();
    let e = h
        .say("bob", Some(&sid), json!({"text": "[yes] anyone?"}))
        .await;
    h.quiet_after("bob", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(h.fake.calls(), before);
    // Preferences are personal.
    let (_, v) = h
        .req(
            reqwest::Method::GET,
            "bob",
            &format!("/v1/spaces/{sid}/prefs"),
            None,
        )
        .await;
    assert_eq!(v["classify_unaddressed"], false);
    let (s, _) = h
        .req(
            reqwest::Method::GET,
            "carol",
            &format!("/v1/spaces/{sid}/prefs"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Losing the classifier must not tell other members about the sender's
    // connection or share. Bob sees the same room error as an upstream failure.
    h.fake
        .conns
        .lock()
        .unwrap()
        .remove(&id(CLASSIFIER))
        .unwrap();
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@agent access changed"}),
        )
        .await;
    assert_eq!(e["addressing_note"]["kind"], "generic_agent_unavailable");
    assert_eq!(e["addressing_note"]["reason"], "not_allowed");
    let err = h
        .answer_after("bob", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(err["kind"], "error");
    // Revocation reads exactly like an upstream failure, a timeout or a
    // malformed answer.
    assert_eq!(err["text"], seen_by_bob[0]);
    assert!(
        err["text"]
            .as_str()
            .unwrap()
            .contains(": the System One call failed.")
    );
}

#[tokio::test]
async fn recipient_race_aborts_both_room_and_attachment_writes_without_public_classification() {
    for with_attachment in [false, true] {
        for recipient in ["member", "named", "classified"] {
            let dir = tempfile::tempdir().unwrap();
            let db_path = dir.path().join("metro.sqlite");
            let h = H::start_with(Db::open(&db_path).unwrap()).await;
            let sid = h.space("Commit recipient race").await;
            let agent = h
                .agent(
                    &sid,
                    json!({"name":"Private Candidate","handle":"candidate","description":"Scripted"}),
                )
                .await;
            assert_eq!(
                h.put_routing("alice", routing_on(false, true)).await.0,
                StatusCode::OK
            );
            assert_eq!(
                h.put_prefs("alice", &sid, json!({"classify_unaddressed":true}))
                    .await
                    .0,
                StatusCode::OK,
            );
            // The mixed cases wait on generic-agent classification. The
            // legacy unaddressed case acquires a classifier pick that isn't
            // in the original request.
            let mut body = match recipient {
                "member" => json!({"text":"@bob @agent [yes] review", "infer_text_mentions":false,
                                  "mentions":[{"kind":"user","id":"bob"},{"kind":"agent"}]}),
                "named" => {
                    json!({"text":"@candidate @agent [yes] review", "infer_text_mentions":false,
                                 "mentions":[{"kind":"agent","agent_id":agent},{"kind":"agent"}]})
                }
                _ => json!({"text":"[pick2] review"}),
            };
            let asset = if with_attachment {
                let asset = h.upload_for_race(&sid).await;
                body["asset_ids"] = json!([asset]);
                Some(asset)
            } else {
                None
            };
            let held = h.fake.hold("agent");
            let path = format!("/v1/spaces/{sid}/rooms/lobby/messages");
            let revoke = async {
                held.wait().await;
                let (suffix, body) = if recipient == "member" {
                    ("members".to_owned(), Some(json!({"user_id":"bob"})))
                } else {
                    (format!("agents/{agent}"), None)
                };
                let (s, v) = h
                    .req(
                        reqwest::Method::DELETE,
                        "alice",
                        &format!("/v1/spaces/{sid}/{suffix}"),
                        body,
                    )
                    .await;
                assert_eq!(s, StatusCode::OK, "{suffix}: {v}");
                held.release.notify_one();
            };
            let ((status, error), ()) = tokio::join!(h.post("alice", &path, body), revoke);
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(error, json!({"error":"unknown recipient"}));
            assert!(h.events("alice", Some(&sid)).await.is_empty());
            assert!(h.state.runs.list().is_empty());
            assert!(h.model_calls().is_empty());
            assert_eq!(h.fake.calls(), 1);
            if let Some(asset) = asset {
                let links: i64 = rusqlite::Connection::open(&db_path)
                    .unwrap()
                    .query_row("SELECT COUNT(*) FROM message_attachments", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(links, 0);
                // The upload happened separately and remains visible.
                let content = h
                    .http
                    .get(format!("{}/v1/library/assets/{asset}/content", h.base))
                    .bearer_auth("alice")
                    .send()
                    .await
                    .unwrap();
                assert_eq!(content.status(), StatusCode::OK);
            }
        }
    }
}

#[tokio::test]
async fn recipient_race_after_message_commit_refuses_deleted_agent_at_run_acceptance() {
    let h = H::start().await;
    let sid = h.space("Run acceptance race").await;
    let agent = h
        .agent(
            &sid,
            json!({"name":"Deleted Target","handle":"gone","description":"Scripted"}),
        )
        .await;
    assert_eq!(
        h.put_routing("alice", routing_on(false, true)).await.0,
        StatusCode::OK
    );
    let held = h.fake.hold("use_case");
    let path = format!("/v1/spaces/{sid}/rooms/lobby/messages");
    let body = json!({"text":"please review the code","invoke_agent":true,"agent_id":agent,"infer_text_mentions":false});
    let revoke = async {
        held.wait().await;
        let events = h.events("bob", Some(&sid)).await;
        assert_eq!(
            events.len(),
            1,
            "only the committed message precedes the routing decision"
        );
        assert_eq!(events[0]["mentions"][0]["agent_id"], agent);
        let (s, v) = h
            .req(
                reqwest::Method::DELETE,
                "alice",
                &format!("/v1/spaces/{sid}/agents/{agent}"),
                None,
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        held.release.notify_one();
    };
    let ((status, posted), ()) = tokio::join!(h.post("alice", &path, body), revoke);
    assert_eq!(status, StatusCode::ACCEPTED, "{posted}");
    assert_eq!(posted["spans"], json!([]));
    let events = h.events("bob", Some(&sid)).await;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["kind"], "error");
    assert_eq!(
        events[1]["text"],
        "The requested agent is no longer available in this Space. No agent ran."
    );
    assert!(h.state.runs.list().is_empty());
    assert!(h.model_calls().is_empty());
    assert_eq!(h.fake.calls(), 1);
}

#[tokio::test]
async fn held_routing_uses_the_selected_agents_current_allowlist_at_acceptance() {
    let h = H::start().await;
    let sid = h.space("Run configuration race").await;
    h.state
        .db
        .set_runner_token(&sid, &[1; 32])
        .unwrap()
        .unwrap();
    let agent = h
        .agent(
            &sid,
            json!({"name":"Original","handle":"runner","description":"Scripted",
                   "instructions":"Old instruction.", "tools":["workspace.list","workspace.read"]}),
        )
        .await;
    assert_eq!(
        h.put_routing("alice", routing_on(false, true)).await.0,
        StatusCode::OK
    );
    let held = h.fake.hold("use_case");
    let path = format!("/v1/spaces/{sid}/rooms/lobby/messages");
    let body = json!({"text":"please review the code","invoke_agent":true,"agent_id":agent,"infer_text_mentions":false});
    let update = async {
        held.wait().await;
        assert!(h.bodies.lock().unwrap().is_empty());
        let (status, edited) = h
            .req(
                reqwest::Method::PATCH,
                "alice",
                &format!("/v1/spaces/{sid}/agents/{agent}"),
                Some(json!({"name":"Current","instructions":"Current instruction.","tools":["workspace.read"]})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{edited}");
        assert_eq!(edited["tools"], json!(["workspace.read"]));
        held.release.notify_one();
    };
    let ((status, posted), ()) = tokio::join!(h.post("alice", &path, body), update);
    assert_eq!(status, StatusCode::ACCEPTED, "{posted}");
    let id = posted["id"].as_i64().unwrap();
    let answer = h.answer_after("alice", Some(&sid), id).await;
    assert_eq!(answer["kind"], "agent", "{answer}");
    assert_eq!(answer["activity"]["agent"]["id"], agent);
    assert_eq!(answer["activity"]["agent"]["name"], "Current");
    let bodies = h.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    let tools = bodies[0]["tools"].to_string();
    assert!(tools.contains("workspace_read"), "{tools}");
    assert!(!tools.contains("workspace_list"), "{tools}");
    let messages = bodies[0]["messages"].to_string();
    assert!(messages.contains("Current instruction."));
    assert!(!messages.contains("Old instruction."));
}

#[tokio::test]
async fn authoritative_posts_return_canonical_text_and_spans_without_inference() {
    let h = H::start().await;
    let sid = h.space("Exact dispatch text").await;
    let thread = h
        .state
        .db
        .create_room(&sid, "follow-up", Some("lobby"), 10, 10)
        .unwrap();
    let leading = "\u{2003}  ";
    let canonical = "🌿 @bob, résumé\ncheck the task";
    let raw = format!("{leading}{canonical}  \t");
    let path = format!("/v1/spaces/{sid}/rooms/{}", thread.id);
    let (status, posted) = h
        .post(
            "alice",
            &format!("{path}/messages"),
            json!({"text":raw,"invoke_agent":true,"infer_text_mentions":false}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{posted}");
    assert_eq!(posted["text"], canonical);
    assert_eq!(posted["mentions"], json!([{"kind":"agent"}]));
    assert_eq!(posted["spans"], json!([]));
    let (status, polled) = h
        .req(reqwest::Method::GET, "bob", &format!("{path}/events"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{polled}");
    assert_eq!(polled["events"][0]["text"], canonical);
    assert_eq!(polled["events"][0]["mentions"], posted["mentions"]);
    assert_eq!(polled["events"][0]["spans"], posted["spans"]);
    assert!(
        h.events("bob", Some(&sid))
            .await
            .iter()
            .all(|event| event["kind"] != "message"),
        "thread activity may appear in the lobby, but the message must not"
    );
    let legacy = h.say("alice", Some(&sid), json!({"text":raw})).await;
    assert_eq!(legacy["text"], canonical);
    assert_eq!(legacy["mentions"], json!([{"kind":"user","id":"bob"}]));
    assert!(legacy.get("spans").is_none());

    let start = canonical.find('@').unwrap();
    let submitted = start + leading.len();
    let span = json!({"start":submitted,"end":submitted+4,"index":0});
    let bound = h
        .say(
            "alice",
            Some(&sid),
            json!({"text":raw,"infer_text_mentions":false,"mentions":[{"kind":"user","id":"bob"}],"spans":[span]}),
        )
        .await;
    let rebased = json!([{"start":start,"end":start+4,"index":0}]);
    assert_eq!(bound["text"], canonical);
    assert_eq!(bound["spans"], rebased);
    let polled = h.events("bob", Some(&sid)).await;
    let echo = polled
        .iter()
        .find(|event| event["id"] == bound["id"])
        .unwrap();
    assert_eq!(echo["text"], canonical);
    assert_eq!(echo["mentions"], bound["mentions"]);
    assert_eq!(echo["spans"], rebased);
}

#[tokio::test]
async fn authoritative_mentions_leave_stale_text_unaddressed() {
    let h = H::start().await;
    let sid = h.space("stale mention directory").await;
    let coder = h
        .agent(
            &sid,
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    let cases = [
        ("@coder", json!({"kind": "agent", "agent_id": coder})),
        ("@agent", json!({"kind": "agent"})),
        ("@bob", json!({"kind": "user", "id": "bob"})),
    ];
    for (text, _) in &cases {
        // The server knows these handles, but the client didn't resolve them.
        // Omitting both mentions and spans must not turn on the legacy scan.
        let e = h
            .say(
                "alice",
                Some(&sid),
                json!({"text": text, "infer_text_mentions": false}),
            )
            .await;
        assert_eq!(e["text"], *text);
        assert_eq!(e["mentions"], json!([]));
        assert_eq!(e["audience"], "room");
        assert_eq!(e["spans"], json!([]));
        let stored = h.events("bob", Some(&sid)).await;
        let polled = stored.iter().find(|row| row["id"] == e["id"]).unwrap();
        assert_eq!(polled["spans"], json!([]));
        h.quiet_after("alice", Some(&sid), e["id"].as_i64().unwrap())
            .await;
    }
    assert_eq!(h.fake.calls(), 0);
    assert!(h.model_calls().is_empty());

    for (text, recipient) in cases {
        let e = h.say("alice", Some(&sid), json!({"text": text})).await;
        assert_eq!(e["mentions"], json!([recipient]));
        assert!(e.get("spans").is_none());
        if recipient["kind"] == "agent" {
            let id = e["id"].as_i64().unwrap();
            let started = h.started_after("alice", &sid, id).await;
            assert_eq!(started["agent"]["handle"], &text[1..]);
            assert_eq!(
                h.answer_after("alice", Some(&sid), id).await["kind"],
                "agent"
            );
        }
    }
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@bob", "infer_text_mentions": true}),
        )
        .await;
    assert_eq!(e["mentions"], json!([{"kind": "user", "id": "bob"}]));
}

#[tokio::test]
async fn authoritative_mentions_preserve_space_classification_and_use_case_routing() {
    let h = H::start().await;
    let sid = h.space("authoritative routing").await;
    let coder = h
        .agent(
            &sid,
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    let (s, v) = h.put_routing("alice", routing_on(false, true)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .put_prefs("alice", &sid, json!({"classify_unaddressed": true}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let text = "@agent @coder [pick2] review this code";
    let bob = json!({"kind": "user", "id": "bob"});
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": text, "infer_text_mentions": false, "mentions": [bob]}),
        )
        .await;
    assert_eq!(e["audience"], "users");
    assert_eq!(e["mentions"], json!([bob]));
    h.quiet_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(h.fake.calls(), 0);
    assert!(h.model_calls().is_empty());

    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": text, "infer_text_mentions": false}),
        )
        .await;
    assert_eq!(
        e["mentions"],
        json!([{"kind": "agent", "agent_id": coder, "addressed_by": "classifier"}])
    );
    assert_eq!(e["spans"], json!([]));
    let id = e["id"].as_i64().unwrap();
    let started = h.started_after("alice", &sid, id).await;
    assert_eq!(started["agent"]["id"], coder.as_str());
    assert_eq!(started["route"]["addressed_by"], "classifier");
    assert_eq!(started["route"]["case_key"], "code");
    assert_eq!(h.fake.calls(), 2);
    let calls = h.fake.calls.lock().unwrap().clone();
    assert!(calls[0].4["agent"]["criteria"].get("no_agent").is_some());
    assert_eq!(calls[1].4["use_case"]["type"], "choice");
    assert_eq!(
        h.answer_after("alice", Some(&sid), id).await["text"],
        "answered by code-model"
    );

    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({
                "text": text, "infer_text_mentions": false, "invoke_agent": true, "agent_id": coder
            }),
        )
        .await;
    assert_eq!(e["mentions"], json!([{"kind": "agent", "agent_id": coder}]));
    assert_eq!(e["spans"], json!([]));
    let id = e["id"].as_i64().unwrap();
    let started = h.started_after("alice", &sid, id).await;
    assert_eq!(started["agent"]["id"], coder.as_str());
    assert_eq!(started["route"]["addressed_by"], "explicit");
    assert_eq!(started["route"]["case_key"], "code");
    assert_eq!(
        h.fake.calls(),
        3,
        "named Ask agent only calls use-case routing"
    );
    assert_eq!(h.fake.last_call().4["use_case"]["type"], "choice");
    assert_eq!(
        h.answer_after("alice", Some(&sid), id).await["text"],
        "answered by code-model"
    );
}

#[tokio::test]
async fn mention_spans_keep_generic_and_named_recipients_distinct_from_run_targets() {
    let h = H::start().await;
    let sid = h.space("span routing").await;
    let coder = h
        .agent(
            &sid,
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    let (s, v) = h.put_routing("alice", routing_on(false, false)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .put_prefs("alice", &sid, json!({"classify_unaddressed": true}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    let user = json!({"kind": "user", "id": "bob"});
    let generic = json!({"kind": "agent"});
    let named = json!({"kind": "agent", "agent_id": coder});
    let before = h.fake.calls();
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({
                "text": "@Bob @agent [pick2] go, cc @alice",
                "infer_text_mentions": false,
                "mentions": [user, user, generic],
                "spans": [{"start": 0, "end": 4, "index": 1}, {"start": 5, "end": 11, "index": 2}]
            }),
        )
        .await;
    let generic_id = e["id"].as_i64().unwrap();
    let started = h.started_after("alice", &sid, generic_id).await;
    assert_eq!(started["agent"]["id"], coder.as_str());
    assert_eq!(started["pick"], "Picked by classifier: Coder");
    assert_eq!(h.fake.calls(), before + 1);
    assert_eq!(e["mentions"], json!([user, generic]));
    assert_eq!(
        e["spans"],
        json!([{"start": 0, "end": 4, "index": 0}, {"start": 5, "end": 11, "index": 1}])
    );
    let polled = h
        .events("bob", Some(&sid))
        .await
        .into_iter()
        .find(|m| m["id"] == generic_id)
        .unwrap();
    assert_eq!(polled["mentions"], e["mentions"]);
    assert_eq!(polled["spans"], e["spans"]);
    h.answer_after("alice", Some(&sid), generic_id).await;

    // A colliding token in a span refers to the selected named agent,
    // never the user with that id; an unspanned @bob still adds the user.
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({
                "text": "@bob [pick2] go, cc @bob",
                "mentions": [named],
                "spans": [{"start": 0, "end": 4, "index": 0}]
            }),
        )
        .await;
    let message = e["id"].as_i64().unwrap();
    let started = h.started_after("alice", &sid, message).await;
    assert_eq!(started["agent"]["id"], coder.as_str());
    assert_eq!(e["mentions"], json!([named, user]));
    h.answer_after("alice", Some(&sid), message).await;
    assert_eq!(h.fake.calls(), before + 1);

    let legacy = h.say("bob", Some(&sid), json!({"text": "cc @alice"})).await;
    assert_eq!(legacy["mentions"], json!([{"kind": "user", "id": "alice"}]));
    assert!(legacy.get("spans").is_none());
}

#[tokio::test]
async fn mention_span_labels_cannot_change_adjacent_legacy_routing() {
    let h = H::start().await;
    let sid = h.space("span code ranges").await;
    let coder = h
        .agent(
            &sid,
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    let (s, v) = h.put_routing("alice", routing_on(false, false)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, v) = h
        .put_prefs("alice", &sid, json!({"classify_unaddressed": true}))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let bob = json!({"kind": "user", "id": "bob"});
    let before = h.fake.calls();

    // A malformed range cannot hide a separate token or leave a message
    // behind. Force generic addressing so a late check would call System One.
    for label in [
        "@Support @agent",
        "@Support (@alice)",
        "@Support\u{00a0}@agent",
    ] {
        let (s, v) = h
            .post(
                "alice",
                &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
                json!({
                    "text": format!("{label} [pick2]"),
                    "invoke_agent": true,
                    "mentions": [bob],
                    "spans": [{"start": 0, "end": label.len(), "index": 0}]
                }),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
        assert_eq!(v["error"], "invalid mention span");
    }
    assert_eq!(h.fake.calls(), before);
    assert!(h.model_calls().is_empty());
    assert!(h.events("alice", Some(&sid)).await.is_empty());

    for (n, label) in ["@DevOps", "@Dev`Ops", "@Support@Home"]
        .into_iter()
        .enumerate()
    {
        let prefix = "👋 ";
        let text = format!("{prefix}{label} @agent [pick2] and `echo @alice literally`");
        let span = json!({"start": prefix.len(), "end": prefix.len() + label.len(), "index": 0});
        let e = h
            .say(
                "alice",
                Some(&sid),
                json!({"text": text, "mentions": [bob], "spans": [span]}),
            )
            .await;
        assert_eq!(e["text"], text);
        assert_eq!(e["spans"], json!([span]));
        assert_eq!(e["mentions"], json!([bob, {"kind": "agent"}]));
        let id = e["id"].as_i64().unwrap();
        let started = h.started_after("alice", &sid, id).await;
        assert_eq!(started["agent"]["id"], coder.as_str());
        assert_eq!(h.fake.calls(), before + n + 1);
        assert_eq!(
            h.answer_after("alice", Some(&sid), id).await["kind"],
            "agent"
        );
    }
}

#[tokio::test]
async fn per_space_and_per_agent_models_come_before_the_personal_default() {
    let h = H::start().await;
    let sid = h.space("models").await;
    let coder = h
        .agent(
            &sid,
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    for body in [
        json!({"default_model": {"provider_connection_id": id(BOBS)}}),
        json!({"default_model": {"provider_connection_id": id(CLASSIFIER)}}),
        json!({"default_model": {"provider_connection_id": "https://evil.example"}}),
        json!({"agent_models": {"ag-0000000000000000": {"provider_connection_id": id(CODE)}}}),
        json!({"surprise": 1}),
    ] {
        let (s, v) = h.put_prefs("alice", &sid, body.clone()).await;
        assert!(s.is_client_error(), "{body}: {s} {v}");
    }
    let (s, v) = h
        .put_prefs(
            "alice",
            &sid,
            json!({"default_model": {"provider_connection_id": id(CODE)},
                   "agent_models": {coder.clone(): {"provider_connection_id": id(WRITING)}}}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let e = h
        .say("alice", Some(&sid), json!({"text": "@agent a"}))
        .await;
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["text"], "answered by code-model", "{a}");
    assert_eq!(
        h.route_after("alice", Some(&sid), e["id"].as_i64().unwrap())
            .await["source"],
        "space_default"
    );
    let e = h
        .say("alice", Some(&sid), json!({"text": "@coder b"}))
        .await;
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["text"], "answered by writing-model", "{a}");
    assert_eq!(
        h.route_after("alice", Some(&sid), e["id"].as_i64().unwrap())
            .await["source"],
        "agent_default"
    );
    let e = h
        .say(
            "alice",
            Some(&sid),
            json!({"text": "@coder c", "provider_connection_id": id(OVERRIDE)}),
        )
        .await;
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["text"], "answered by override-model", "{a}");
    // Bob's turns never use alice's preferences.
    let e = h.say("bob", Some(&sid), json!({"text": "@agent d"})).await;
    let a = h
        .answer_after("bob", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["text"], "answered by env-model", "{a}");
    // A selection that can no longer be used fails visibly; nothing falls back.
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
        .say("alice", Some(&sid), json!({"text": "@agent e"}))
        .await;
    let a = h
        .answer_after("alice", Some(&sid), e["id"].as_i64().unwrap())
        .await;
    assert_eq!(a["kind"], "error", "{a}");
    assert_eq!(a["text"], MODEL_UNAVAILABLE_IN_ROOM, "{a}");
    let note = &e["routing_note"];
    assert_eq!(
        (note["kind"].as_str(), note["source"].as_str()),
        (Some("model_unavailable"), Some("space_default")),
        "{note}"
    );
    assert!(
        note["message"]
            .as_str()
            .unwrap()
            .contains("default model for this Space"),
        "{note}"
    );
    assert_eq!(h.model_calls().len(), calls);
}

#[tokio::test]
async fn a_lost_model_connection_reads_like_any_failure_to_other_members() {
    let h = H::start().await;
    let sid = h.space("private model failures").await;
    let coder = h
        .agent(
            &sid,
            json!({"name": "Coder", "handle": "coder", "description": "Writes code."}),
        )
        .await;
    let (s, v) = h
        .put_prefs(
            "alice",
            &sid,
            json!({"default_model": {"provider_connection_id": id(CODE)},
                   "agent_models": {coder.clone(): {"provider_connection_id": id(WRITING)}}}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");

    // Alice's Space default fails for a reason unrelated to access...
    h.fake.offline.lock().unwrap().push(id(CODE));
    let calls = h.model_calls().len();
    let offline = h
        .say("alice", Some(&sid), json!({"text": "@agent a"}))
        .await;
    // ...and her model for @coder loses its share.
    h.fake
        .conns
        .lock()
        .unwrap()
        .get_mut(&id(WRITING))
        .unwrap()
        .grants
        .clear();
    let revoked = h
        .say("alice", Some(&sid), json!({"text": "@coder b"}))
        .await;

    let mut seen = Vec::new();
    for e in [&offline, &revoked] {
        let a = h
            .answer_after("bob", Some(&sid), e["id"].as_i64().unwrap())
            .await;
        assert_eq!(a["kind"], "error", "{a}");
        seen.push(a["text"].clone());
    }
    assert_eq!(seen[0], seen[1], "bob can't tell them apart");
    assert_eq!(seen[0], MODEL_UNAVAILABLE_IN_ROOM);
    assert_eq!(
        h.model_calls().len(),
        calls,
        "no model ran, nothing fell back"
    );
    let transcript = serde_json::to_string(&h.events("bob", Some(&sid)).await).unwrap();
    for leak in [
        "routing_note",
        "not_allowed",
        "offline",
        "default model for this",
        "can't be used here",
    ] {
        assert!(!transcript.contains(leak), "{leak}");
    }
    assert_eq!(
        h.events("alice", Some(&sid)).await.len(),
        h.events("bob", Some(&sid)).await.len()
    );

    // Alice still gets an actionable reason for each, privately.
    let note = &offline["routing_note"];
    assert_eq!(
        (
            note["kind"].as_str(),
            note["source"].as_str(),
            note["reason"].as_str()
        ),
        (
            Some("model_unavailable"),
            Some("space_default"),
            Some("offline")
        ),
        "{note}"
    );
    assert!(
        note["message"]
            .as_str()
            .unwrap()
            .contains("default model for this Space")
    );
    let note = &revoked["routing_note"];
    assert_eq!(
        (
            note["kind"].as_str(),
            note["source"].as_str(),
            note["reason"].as_str()
        ),
        (
            Some("model_unavailable"),
            Some("agent_default"),
            Some("not_allowed")
        ),
        "{note}"
    );
    assert!(
        note["message"]
            .as_str()
            .unwrap()
            .contains("default model for this agent in this Space")
    );
    assert!(offline.get("addressing_note").is_some_and(Value::is_null));
}

#[tokio::test]
async fn spaces_can_be_edited_left_and_deleted() {
    let h = H::start().await;
    let sid = h.space("old name").await;
    let path = format!("/v1/spaces/{sid}");
    let (s, _) = h
        .req(
            reqwest::Method::PATCH,
            "bob",
            &path,
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h
        .req(
            reqwest::Method::PATCH,
            "carol",
            &path,
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = h
        .req(
            reqwest::Method::PATCH,
            "alice",
            &path,
            Some(json!({"name": "New name", "description": "Where we plan."})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        (v["name"].as_str(), v["description"].as_str()),
        (Some("New name"), Some("Where we plan."))
    );
    // Leaving: members only.
    let (s, _) = h.post("alice", &format!("{path}/leave"), json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, v) = h.post("bob", &format!("{path}/leave"), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = h.req(reqwest::Method::GET, "bob", &path, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h.post("bob", &format!("{path}/leave"), json!({})).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Deleting: owner only, with the exact name.
    h.post(
        "alice",
        &format!("{path}/members"),
        json!({"user_id": "bob"}),
    )
    .await;
    h.say("alice", Some(&sid), json!({"text": "hello"})).await;
    let (s, _) = h.put_prefs("bob", &sid, json!({"routing": false})).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h
        .req(
            reqwest::Method::DELETE,
            "bob",
            &path,
            Some(json!({"confirm_name": "New name"})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = h
        .req(
            reqwest::Method::DELETE,
            "alice",
            &path,
            Some(json!({"confirm_name": "new name"})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = h
        .req(reqwest::Method::DELETE, "alice", &path, Some(json!({})))
        .await;
    assert!(s.is_client_error());
    let (s, v) = h
        .req(
            reqwest::Method::DELETE,
            "alice",
            &path,
            Some(json!({"confirm_name": "New name"})),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    for who in ["alice", "bob"] {
        let (s, _) = h.req(reqwest::Method::GET, who, &path, None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (_, v) = h.req(reqwest::Method::GET, who, "/v1/spaces", None).await;
        assert!(v["spaces"].as_array().unwrap().is_empty());
    }
    let sid2 = sid.clone();
    let left = h
        .state
        .db
        .call(move |db| {
            Ok((
                db.space_agents(&sid2)?,
                db.list_rooms(&sid2)?,
                db.list_members(&sid2)?,
            ))
        })
        .await
        .unwrap();
    assert!(left.0.is_empty() && left.1.is_empty() && left.2.is_empty());
}
