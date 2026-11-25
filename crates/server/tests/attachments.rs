//! Library files attached to chat messages, over real HTTP: personal Chat and
//! Space rooms and threads, scope gates (no personal↔Space or cross-Space
//! laundering), delivery on send, poll and reload, deletion, empty and
//! duplicate ids, attachment-only sends that never reach System One or a
//! model, and a model request that never carries file names or bytes.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::routing::post;
use metro_model_protocol::Capability;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::model_routing::{BoxFuture, Routing, RoutingBackend};
use metro_server::providers::{SelectedModel, UseError};
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

const CLASSIFIER: &str = "pc_11111111111111111111111111111111";
const CHAT: &str = "pc_22222222222222222222222222222222";

/// System One always says "for the agent", and alice's chat connection is a
/// recording OpenAI-compatible endpoint: any classification or model call
/// an attachment-only send made would show up here.
struct Fake {
    system_one_calls: Mutex<usize>,
    base_url: String,
}

struct Backend(Arc<Fake>);

impl RoutingBackend for Backend {
    fn system_one<'a>(
        &'a self,
        _state: &'a AppState,
        _user: &'a str,
        _space: Option<&'a str>,
        _id: &'a str,
        _state_payload: Value,
        _questions: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            *self.0.system_one_calls.lock().unwrap() += 1;
            Ok(json!({"model": "jev-test", "usage": {},
                      "answers": {"directed": {"type": "noul", "noul": 0.99}}}))
        })
    }

    fn visible_capability<'a>(
        &'a self,
        _state: &'a AppState,
        user: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Capability>>> {
        Box::pin(async move {
            Ok(match (user, id) {
                ("alice", CLASSIFIER) => Some(Capability::SystemOne),
                ("alice", CHAT) => Some(Capability::Chat),
                _ => None,
            })
        })
    }

    fn chat_model<'a>(
        &'a self,
        _state: &'a AppState,
        user: &'a str,
        _space: Option<&'a str>,
        id: &'a str,
    ) -> BoxFuture<'a, Result<SelectedModel, UseError>> {
        Box::pin(async move {
            if user != "alice" || id != CHAT {
                return Err(UseError::NotFound);
            }
            Ok(SelectedModel {
                connection_id: id.to_owned(),
                provider_id: "fake".to_owned(),
                model_id: "chat-model".to_owned(),
                model: metro_server::agent::openai_compatible_model(&ModelConfig {
                    base_url: self.0.base_url.clone(),
                    model: "chat-model".to_owned(),
                    api_key: String::new(),
                }),
                max_tokens: None,
                built_from: None,
            })
        })
    }
}

