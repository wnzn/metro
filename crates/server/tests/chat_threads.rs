//! Threads of a personal Chat: owner-only management, separate history,
//! and permanent deletion of everything in a thread.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::routing::post;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, ModelConfig, router};
use reqwest::{Method, StatusCode};
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

/// An OpenAI-compatible endpoint that answers "ok" and records requests.
async fn model(calls: Arc<Mutex<Vec<Value>>>) -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let calls = calls.clone();
            async move {
                calls.lock().unwrap().push(body.clone());
                Json(json!({
                    "id": "c", "object": "chat.completion", "created": 0, "model": body["model"],
                    "choices": [{"index": 0, "finish_reason": "stop",
                                 "message": {"role": "assistant", "content": "ok"}}],
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
    calls: Arc<Mutex<Vec<Value>>>,
    db_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl H {
    async fn start() -> Self {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: Some(ModelConfig {
                base_url: model(calls.clone()).await,
                model: "env-model".into(),
                api_key: String::new(),
            }),
            limits: Limits {
                agent_timeout: Duration::from_secs(20),
                ..Limits::default()
            },
            org_admins: vec!["carol".into()],
        };
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metro.sqlite3");
        let db = Db::open(&db_path).unwrap();
        let state = AppState::new(config, db, Arc::new(FakeAuth)).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served = state.clone();
        tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            calls,
            db_path,
            _dir: dir,
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

    async fn thread(&self, who: &str, title: Option<&str>) -> String {
        let body = title.map_or(json!({}), |t| json!({ "title": t }));
        let (s, v) = self.post(who, "/v1/chat/threads", body).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    /// Events of the main Chat (`None`) or thread `tid`.
    async fn events(&self, who: &str, tid: Option<&str>) -> Vec<Value> {
        let path = match tid {
            Some(t) => format!("/v1/chat/events?thread={t}"),
            None => "/v1/chat/events".to_owned(),
        };
        let (s, v) = self.req(Method::GET, who, &path, None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["events"].as_array().unwrap().clone()
    }

    async fn send(&self, who: &str, tid: Option<&str>, body: Value) -> Value {
        let path = match tid {
            Some(t) => format!("/v1/chat/messages?thread={t}"),
            None => "/v1/chat/messages".to_owned(),
        };
        let (s, v) = self.post(who, &path, body).await;
        assert_eq!(s, StatusCode::ACCEPTED, "{v}");
        v
    }

    /// The agent's reply after `after` in the main Chat or thread `tid`.
    async fn reply_after(&self, tid: Option<&str>, after: i64) -> Value {
        for _ in 0..400 {
            if let Some(e) = self
                .events("alice", tid)
                .await
                .into_iter()
                .find(|e| e["kind"] == "agent" && id(e) > after)
            {
                return e;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no reply after {after}");
    }

    /// The text of the last request the model was sent.
    fn last_call(&self) -> String {
        serde_json::to_string(self.calls.lock().unwrap().last().unwrap()).unwrap()
    }

    fn conn(&self) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open(&self.db_path).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn
    }

    fn count(&self, sql: &str, p: impl rusqlite::Params) -> i64 {
        self.conn().query_row(sql, p, |r| r.get(0)).unwrap()
    }
}

fn id(v: &Value) -> i64 {
    v["id"].as_i64().unwrap()
}

#[tokio::test]
async fn chat_threads_are_their_owners_alone() {
    let h = H::start().await;
    let untitled = h.thread("alice", None).await;
    let titled = h.thread("alice", Some("  Trip plans ")).await;
    let (s, list) = h.req(Method::GET, "alice", "/v1/chat/threads", None).await;
    assert_eq!(s, StatusCode::OK);
    let list = list["threads"].as_array().unwrap().clone();
    assert_eq!(list.len(), 2);
    let named = list.iter().find(|t| t["id"] == titled).unwrap();
    assert_eq!(named["title"], "Trip plans");
    assert_eq!(named["count"], 0);
    let plain = list.iter().find(|t| t["id"] == untitled).unwrap();
    assert_eq!(plain["title"], "New thread");

    // Rename; a blank or overlong title is refused.
    let path = format!("/v1/chat/threads/{titled}");
    let (s, v) = h
        .req(
            Method::PATCH,
            "alice",
            &path,
            Some(json!({"title": "Trip"})),
        )
        .await;
    assert_eq!((s, v["title"].as_str()), (StatusCode::OK, Some("Trip")));
    for bad in ["   ", &"x".repeat(81), "a\nb"] {
        let (s, _) = h
            .req(Method::PATCH, "alice", &path, Some(json!({"title": bad})))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad:?}");
    }

    // Someone else's thread is unknown, whatever they try.
    let (_, theirs) = h.req(Method::GET, "bob", "/v1/chat/threads", None).await;
    assert_eq!(theirs["threads"], json!([]));
    let q = format!("?thread={titled}");
    for (method, path, body) in [
        (Method::GET, format!("/v1/chat/events{q}"), None),
        (
            Method::POST,
            format!("/v1/chat/messages{q}"),
            Some(json!({"text": "hi", "save_note": true})),
        ),
        (
            Method::POST,
            format!("/v1/chat/compact{q}"),
            Some(json!({})),
        ),
        (Method::PATCH, path.clone(), Some(json!({"title": "Mine"}))),
        (Method::DELETE, path.clone(), None),
    ] {
        let (s, v) = h.req(method.clone(), "bob", &path, body).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}: {v}");
    }
    let (s, _) = h
        .req(Method::GET, "alice", "/v1/chat/events?thread=ct-nope", None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Still Alice's, unchanged.
    let (_, list) = h.req(Method::GET, "alice", "/v1/chat/threads", None).await;
    assert!(
        list["threads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == titled && t["title"] == "Trip")
    );
}

#[tokio::test]
async fn a_chat_thread_is_its_own_conversation() {
    let h = H::start().await;
    let main = h
        .send("alice", None, json!({"text": "main says swordfish"}))
        .await;
    h.reply_after(None, id(&main)).await;
    let tid = h.thread("alice", Some("Side")).await;
    let asked = h
        .send("alice", Some(&tid), json!({"text": "thread asks marlin"}))
        .await;
    let reply = h.reply_after(Some(&tid), id(&asked)).await;
    let saw = h.last_call();
    assert!(saw.contains("marlin"), "the thread turn reached the model");
    assert!(
        !saw.contains("swordfish"),
        "main Chat history leaked: {saw}"
    );
    // Each stream shows only its own events.
    let in_thread: Vec<i64> = h.events("alice", Some(&tid)).await.iter().map(id).collect();
    let in_main: Vec<i64> = h.events("alice", None).await.iter().map(id).collect();
    assert!(in_thread.contains(&id(&asked)) && in_thread.contains(&id(&reply)));
    assert!(!in_thread.contains(&id(&main)));
    assert!(!in_main.contains(&id(&asked)) && !in_main.contains(&id(&reply)));
    // And the main Chat never sees the thread.
    let again = h.send("alice", None, json!({"text": "main again"})).await;
    h.reply_after(None, id(&again)).await;
    let saw = h.last_call();
    assert!(
        saw.contains("swordfish") && !saw.contains("marlin"),
        "{saw}"
    );
    let (_, list) = h.req(Method::GET, "alice", "/v1/chat/threads", None).await;
    assert_eq!(list["threads"][0]["count"], 2, "{list}");
}

#[tokio::test]
async fn a_reply_thread_starts_from_a_main_chat_message() {
    let h = H::start().await;
    let note = h
        .send(
            "alice",
            None,
            json!({"text": "the anchor bluefin", "save_note": true}),
        )
        .await;
    let path = format!("/v1/chat/events/{}/thread", id(&note));
    let (s, thread) = h.post("alice", &path, json!({})).await;
    assert_eq!(s, StatusCode::CREATED, "{thread}");
    assert_eq!(thread["anchor_event_id"], note["id"]);
    assert_eq!(thread["title"], "the anchor bluefin");
    let tid = thread["id"].as_str().unwrap().to_owned();
    let (s, again) = h.post("alice", &path, json!({})).await;
    assert_eq!((s, &again["id"]), (StatusCode::OK, &thread["id"]));
    assert_eq!(
        h.post("bob", &path, json!({})).await.0,
        StatusCode::NOT_FOUND
    );

    // The thread's agent sees its anchor first.
    let asked = h
        .send("alice", Some(&tid), json!({"text": "what was it?"}))
        .await;
    h.reply_after(Some(&tid), id(&asked)).await;
    assert!(h.last_call().contains("bluefin"));
    let anchor = h
        .events("alice", None)
        .await
        .into_iter()
        .find(|e| id(e) == id(&note))
        .unwrap();
    assert_eq!(anchor["replies"]["room_id"], json!(tid));
    assert_eq!(anchor["replies"]["count"], 2, "{anchor}");

    // One level deep, and only messages or replies.
    let (s, _) = h
        .post(
            "alice",
            &format!("/v1/chat/events/{}/thread", id(&asked)),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let activity = h
        .events("alice", Some(&tid))
        .await
        .into_iter()
        .find(|e| e["kind"] == "activity")
        .unwrap();
    let (s, _) = h
        .post(
            "alice",
            &format!("/v1/chat/events/{}/thread", id(&activity)),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Deleting the anchor leaves a tombstone while the thread exists, and
    // it is no longer anyone's words.
    let (s, v) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/chat/events/{}", id(&note)),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let stone = h
        .events("alice", None)
        .await
        .into_iter()
        .find(|e| id(e) == id(&note))
        .unwrap();
    assert_eq!(
        (stone["kind"].as_str(), stone["text"].as_str()),
        (Some("deleted"), Some(""))
    );
    let asked = h
        .send("alice", Some(&tid), json!({"text": "and now?"}))
        .await;
    h.reply_after(Some(&tid), id(&asked)).await;
    assert!(!h.last_call().contains("bluefin"));
    // Deleting the thread takes the tombstone with it.
    let (s, v) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/chat/threads/{tid}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["events"][0]["kind"], "message_deleted");
    assert!(
        !h.events("alice", None)
            .await
            .iter()
            .any(|e| id(e) == id(&note))
    );
}

#[tokio::test]
async fn deleting_a_chat_thread_removes_everything_in_it() {
    let h = H::start().await;
    let keep = h.send("alice", None, json!({"text": "keep me"})).await;
    h.reply_after(None, id(&keep)).await;
    let tid = h.thread("alice", Some("Gone")).await;
    let asked = h
        .send("alice", Some(&tid), json!({"text": "secret tuna"}))
        .await;
    let reply = h.reply_after(Some(&tid), id(&asked)).await;
    let (s, _) = h
        .req(
            Method::PUT,
            "alice",
            &format!("/v1/chat/events/{}/reactions", id(&asked)),
            Some(json!({"emoji": "👍"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    // A reaction event lands in the target's thread.
    assert!(
        h.events("alice", Some(&tid))
            .await
            .iter()
            .any(|e| e["kind"] == "reaction")
    );
    assert!(
        !h.events("alice", None)
            .await
            .iter()
            .any(|e| e["kind"] == "reaction")
    );
    // Derived rows of the thread: a search vector and a summary.
    {
        let conn = h.conn();
        for eid in [id(&asked), id(&reply), id(&keep)] {
            conn.execute(
                "INSERT INTO search_vectors (user_id, source, event_id, fingerprint, text_hash, vector)
                 VALUES ('alice', 'chat', ?1, 'fp', x'00', x'00')",
                [eid],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO personal_events (user_id, thread_id, kind, actor, text, created_at, activity)
             VALUES ('alice', ?1, 'compaction', 'system', 'summary of tuna', 1, ?2)",
            rusqlite::params![
                tid,
                json!({"compaction": {"from_event_id": id(&asked), "through_event_id": id(&reply)}})
                    .to_string()
            ],
        )
        .unwrap();
    }
    // Not Bob's to delete.
    let path = format!("/v1/chat/threads/{tid}");
    assert_eq!(
        h.req(Method::DELETE, "bob", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    let (s, v) = h.req(Method::DELETE, "alice", &path, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        h.req(Method::DELETE, "alice", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.req(
            Method::GET,
            "alice",
            &format!("/v1/chat/events?thread={tid}"),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM personal_events WHERE thread_id = ?1",
            [&tid]
        ),
        0
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM chat_threads", []), 0);
    assert_eq!(h.count("SELECT COUNT(*) FROM personal_reactions", []), 0);
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM search_vectors WHERE event_id IN (?1, ?2)",
            [id(&asked), id(&reply)]
        ),
        0
    );
    assert_eq!(
        h.count(
            "SELECT COUNT(*) FROM search_vectors WHERE event_id = ?1",
            [id(&keep)]
        ),
        1
    );
    // The main Chat is untouched, and nothing of the thread reaches a model.
    assert!(
        h.events("alice", None)
            .await
            .iter()
            .any(|e| id(e) == id(&keep))
    );
    let after = h
        .send("alice", None, json!({"text": "what was secret?"}))
        .await;
    h.reply_after(None, id(&after)).await;
    assert!(!h.last_call().contains("tuna"));
    // Sending to it now is 404, and nothing is stored.
    let (s, _) = h
        .post(
            "alice",
            &format!("/v1/chat/messages?thread={tid}"),
            json!({"text": "late"}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_thread_message_is_said_in_that_thread() {
    let h = H::start().await;
    let tid = h.thread("alice", None).await;
    let note = h
        .send(
            "alice",
            Some(&tid),
            json!({"text": "oops", "save_note": true}),
        )
        .await;
    let (s, v) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/chat/events/{}", id(&note)),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let marker = id(&v["events"][0]);
    let thread = h.events("alice", Some(&tid)).await;
    assert!(thread.iter().any(|e| id(e) == marker));
    assert!(!thread.iter().any(|e| id(e) == id(&note)));
    assert!(
        !h.events("alice", None)
            .await
            .iter()
            .any(|e| id(e) == marker)
    );
}
