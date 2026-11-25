//! Conversation compaction over HTTP with scripted OpenAI-compatible models:
//! automatic summaries by event count and by a model's context size, the
//! summary as later runs' history (shared in a Space), failures that store
//! nothing and never reach another connection, the auto rate limit and
//! lock, manual `/compact` with its 404/409/502 answers, reply-thread
//! anchors, the per-person setting, and exclusion from search.

use std::sync::atomic::{AtomicBool, Ordering};
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
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::sync::Notify;

const SUMMARY_PROMPT: &str = "Summarize the conversation below";
const SUMMARY_PREAMBLE: &str = "A `compaction` turn is a model-written summary";

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

/// A scripted model: summaries answer `SUMMARY zebrafish …` (or fail, or
/// wait for `release`), other turns answer `ok` (or wait when `hang`).
#[derive(Default)]
struct Mock {
    calls: Mutex<Vec<Value>>,
    fail_summary: AtomicBool,
    gate_summary: AtomicBool,
    hang: AtomicBool,
    reached: Notify,
    release: Notify,
}

fn parts(m: &Value) -> Vec<String> {
    match &m["content"] {
        Value::String(s) => vec![s.clone()],
        Value::Array(p) => p
            .iter()
            .filter_map(|p| p["text"].as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

fn system(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "system")
        .flat_map(parts)
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_summary(body: &Value) -> bool {
    system(body).starts_with(SUMMARY_PROMPT)
}

/// The untrusted text of each user turn, in order.
fn user_texts(body: &Value) -> Vec<String> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .flat_map(parts)
        .filter(|t| !t.starts_with("metro-context: "))
        .collect()
}

/// Metro's context parts of the user turns.
fn contexts(body: &Value) -> Vec<Value> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .flat_map(parts)
        .filter_map(|t| {
            t.strip_prefix("metro-context: ")
                .map(|j| serde_json::from_str(j).unwrap())
        })
        .collect()
}

impl Mock {
    fn summaries(&self) -> Vec<Value> {
        let calls = self.calls.lock().unwrap();
        calls.iter().filter(|b| is_summary(b)).cloned().collect()
    }
    fn runs(&self) -> Vec<Value> {
        let calls = self.calls.lock().unwrap();
        calls.iter().filter(|b| !is_summary(b)).cloned().collect()
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    async fn answer(self: Arc<Self>, body: Value) -> axum::response::Response {
        self.calls.lock().unwrap().push(body.clone());
        let text = if is_summary(&body) {
            if self.gate_summary.load(Ordering::SeqCst) {
                self.reached.notify_one();
                self.release.notified().await;
            }
            if self.fail_summary.load(Ordering::SeqCst) {
                return (
                    AxumStatus::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": {"message": "upstream secret detail"}})),
                )
                    .into_response();
            }
            let lines = parts(&body["messages"].as_array().unwrap()[1]).join("");
            format!("SUMMARY zebrafish of {} lines", lines.lines().count())
        } else {
            if self.hang.load(Ordering::SeqCst) {
                let _ =
                    tokio::time::timeout(Duration::from_secs(20), self.release.notified()).await;
            }
            "ok".to_owned()
        };
        Json(
            json!({"id": "c", "object": "chat.completion", "created": 0, "model": body["model"],
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": text}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}),
        )
        .into_response()
    }

    async fn serve() -> (Arc<Self>, String) {
        let mock = Arc::new(Self::default());
        let m = mock.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| m.clone().answer(body)),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (mock, url)
    }
}

struct H {
    base: String,
    http: reqwest::Client,
    /// The operator's `METRO_MODEL`.
    env: Arc<Mock>,
    /// Endpoints for people's own connections.
    a: (Arc<Mock>, String),
    b: (Arc<Mock>, String),
}

impl H {
    async fn start(agent_history: usize) -> Self {
        let (env, env_url) = Mock::serve().await;
        let a = Mock::serve().await;
        let b = Mock::serve().await;
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: Some(ModelConfig {
                base_url: env_url,
                model: "env-model".into(),
                api_key: String::new(),
            }),
            limits: Limits {
                agent_history,
                agent_timeout: Duration::from_secs(30),
                ..Limits::default()
            },
            org_admins: Vec::new(),
        };
        let mut state = AppState::new(config, Db::open_in_memory().unwrap(), Arc::new(FakeAuth))
            .await
            .unwrap();
        let master =
            MasterKey::parse(&base64::engine::general_purpose::STANDARD.encode([5u8; 32])).unwrap();
        state.providers =
            ProviderService::new(Some(master), vec![a.1.clone(), b.1.clone()]).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            env,
            a,
            b,
        }
    }

    async fn req(
        &self,
        method: Method,
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
        self.req(Method::POST, who, path, Some(body)).await
    }

    async fn get(&self, who: &str, path: &str) -> (StatusCode, Value) {
        self.req(Method::GET, who, path, None).await
    }

    /// A custom connection of `who` to `url` with model `m` (their first,
    /// so their default), with `context_tokens` when given.
    async fn connection(&self, who: &str, url: &str, context_tokens: Option<u32>) -> String {
        let (s, c) = self
            .post(
                who,
                "/v1/providers",
                json!({"name": "Mine", "provider_id": "custom", "base_url": url,
                       "models": [{"model_id": "m"}]}),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{c}");
        assert_eq!(c["is_default"], true);
        let id = c["id"].as_str().unwrap().to_owned();
        if let Some(n) = context_tokens {
            let (s, v) = self
                .req(
                    Method::PATCH,
                    who,
                    &format!("/v1/providers/{id}/model-list"),
                    Some(json!({"model_id": "m", "context_tokens": n})),
                )
                .await;
            assert_eq!(s, StatusCode::OK, "{v}");
            assert_eq!(v["models"][0]["context_tokens"], n);
        }
        id
    }

    async fn chat_events(&self, who: &str) -> Vec<Value> {
        let (s, v) = self.get(who, "/v1/chat/events").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["events"].as_array().unwrap().clone()
    }

    /// Send `text` to `who`'s Chat and wait until its turn ended.
    async fn chat(&self, who: &str, text: &str) -> Vec<Value> {
        let (s, sent) = self
            .post(who, "/v1/chat/messages", json!({"text": text}))
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{sent}");
        let id = sent["id"].as_i64().unwrap();
        for _ in 0..400 {
            let events = self.chat_events(who).await;
            let after: Vec<Value> = events
                .into_iter()
                .filter(|e| e["id"].as_i64().unwrap() > id)
                .collect();
            if after
                .iter()
                .any(|e| e["kind"] == "activity" && e["activity"]["phase"] != "started")
            {
                return after;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no end of the turn for {text}");
    }

    /// A Space owned by alice with bob as a member.
    async fn space(&self) -> String {
        let (s, v) = self.post("alice", "/v1/spaces", json!({"name": "S"})).await;
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
        sid
    }

    async fn events(&self, who: &str, sid: &str, rid: &str) -> Vec<Value> {
        let (s, v) = self
            .get(who, &format!("/v1/spaces/{sid}/rooms/{rid}/events"))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["events"].as_array().unwrap().clone()
    }

    async fn say(&self, who: &str, sid: &str, rid: &str, text: &str) -> i64 {
        let (s, v) = self
            .post(
                who,
                &format!("/v1/spaces/{sid}/rooms/{rid}/messages"),
                json!({"text": text}),
            )
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        v["id"].as_i64().unwrap()
    }

    /// `who` asks the agent in `rid` and waits for the run to end; the
    /// events after the question.
    async fn ask(&self, who: &str, sid: &str, rid: &str, text: &str) -> Vec<Value> {
        let id = self.say(who, sid, rid, &format!("@agent {text}")).await;
        for _ in 0..400 {
            let after: Vec<Value> = self
                .events(who, sid, rid)
                .await
                .into_iter()
                .filter(|e| e["id"].as_i64().unwrap() > id)
                .collect();
            if after
                .iter()
                .any(|e| e["kind"] == "activity" && e["activity"]["phase"] != "started")
            {
                return after;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no end of the run for {text}");
    }

    async fn compact(&self, who: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.post(who, path, body).await
    }
}

fn of_kind<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events.iter().filter(|e| e["kind"] == kind).collect()
}

fn room_path(sid: &str, rid: &str) -> String {
    format!("/v1/spaces/{sid}/rooms/{rid}/compact")
}

#[tokio::test]
async fn chat_compacts_when_events_would_drop_and_later_runs_use_the_summary() {
    let h = H::start(10).await;
    let mut compacted = None;
    for i in 1..=10 {
        let after = h.chat("alice", &format!("message number {i}")).await;
        assert_eq!(
            of_kind(&after, "agent").len(),
            1,
            "every turn answers: {after:?}"
        );
        if let Some(c) = of_kind(&after, "compaction").first() {
            compacted = Some((i, (*c).clone()));
            break;
        }
    }
    let (at, c) = compacted.expect("a summary once more than 10 events followed");
    // 5 exchanges are 10 events; the 7th turn is the first with 12.
    assert_eq!(at, 7);
    assert_eq!(c["actor"], "system");
    assert!(c["actor_id"].is_null());
    assert!(c["text"].as_str().unwrap().starts_with("SUMMARY zebrafish"));
    let meta = &c["compaction"];
    assert_eq!(meta["trigger"], "auto");
    assert_eq!(meta["by"], "alice");
    assert_eq!(meta["by_name"], "alice");
    assert!(meta["focus"].is_null() && meta["previous"].is_null());
    // The oldest events beyond the 10 kept: message 1 and its answer.
    assert_eq!(meta["events"], 2);
    assert!(meta["from_event_id"].as_i64() < meta["through_event_id"].as_i64());

    let summaries = h.env.summaries();
    assert_eq!(summaries.len(), 1);
    let input = user_texts(&summaries[0]).join("\n");
    assert!(input.contains("alice (alice) at "), "{input}");
    assert!(input.contains(": message number 1\n"), "{input}");
    assert!(!input.contains("message number 2"), "{input}");
    assert!(summaries[0]["tools"].is_null(), "no tools");

    // The run that compacted and the next one: summary first, under the
    // run's own key, then the newest events; never the folded ones.
    let after = h.chat("alice", "message number 8").await;
    assert!(of_kind(&after, "compaction").is_empty(), "rate limited");
    assert_eq!(h.env.summaries().len(), 1);
    let runs = h.env.runs();
    for run in &runs[runs.len() - 2..] {
        let ctx = contexts(run);
        assert_eq!(ctx[0]["kind"], "compaction");
        assert_eq!(ctx[0]["covers"], 2);
        let key = ctx[0]["turn_key"].as_str().unwrap();
        assert!(system(run).contains(&format!("This turn's key is `{key}`")));
        assert!(system(run).contains(SUMMARY_PREAMBLE));
        let texts = user_texts(run);
        assert!(texts[0].starts_with("SUMMARY zebrafish"));
        assert!(!texts.iter().any(|t| t == "message number 1"), "{texts:?}");
        // Summary, at most 10 window events, the trigger.
        assert!(run["messages"].as_array().unwrap().len() <= 1 + 1 + 10 + 1);
    }
    // Before the summary, runs had no summary turn or preamble line.
    assert!(!system(&runs[0]).contains(SUMMARY_PREAMBLE));
    assert!(contexts(&runs[0]).iter().all(|c| c["kind"] != "compaction"));
}

#[tokio::test]
async fn without_a_summary_history_is_the_usual_window() {
    let h = H::start(40).await;
    for i in 1..=4 {
        h.chat("alice", &format!("plain {i}")).await;
    }
    let run = h.env.runs().last().unwrap().clone();
    assert_eq!(
        user_texts(&run),
        ["plain 1", "plain 2", "plain 3", "plain 4"],
        "every earlier message, verbatim, in order"
    );
    assert_eq!(run["messages"].as_array().unwrap().len(), 1 + 7);
    assert!(!system(&run).contains(SUMMARY_PREAMBLE));
    assert!(h.env.summaries().is_empty());
}

#[tokio::test]
async fn a_small_context_window_triggers_on_tokens_and_unknown_means_32k() {
    let h = H::start(40).await;
    let (a, url) = (h.a.0.clone(), h.a.1.clone());
    // Out of range and null.
    let id = h.connection("alice", &url, None).await;
    let list = format!("/v1/providers/{id}/model-list");
    for bad in [json!(100), json!(20_000_000), json!("big")] {
        let (s, _) = h
            .req(
                Method::PATCH,
                "alice",
                &list,
                Some(json!({"model_id": "m", "context_tokens": bad})),
            )
            .await;
        assert!(s.is_client_error(), "{bad}");
    }
    let (s, v) = h
        .req(
            Method::PATCH,
            "alice",
            &list,
            Some(json!({"model_id": "m", "context_tokens": 16384})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["models"][0]["context_tokens"], 16384);
    let (_, v) = h.get("bob", &list).await;
    assert!(v["models"].is_null(), "owner only");

    let big = "word ".repeat(1_200);
    let mut at = None;
    for i in 1..=8 {
        let after = h.chat("alice", &format!("{i} {big}")).await;
        if !of_kind(&after, "compaction").is_empty() {
            at = Some(i);
            break;
        }
    }
    let at = at.expect("the 16k window fills before 40 events");
    // 16k less the 4k reserve, 70%: the 6th turn, long before 40 events.
    assert_eq!(at, 6);
    assert_eq!(a.summaries().len(), 1);
    let run = a.runs().last().unwrap().clone();
    assert_eq!(contexts(&run)[0]["kind"], "compaction");
    // The 8 newest events (4 messages, 4 answers) stayed verbatim: the
    // summary, those 4 messages and the trigger.
    let texts = user_texts(&run);
    assert_eq!(texts.len(), 6, "{}", texts.len());
    assert!(
        texts[1].starts_with("2 word"),
        "messages 1 and its answer folded"
    );
    assert_eq!(h.env.count(), 0, "never the operator's model");

    // The same conversation on a model of unknown size (32k) doesn't.
    for i in 1..=at {
        let after = h.chat("bob", &format!("{i} {big}")).await;
        assert!(of_kind(&after, "compaction").is_empty());
    }
    assert!(h.env.summaries().is_empty());
}

#[tokio::test]
async fn a_rooms_summary_is_shared_and_used_by_every_members_run() {
    let h = H::start(10).await;
    let sid = h.space().await;
    let mut found = None;
    for i in 1..=10 {
        let after = h
            .ask("alice", &sid, "lobby", &format!("lobby note {i}"))
            .await;
        if let Some(c) = of_kind(&after, "compaction").first() {
            found = Some((*c).clone());
            break;
        }
    }
    let c = found.expect("a room summary");
    assert_eq!(c["compaction"]["by"], "alice");
    // Bob reads it like any event, and his own run uses it.
    let seen = h.events("bob", &sid, "lobby").await;
    assert!(
        seen.iter()
            .any(|e| e["id"] == c["id"] && e["kind"] == "compaction")
    );
    let summaries = h.env.summaries().len();
    h.ask("bob", &sid, "lobby", "what did we decide").await;
    let run = h.env.runs().last().unwrap().clone();
    assert_eq!(contexts(&run)[0]["kind"], "compaction");
    assert!(user_texts(&run)[0].starts_with("SUMMARY zebrafish"));
    assert!(system(&run).contains(SUMMARY_PREAMBLE));
    assert_eq!(h.env.summaries().len(), summaries);

    // Never searched or quick-searched; the messages still are.
    for who in ["alice", "bob"] {
        let (s, v) = h
            .post(
                who,
                "/v1/search/quick",
                json!({"query": "zebrafish", "scope": "all", "mode": "conversations"}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert!(v["messages"].as_array().unwrap().is_empty(), "{v}");
    }
    let (_, v) = h
        .post(
            "bob",
            "/v1/search/quick",
            json!({"query": "lobby note", "scope": "all", "mode": "conversations"}),
        )
        .await;
    assert!(!v["messages"].as_array().unwrap().is_empty(), "{v}");
}

#[tokio::test]
async fn an_auto_failure_stores_nothing_tells_only_the_initiator_and_never_falls_back() {
    let h = H::start(10).await;
    let (a, b) = (h.a.0.clone(), h.b.0.clone());
    h.connection("alice", &h.a.1, None).await;
    h.connection("bob", &h.b.1, None).await;
    a.fail_summary.store(true, Ordering::SeqCst);
    let sid = h.space().await;
    let mut failed = None;
    for i in 1..=10 {
        let after = h.ask("alice", &sid, "lobby", &format!("try {i}")).await;
        if let Some(s) = of_kind(&after, "status").first() {
            // The run still answered, from its window.
            assert_eq!(of_kind(&after, "agent").len(), 1, "{after:?}");
            failed = Some(((*s).clone(), after.clone()));
            break;
        }
    }
    let (status, after) = failed.expect("an automatic attempt");
    let shown = status["text"].as_str().unwrap();
    assert!(
        shown.starts_with("Couldn't summarize earlier messages"),
        "{shown}"
    );
    assert!(
        !shown.contains("500") && !shown.contains("secret"),
        "{shown}"
    );
    assert_eq!(a.summaries().len(), 1);
    assert!(
        h.events("alice", &sid, "lobby")
            .await
            .iter()
            .all(|e| e["kind"] != "compaction")
    );
    // Neither another person's connection nor the operator's model.
    assert_eq!(b.count(), 0);
    assert_eq!(h.env.count(), 0);
    // The reason, for the initiator alone.
    let run_id = after.iter().find(|e| e["kind"] == "activity").unwrap()["activity"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, mine) = h.get("alice", &format!("/v1/runs/{run_id}")).await;
    let note = mine["compaction_failure"]["message"].as_str().unwrap();
    assert!(note.contains("HTTP 500"), "{note}");
    assert!(!note.contains("secret"), "{note}");
    let (_, theirs) = h.get("bob", &format!("/v1/runs/{run_id}")).await;
    assert!(theirs.get("compaction_failure").is_none(), "{theirs}");
    let (_, listed) = h.get("bob", "/v1/runs").await;
    assert!(!listed.to_string().contains("compaction_failure"));
    // The next run within two minutes doesn't try again.
    let after = h.ask("alice", &sid, "lobby", "again").await;
    assert!(of_kind(&after, "status").is_empty());
    assert_eq!(a.summaries().len(), 1);

    // In Chat the reason is shown in the person's own Chat.
    let mut note = None;
    for i in 1..=10 {
        let after = h.chat("alice", &format!("chat {i}")).await;
        if let Some(s) = of_kind(&after, "status").first() {
            note = Some(s["text"].as_str().unwrap().to_owned());
            break;
        }
    }
    let note = note.unwrap();
    assert!(
        note.starts_with("Couldn't summarize earlier messages: "),
        "{note}"
    );
    assert!(note.contains("HTTP 500"), "{note}");
    assert!(
        h.chat_events("alice")
            .await
            .iter()
            .all(|e| e["kind"] != "compaction")
    );
    assert_eq!(b.count(), 0);
    assert_eq!(h.env.count(), 0);
}

#[tokio::test]
async fn auto_off_never_compacts_and_prefs_are_bounded() {
    let h = H::start(4).await;
    let (s, v) = h.get("alice", "/v1/compaction").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["auto"], true);
    assert_eq!(v["at_percent"], 70);
    assert_eq!(v["min_percent"], 40);
    assert_eq!(v["max_percent"], 90);
    assert_eq!(v["default_context_tokens"], 32000);
    for bad in [
        json!({"at_percent": 39}),
        json!({"at_percent": 91}),
        json!({"other": 1}),
    ] {
        let (s, _) = h
            .req(Method::PUT, "alice", "/v1/compaction", Some(bad.clone()))
            .await;
        assert!(s.is_client_error(), "{bad}");
    }
    let (s, v) = h
        .req(
            Method::PUT,
            "alice",
            "/v1/compaction",
            Some(json!({"auto": false, "at_percent": 40})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["auto"], false);
    assert_eq!(v["at_percent"], 40);
    let (_, v) = h
        .req(
            Method::PUT,
            "alice",
            "/v1/compaction",
            Some(json!({"at_percent": 90})),
        )
        .await;
    assert_eq!(
        (v["auto"].clone(), v["at_percent"].clone()),
        (json!(false), json!(90))
    );
    // Per person.
    assert_eq!(h.get("bob", "/v1/compaction").await.1["auto"], true);
    for i in 1..=6 {
        let after = h.chat("alice", &format!("no summary {i}")).await;
        assert!(of_kind(&after, "compaction").is_empty());
    }
    assert!(h.env.summaries().is_empty());
}

#[tokio::test]
async fn a_held_lock_lets_the_run_continue_without_waiting() {
    let h = H::start(4).await;
    for i in 1..=6 {
        let (s, v) = h
            .post(
                "alice",
                "/v1/chat/messages",
                json!({"text": format!("note {i}"), "save_note": true}),
            )
            .await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    }
    // Hold the Chat's lock with a manual summary that waits.
    h.env.gate_summary.store(true, Ordering::SeqCst);
    let base = h.base.clone();
    let manual = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{base}/v1/chat/compact"))
            .bearer_auth("alice")
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status()
    });
    h.env.reached.notified().await;
    // Over the 4-event cap, but the lock is held: the run answers at once.
    let after = h.chat("alice", "during").await;
    assert_eq!(of_kind(&after, "agent").len(), 1);
    assert!(of_kind(&after, "compaction").is_empty());
    assert_eq!(h.env.summaries().len(), 1);
    h.env.gate_summary.store(false, Ordering::SeqCst);
    h.env.release.notify_one();
    assert_eq!(manual.await.unwrap(), StatusCode::CREATED);
}

#[tokio::test]
async fn manual_compaction_in_rooms_and_chat() {
    let h = H::start(40).await;
    let sid = h.space().await;
    let path = room_path(&sid, "lobby");
    // Outsiders and unknown places.
    assert_eq!(
        h.compact("carol", &path, json!({})).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.compact("alice", &room_path(&sid, "nowhere"), json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.compact("alice", &room_path("nope", "lobby"), json!({}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // Nothing new yet.
    for i in 1..=5 {
        h.say("alice", &sid, "lobby", &format!("plan step {i}"))
            .await;
    }
    let (s, v) = h.compact("bob", &path, json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["code"], "nothing_new");
    assert_eq!(v["error"], "Nothing new to summarize yet");
    h.say("bob", &sid, "lobby", "plan step 6").await;
    // Bad focus.
    let long = "f".repeat(201);
    assert_eq!(
        h.compact("bob", &path, json!({"focus": long})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.compact("bob", &path, json!({"other": 1})).await.0,
        StatusCode::BAD_REQUEST
    );
    // A model failure: 502 with the reason, for the invoker only; nothing stored.
    h.env.fail_summary.store(true, Ordering::SeqCst);
    let (s, v) = h.compact("bob", &path, json!({"focus": "the plan"})).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(v["code"], "summary_failed");
    assert!(v["error"].as_str().unwrap().contains("HTTP 500"), "{v}");
    assert!(!v.to_string().contains("secret"));
    let events = h.events("alice", &sid, "lobby").await;
    assert!(
        events
            .iter()
            .all(|e| e["kind"] != "compaction" && e["kind"] != "error"),
        "{events:?}"
    );
    h.env.fail_summary.store(false, Ordering::SeqCst);
    // Success: 201 with the event, by the invoker, with the focus.
    let (s, c) = h.compact("bob", &path, json!({"focus": "the plan"})).await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    assert_eq!(c["kind"], "compaction");
    assert_eq!(c["compaction"]["trigger"], "manual");
    assert_eq!(c["compaction"]["by"], "bob");
    assert_eq!(c["compaction"]["focus"], "the plan");
    assert_eq!(c["compaction"]["events"], 6);
    let input = user_texts(h.env.summaries().last().unwrap()).join("\n");
    assert!(input.ends_with("Focus: the plan"), "{input}");
    assert!(input.contains("bob (bob) at ") && input.contains("plan step 1"));
    assert!(
        h.events("alice", &sid, "lobby")
            .await
            .iter()
            .any(|e| e["id"] == c["id"])
    );
    // No run or reply.
    assert!(h.env.runs().is_empty());
    // Straight away there's nothing new.
    assert_eq!(
        h.compact("alice", &path, json!({})).await.1["code"],
        "nothing_new"
    );

    // While a reply runs: 409.
    h.env.hang.store(true, Ordering::SeqCst);
    h.say("alice", &sid, "lobby", "@agent take your time").await;
    for _ in 0..200 {
        if !h.env.runs().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let (s, v) = h.compact("alice", &path, json!({})).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["code"], "reply_running");
    assert_eq!(v["error"], "Wait for the current reply to finish");
    h.env.hang.store(false, Ordering::SeqCst);
    h.env.release.notify_one();

    // Chat: the owner's own, the same rules.
    let (s, v) = h.compact("alice", "/v1/chat/compact", json!({})).await;
    assert_eq!(
        (s, v["code"].clone()),
        (StatusCode::CONFLICT, json!("nothing_new"))
    );
    for i in 1..=3 {
        h.chat("alice", &format!("chat {i}")).await;
    }
    let (s, c) = h
        .compact("alice", "/v1/chat/compact", json!({"focus": "chat"}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    assert_eq!(c["compaction"]["by"], "alice");
    assert!(
        h.chat_events("alice")
            .await
            .iter()
            .any(|e| e["id"] == c["id"])
    );
    assert!(
        h.chat_events("bob")
            .await
            .iter()
            .all(|e| e["kind"] != "compaction")
    );
    // An empty body is no focus; with a Chat turn running, 409.
    for i in 4..=9 {
        h.chat("alice", &format!("chat {i}")).await;
    }
    h.env.hang.store(true, Ordering::SeqCst);
    h.post("alice", "/v1/chat/messages", json!({"text": "slow"}))
        .await;
    for _ in 0..200 {
        if h.env
            .runs()
            .iter()
            .any(|r| user_texts(r).last().map(String::as_str) == Some("slow"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let r = h
        .http
        .post(format!("{}/v1/chat/compact", h.base))
        .bearer_auth("alice")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT);
    h.env.hang.store(false, Ordering::SeqCst);
    h.env.release.notify_one();
}

#[tokio::test]
async fn a_compacted_reply_thread_keeps_its_anchor_first() {
    let h = H::start(40).await;
    let sid = h.space().await;
    let anchor = h.say("bob", &sid, "lobby", "the anchor message").await;
    let (s, t) = h
        .post(
            "alice",
            &format!("/v1/spaces/{sid}/rooms/lobby/events/{anchor}/thread"),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    let thread = t["id"].as_str().unwrap().to_owned();
    for i in 1..=6 {
        h.say("bob", &sid, &thread, &format!("thread talk {i}"))
            .await;
    }
    let (s, c) = h
        .compact("alice", &room_path(&sid, &thread), json!({}))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let input = user_texts(h.env.summaries().last().unwrap()).join("\n");
    assert!(
        !input.contains("the anchor message"),
        "the anchor is never folded"
    );
    h.ask("alice", &sid, &thread, "go on").await;
    let run = h.env.runs().last().unwrap().clone();
    let texts = user_texts(&run);
    assert_eq!(texts[0], "the anchor message");
    assert!(texts[1].starts_with("SUMMARY zebrafish"), "{texts:?}");
    assert_eq!(texts[2], "@agent go on");
    assert!(!texts.iter().any(|t| t.starts_with("thread talk")));
}