/// A chat-completions endpoint that records every request body.
async fn endpoint(bodies: Arc<Mutex<Vec<String>>>) -> String {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |Json(body): Json<Value>| {
            let bodies = bodies.clone();
            async move {
                bodies.lock().unwrap().push(body.to_string());
                Json(json!({
                    "id": "c", "object": "chat.completion", "created": 0, "model": "chat-model",
                    "choices": [{"index": 0, "finish_reason": "stop",
                                 "message": {"role": "assistant", "content": "answered"}}],
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
    model_bodies: Arc<Mutex<Vec<String>>>,
    db_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl H {
    async fn start() -> Self {
        let model_bodies = Arc::new(Mutex::new(Vec::new()));
        let base_url = endpoint(model_bodies.clone()).await;
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits {
                agent_timeout: Duration::from_secs(20),
                ..Limits::default()
            },
            org_admins: vec!["alice".to_owned()],
        };
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("metro.sqlite3");
        let mut state = AppState::new(config, Db::open(&db_path).unwrap(), Arc::new(FakeAuth))
            .await
            .unwrap();
        let fake = Arc::new(Fake {
            system_one_calls: Mutex::new(0),
            base_url,
        });
        state.routing = Routing::new(Arc::new(Backend(fake.clone())));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            state,
            fake,
            model_bodies,
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
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn upload(&self, who: &str, query: &str, name: &str, content: &str) -> String {
        let r = self
            .http
            .post(format!(
                "{}/v1/library/assets?{query}&name={name}",
                self.base
            ))
            .bearer_auth(who)
            .header("content-type", "text/plain")
            .body(content.to_owned())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
        r.json::<Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn space(&self, name: &str, members: &[&str]) -> String {
        let (s, v) = self
            .req(
                Method::POST,
                "alice",
                "/v1/spaces",
                Some(json!({"name": name})),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let sid = v["id"].as_str().unwrap().to_owned();
        for m in members {
            let (s, _) = self
                .req(
                    Method::POST,
                    "alice",
                    &format!("/v1/spaces/{sid}/members"),
                    Some(json!({"user_id": m})),
                )
                .await;
            assert_eq!(s, StatusCode::CREATED);
        }
        sid
    }

    async fn chat(&self, who: &str, body: Value) -> (StatusCode, Value) {
        self.req(Method::POST, who, "/v1/chat/messages", Some(body))
            .await
    }

    async fn room(&self, who: &str, sid: &str, rid: &str, body: Value) -> (StatusCode, Value) {
        self.req(
            Method::POST,
            who,
            &format!("/v1/spaces/{sid}/rooms/{rid}/messages"),
            Some(body),
        )
        .await
    }

    async fn chat_events(&self, who: &str) -> Vec<Value> {
        let (s, v) = self.req(Method::GET, who, "/v1/chat/events", None).await;
        assert_eq!(s, StatusCode::OK);
        v["events"].as_array().unwrap().clone()
    }

    async fn room_events(&self, who: &str, sid: &str, rid: &str) -> (StatusCode, Vec<Value>) {
        let (s, v) = self
            .req(
                Method::GET,
                who,
                &format!("/v1/spaces/{sid}/rooms/{rid}/events"),
                None,
            )
            .await;
        (s, v["events"].as_array().cloned().unwrap_or_default())
    }

    /// Opt alice in to classification of unaddressed messages.
    async fn classify_for_alice(&self) {
        let (s, v) = self
            .req(
                Method::PUT,
                "alice",
                "/v1/model-routing",
                Some(json!({"enabled": false, "infer_addressing": true,
                            "classifier_id": CLASSIFIER, "cases": []})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }

    fn system_one_calls(&self) -> usize {
        *self.fake.system_one_calls.lock().unwrap()
    }

    /// Link rows, and those still naming a file.
    fn link_rows(&self) -> (i64, i64) {
        let conn = rusqlite::Connection::open(&self.db_path).unwrap();
        conn.query_row(
            "SELECT COUNT(*), COUNT(asset_id) FROM message_attachments",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }
}

/// Attachment ids in order; a deleted file's place reads "removed".
fn ids(e: &Value) -> Vec<String> {
    e["attachments"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| match x["asset_id"].as_str() {
                    Some(id) => id.to_owned(),
                    None => {
                        assert_eq!(x, &json!({"removed": true}));
                        "removed".to_owned()
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn by_id(events: &[Value], id: &Value) -> Value {
    events
        .iter()
        .find(|e| &e["id"] == id)
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn attachment_only_chat_message_is_delivered_without_any_agent_work() {
    let h = H::start().await;
    h.classify_for_alice().await;
    let a = h
        .upload("alice", "scope=personal", "notes.txt", "hello")
        .await;
    let b = h
        .upload("alice", "scope=personal", "plan.txt", "plan")
        .await;

    let (s, e) = h
        .chat("alice", json!({"text": "  ", "asset_ids": [b, a]}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{e}");
    assert_eq!(e["text"], "");
    assert_eq!(e["audience"], "room");
    // Authoritative metadata, in the order sent; no paths, urls or uploader.
    assert_eq!(ids(&e), vec![b.clone(), a.clone()]);
    assert_eq!(
        e["attachments"][1],
        json!({"asset_id": a, "name": "notes.txt", "mime_type": "text/plain",
               "kind": "text", "size_bytes": 5})
    );

    // Poll and reload return the same.
    let events = h.chat_events("alice").await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["attachments"], e["attachments"]);

    // Ordinary conversation: no System One, no model, no agent events.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.system_one_calls(), 0);
    assert!(h.model_bodies.lock().unwrap().is_empty());
    assert_eq!(h.chat_events("alice").await.len(), 1);

    // The same in a room, with alice's own upload to the Space.
    let sid = h.space("Acme", &[]).await;
    let f = h
        .upload(
            "alice",
            &format!("scope=space&space_id={sid}"),
            "s.txt",
            "s",
        )
        .await;
    let (s, e) = h
        .room(
            "alice",
            &sid,
            "lobby",
            json!({"text": "", "asset_ids": [f]}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{e}");
    assert_eq!(e["audience"], "room");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(h.system_one_calls(), 0);
    assert_eq!(h.room_events("alice", &sid, "lobby").await.1.len(), 1);

    // Text with attachments keeps the opt-in classification as before.
    let (s, _) = h
        .chat("alice", json!({"text": "what is this?", "asset_ids": [a]}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(h.system_one_calls(), 1);
}

#[tokio::test]
async fn ask_agent_needs_a_question_and_bad_id_lists_post_nothing() {
    let h = H::start().await;
    let a = h.upload("alice", "scope=personal", "a.txt", "a").await;
    let sid = h.space("Acme", &["bob"]).await;
    let s_asset = h
        .upload(
            "alice",
            &format!("scope=space&space_id={sid}"),
            "s.txt",
            "s",
        )
        .await;

    let refused = [
        (
            json!({"text": "", "asset_ids": [a], "invoke_agent": true}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text": "", "asset_ids": []}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text": "", "asset_ids": [a, a]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text": "x", "asset_ids": vec![a.clone(); 11]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"text": "x", "asset_ids": ["../../etc/passwd"]}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"text": "x", "asset_ids": ["00000000-0000-0000-0000-000000000000"]}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"text": "", "asset_ids": [a], "provider_connection_id": CHAT}),
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (body, status) in refused {
        let (s, v) = h.chat("alice", body.clone()).await;
        assert_eq!(s, status, "{body} -> {v}");
    }
    assert!(h.chat_events("alice").await.is_empty());

    // In a room too: an explicit agent mention needs words.
    let (s, _) = h
        .room(
            "alice",
            &sid,
            "lobby",
            json!({"text": "", "asset_ids": [s_asset], "mentions": [{"kind": "agent"}]}),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // People only, with a file and no words, is fine.
    let (s, e) = h
        .room("alice", &sid, "lobby", json!({"text": "", "asset_ids": [s_asset], "mentions": [{"kind": "user", "id": "bob"}]}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{e}");
    assert_eq!(e["audience"], "users");

    // Existing posts are unchanged: no attachments key at all.
    let (s, e) = h
        .chat("alice", json!({"text": "plain", "asset_ids": []}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert!(e.get("attachments").is_none(), "{e}");
    let (s, e) = h
        .room("alice", &sid, "lobby", json!({"text": "plain"}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert!(e.get("attachments").is_none(), "{e}");
    assert!(
        h.chat_events("alice")
            .await
            .iter()
            .all(|e| e.get("attachments").is_none())
    );
}

#[tokio::test]
async fn scopes_never_cross_personal_space_or_another_space() {
    let h = H::start().await;
    let acme = h.space("Acme", &["bob"]).await;
    let other = h.space("Other", &[]).await;
    let mine = h.upload("alice", "scope=personal", "mine.txt", "m").await;
    let bobs = h.upload("bob", "scope=personal", "bobs.txt", "b").await;
    let acme_file = h
        .upload(
            "bob",
            &format!("scope=space&space_id={acme}"),
            "acme.txt",
            "a",
        )
        .await;
    let other_file = h
        .upload(
            "alice",
            &format!("scope=space&space_id={other}"),
            "other.txt",
            "o",
        )
        .await;

    h.classify_for_alice().await;
    // Personal Chat: only the sender's own personal files.
    for id in [&bobs, &acme_file, &other_file] {
        let (s, v) = h
            .chat("alice", json!({"text": "x", "asset_ids": [id]}))
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    }
    // A room: only the sender's own uploads to that same Space. Alice reads
    // bob's Acme file, but can't attach it.
    let (s, _) = h
        .req(
            Method::GET,
            "alice",
            &format!("/v1/library/assets/{acme_file}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    for id in [&mine, &bobs, &other_file, &acme_file] {
        let (s, v) = h
            .room(
                "alice",
                &acme,
                "lobby",
                json!({"text": "x", "asset_ids": [id]}),
            )
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    }
    // One bad file refuses the whole message.
    let (s, _) = h
        .room(
            "bob",
            &acme,
            "lobby",
            json!({"text": "x", "asset_ids": [acme_file, bobs]}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(h.room_events("alice", &acme, "lobby").await.1.is_empty());
    assert!(h.chat_events("alice").await.is_empty());
    assert_eq!(
        h.system_one_calls(),
        0,
        "rejected messages reached the classifier"
    );
    assert_eq!(h.link_rows(), (0, 0));

    // Outsiders can't post there at all.
    let (s, _) = h
        .room(
            "carol",
            &acme,
            "lobby",
            json!({"text": "x", "asset_ids": [acme_file]}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // A thread of the same Space works; members see it, outsiders don't.
    let limits = h.state.config.limits.clone();
    let thread = h
        .state
        .db
        .create_room(
            &acme,
            "side",
            Some("lobby"),
            limits.max_rooms,
            limits.max_threads_per_room,
        )
        .unwrap();
    let (s, e) = h
        .room(
            "bob",
            &acme,
            &thread.id,
            json!({"text": "see this", "asset_ids": [acme_file]}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{e}");
    assert_eq!(ids(&e), vec![acme_file.clone()]);
    let (_, events) = h.room_events("alice", &acme, &thread.id).await;
    assert_eq!(ids(&by_id(&events, &e["id"])), vec![acme_file.clone()]);
    let (s, _) = h.room_events("carol", &acme, &thread.id).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A personal file can't go into a thread either.
    let (s, _) = h
        .room(
            "bob",
            &acme,
            &thread.id,
            json!({"text": "x", "asset_ids": [bobs]}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Bytes open only through the library's own access check.
    let (s, _) = h
        .req(
            Method::GET,
            "carol",
            &format!("/v1/library/assets/{acme_file}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let r = h
        .http
        .get(format!("{}/v1/library/assets/{acme_file}/content", h.base))
        .bearer_auth("alice")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // A removed member loses the room and its attachments with it.
    let (s, _) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/spaces/{acme}/members"),
            Some(json!({"user_id": "bob"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = h.room_events("bob", &acme, &thread.id).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .room(
            "bob",
            &acme,
            "lobby",
            json!({"text": "x", "asset_ids": [acme_file]}),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = h
        .req(
            Method::GET,
            "bob",
            &format!("/v1/library/assets/{acme_file}/content"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // The file is the Space's: it stays for the members who remain.
    let (_, events) = h.room_events("alice", &acme, &thread.id).await;
    assert_eq!(ids(&by_id(&events, &e["id"])), vec![acme_file]);
}

#[tokio::test]
async fn same_event_ids_in_chat_and_a_room_keep_their_own_attachments() {
    let h = H::start().await;
    let sid = h.space("Acme", &[]).await;
    let personal = h.upload("alice", "scope=personal", "p.txt", "p").await;
    let shared = h
        .upload(
            "alice",
            &format!("scope=space&space_id={sid}"),
            "s.txt",
            "s",
        )
        .await;
    let (_, p) = h
        .chat("alice", json!({"text": "", "asset_ids": [personal]}))
        .await;
    let (_, mut r) = h
        .room(
            "alice",
            &sid,
            "lobby",
            json!({"text": "", "asset_ids": [shared]}),
        )
        .await;
    // Personal and Space event ids are separate sequences; line them up.
    while r["id"].as_i64() < p["id"].as_i64() {
        r = h
            .room(
                "alice",
                &sid,
                "lobby",
                json!({"text": "", "asset_ids": [shared]}),
            )
            .await
            .1;
    }
    let (_, mut p) = (0, p);
    while p["id"].as_i64() < r["id"].as_i64() {
        p = h
            .chat("alice", json!({"text": "", "asset_ids": [personal]}))
            .await
            .1;
    }
    assert_eq!(p["id"], r["id"]);
    let chat = h.chat_events("alice").await;
    assert_eq!(ids(&by_id(&chat, &p["id"])), vec![personal.clone()]);
    let (_, room) = h.room_events("alice", &sid, "lobby").await;
    assert_eq!(ids(&by_id(&room, &r["id"])), vec![shared.clone()]);
    assert!(chat.iter().all(|e| !ids(e).contains(&shared)));
    assert!(room.iter().all(|e| !ids(e).contains(&personal)));
}

#[tokio::test]
async fn deleting_a_file_keeps_the_message_and_drops_only_its_link() {
    let h = H::start().await;
    let a = h.upload("alice", "scope=personal", "a.txt", "a").await;
    let b = h.upload("alice", "scope=personal", "b.txt", "b").await;
    let (_, e) = h
        .chat("alice", json!({"text": "two files", "asset_ids": [a, b]}))
        .await;
    assert_eq!(h.link_rows(), (2, 2));

    let (s, _) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/library/assets/{a}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    // The link no longer names the file; its place stays, anonymous.
    assert_eq!(h.link_rows(), (2, 1));
    let events = h.chat_events("alice").await;
    let kept = by_id(&events, &e["id"]);
    assert_eq!(kept["text"], "two files");
    assert_eq!(ids(&kept), vec!["removed".to_owned(), b.clone()]);
    assert!(!kept.to_string().contains(&a));
    assert!(!kept.to_string().contains("a.txt"));

    // A deleted file can't be attached; nothing is posted.
    let (s, _) = h
        .chat("alice", json!({"text": "again", "asset_ids": [a]}))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(h.chat_events("alice").await.len(), 1);

    // An attachment-only message whose file is gone stays in the transcript.
    let (_, only) = h.chat("alice", json!({"text": "", "asset_ids": [b]})).await;
    let (s, _) = h
        .req(
            Method::DELETE,
            "alice",
            &format!("/v1/library/assets/{b}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let events = h.chat_events("alice").await;
    let left = by_id(&events, &only["id"]);
    assert_eq!(left["text"], "");
    assert_eq!(left["attachments"], json!([{"removed": true}]));
    assert_eq!(h.link_rows(), (3, 0));
}

#[tokio::test]
async fn the_model_gets_the_question_but_never_file_names_or_bytes() {
    let h = H::start().await;
    let secret = h
        .upload(
            "alice",
            "scope=personal",
            "codename-orchid.txt",
            "the bytes say tangerine",
        )
        .await;
    let (s, _) = h
        .chat("alice", json!({"text": "", "asset_ids": [secret]}))
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let (s, e) = h
        .chat(
            "alice",
            json!({"text": "what time is it?", "invoke_agent": true,
                   "provider_connection_id": CHAT, "asset_ids": [secret]}),
        )
        .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{e}");
    assert_eq!(ids(&e), vec![secret]);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while h.model_bodies.lock().unwrap().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the agent never called the model"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let bodies = h.model_bodies.lock().unwrap().join("\n");
    assert!(bodies.contains("what time is it?"));
    assert!(!bodies.contains("codename-orchid"), "{bodies}");
    assert!(!bodies.contains("tangerine"), "{bodies}");
    assert!(!bodies.contains("attachments"), "{bodies}");
}

#[tokio::test]
async fn an_older_database_gains_the_link_table_and_keeps_its_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metro.sqlite3");
    {
        let db = Db::open(&path).unwrap();
        db.append_personal(
            "alice",
            "message",
            &metro_server::db::Author::server("alice"),
            "before attachments",
            &metro_server::db::Addressed::room(),
        )
        .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TABLE message_attachments;")
            .unwrap();
    }
    let db = Db::open(&path).unwrap();
    let events = db.personal_after("alice", 0, 10).unwrap();
    assert_eq!(events.len(), 1);
    assert!(
        serde_json::to_value(&events[0])
            .unwrap()
            .get("attachments")
            .is_none()
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM message_attachments", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}
