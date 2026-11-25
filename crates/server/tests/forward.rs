use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use metro_model_protocol::Capability;
use metro_server::db::{Addressed, Author, Db, kind};
use metro_server::model_routing::{BoxFuture, Routing, RoutingBackend};
use metro_server::providers::{SelectedModel, UseError};
use metro_server::{AppState, Config, Limits, router};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};

const ALICE: &str = "alice-test-token-0000000000000";
const BOB: &str = "bob-test-token-000000000000000";
const NEVER: &str = "never-used-test-token-00000000";
const CLASSIFIER: &str = "pc_11111111111111111111111111111111";

#[derive(Default)]
struct Classifier(AtomicUsize);

impl RoutingBackend for Classifier {
    fn system_one<'a>(
        &'a self,
        _: &'a AppState,
        _: &'a str,
        _: Option<&'a str>,
        _: &'a str,
        _: Value,
        _: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { anyhow::bail!("unexpected classifier call") })
    }

    fn visible_capability<'a>(
        &'a self,
        _: &'a AppState,
        _: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Capability>>> {
        Box::pin(async move { Ok((id == CLASSIFIER).then_some(Capability::SystemOne)) })
    }

    fn chat_model<'a>(
        &'a self,
        _: &'a AppState,
        _: &'a str,
        _: Option<&'a str>,
        _: &'a str,
    ) -> BoxFuture<'a, Result<SelectedModel, UseError>> {
        Box::pin(async { Err(UseError::NotFound) })
    }
}

struct Server {
    client: Client,
    base: String,
    db: Db,
    classifier: Arc<Classifier>,
}

impl Server {
    async fn start() -> Self {
        let users = json!({
            "alice": ALICE, "bob": BOB, "never": NEVER
        })
        .to_string();
        let auth = metro_auth::Authenticator::from_lookup(move |key| {
            Ok((key == metro_auth::env::DEV_USERS).then(|| users.clone()))
        })
        .await
        .unwrap();
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits::default(),
            org_admins: vec!["bob".into()],
        };
        let db = Db::open_in_memory().unwrap();
        let classifier = Arc::new(Classifier::default());
        let mut state = AppState::new(config, db.clone(), Arc::new(auth))
            .await
            .unwrap();
        state.routing = Routing::new(classifier.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            client: Client::new(),
            base,
            db,
            classifier,
        }
    }

    async fn call(
        &self,
        method: Method,
        user: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(user);
        if !body.is_null() {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        (
            response.status(),
            response.json().await.unwrap_or(Value::Null),
        )
    }

    async fn expect(
        &self,
        method: Method,
        user: &str,
        path: &str,
        body: Value,
        expected: StatusCode,
    ) -> Value {
        let (status, value) = self.call(method, user, path, body).await;
        assert_eq!(status, expected, "{path}: {value}");
        value
    }

    async fn get(&self, user: &str, path: &str) -> Value {
        self.expect(Method::GET, user, path, Value::Null, StatusCode::OK)
            .await
    }

    async fn space(&self, user: &str) -> String {
        let space = self
            .expect(
                Method::POST,
                user,
                "/v1/spaces",
                json!({"name": "Test"}),
                StatusCode::CREATED,
            )
            .await;
        space["id"].as_str().unwrap().to_owned()
    }

    async fn classify(&self, space: &str) {
        self.expect(Method::PUT, ALICE, "/v1/model-routing", json!({
            "enabled": false, "infer_addressing": false, "classifier_id": CLASSIFIER, "cases": []
        }), StatusCode::OK).await;
        self.expect(
            Method::PUT,
            ALICE,
            &format!("/v1/spaces/{space}/prefs"),
            json!({"classify_unaddressed": true}),
            StatusCode::OK,
        )
        .await;
    }

    async fn forward(
        &self,
        user: &str,
        source: &Value,
        destination: &Value,
        comment: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut body = json!({"source": source, "destination": destination});
        if let Some(comment) = comment {
            body["comment"] = json!(comment);
        }
        self.call(Method::POST, user, "/v1/forward", body).await
    }
}

fn room(space: &str, room: &str) -> Value {
    json!({"kind":"space", "space_id":space, "room_id":room})
}

fn in_room(space: &str, room_id: &str, event: &Value) -> Value {
    let mut source = room(space, room_id);
    source["event_id"] = event.clone();
    source
}

fn to_dm(user: &str) -> Value {
    json!({"kind":"dm", "user_id":user})
}

#[tokio::test]
async fn same_space_forward_and_comment_copy_only_text_without_dispatch() {
    let s = Server::start().await;
    let sid = s.space(ALICE).await;
    let members = format!("/v1/spaces/{sid}/members");
    s.expect(
        Method::POST,
        ALICE,
        &members,
        json!({"user_id":"bob"}),
        StatusCode::CREATED,
    )
    .await;
    let library = s
        .client
        .post(format!(
            "{}/v1/library/assets?scope=space&space_id={sid}&name=notes.txt",
            s.base
        ))
        .bearer_auth(BOB)
        .header("content-type", "text/plain")
        .body("private file")
        .send()
        .await
        .unwrap();
    assert_eq!(library.status(), StatusCode::CREATED);
    let asset = library.json::<Value>().await.unwrap();
    let base = format!("/v1/spaces/{sid}/rooms/lobby");
    let text = "@alice **important**\n\n`@agent`";
    // Legacy clients do not need any new fields to send a message.
    let event = s
        .expect(
            Method::POST,
            BOB,
            &format!("{base}/messages"),
            json!({"text":text, "asset_ids":[asset["id"]]}),
            StatusCode::ACCEPTED,
        )
        .await;
    assert_eq!(event["mentions"][0]["id"], "alice");
    assert!(event.get("forwarded").is_none());
    let thread = s
        .expect(
            Method::POST,
            ALICE,
            &format!("{base}/events/{}/thread", event["id"]),
            json!({}),
            StatusCode::CREATED,
        )
        .await;
    let rid = thread["id"].as_str().unwrap();
    s.classify(&sid).await;

    let source = in_room(&sid, "lobby", &event["id"]);
    let (status, result) = s
        .forward(
            ALICE,
            &source,
            &room(&sid, rid),
            Some("  @agent see this  "),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{result}");
    assert_eq!(result["omitted_attachments"], 1);
    let copy = &result["events"][1];
    assert_eq!(copy["text"], text);
    assert_eq!(
        copy["forwarded"],
        json!({
            "author_name":event["actor"], "author_kind":"person", "sent_at":event["created_at"],
            "same_space":{"room_id":"lobby", "event_id":event["id"]}
        })
    );
    let comment = &result["events"][0];
    assert_eq!(comment["text"], "@agent see this");
    assert!(comment.get("forwarded").is_none());
    for event in result["events"].as_array().unwrap() {
        assert_eq!(event["actor_id"], "alice");
        assert_eq!(event["kind"], "message");
        assert_eq!(event["audience"], "room");
        assert_eq!(event["mentions"], json!([]));
        assert_eq!(event["spans"], json!([]));
        for key in [
            "attachments",
            "activity",
            "reply_to",
            "replies",
            "reactions",
            "via",
        ] {
            assert!(event.get(key).is_none(), "unexpected {key}: {event}");
        }
    }
    assert_eq!(
        s.get(BOB, &format!("/v1/spaces/{sid}/rooms/{rid}/events"))
            .await["events"],
        result["events"]
    );
    assert_eq!(s.get(ALICE, "/v1/runs").await["runs"], json!([]));
    assert_eq!(s.classifier.0.load(Ordering::Relaxed), 0);

    // Legacy listings keep both kinds of thread; the picker can fetch just rooms.
    let agent_thread =
        s.db.create_room(&sid, "Agent thread", Some("lobby"), 10, 10)
            .unwrap();
    let rooms_url = format!("/v1/spaces/{sid}/rooms");
    let rooms = s.get(ALICE, &rooms_url).await;
    assert!(rooms["rooms"].as_array().unwrap().contains(&thread));
    assert!(
        rooms["rooms"]
            .as_array()
            .unwrap()
            .contains(&json!(agent_thread))
    );
    assert_eq!(
        s.get(BOB, &format!("{rooms_url}?top_level_only=true"))
            .await["rooms"],
        json!([rooms["rooms"][0]])
    );
    assert_eq!(
        s.get(ALICE, &format!("{rooms_url}?top_level_only=false"))
            .await,
        rooms
    );
}

#[tokio::test]
async fn cross_space_and_dm_forwards_do_not_expose_source_locations() {
    let s = Server::start().await;
    s.get(BOB, "/v1/me").await;
    let a = s.space(ALICE).await;
    let b = s.space(ALICE).await;
    let agent =
        s.db.append_event(
            &a,
            "lobby",
            kind::AGENT,
            &Author {
                id: None,
                name: "Coder".into(),
            },
            "**Reply**",
            &Addressed::room(),
        )
        .unwrap();
    let source = in_room(&a, "lobby", &json!(agent.id));
    let (status, cross) = s.forward(ALICE, &source, &room(&b, "lobby"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{cross}");
    let forwarded =
        json!({"author_name":"Coder", "author_kind":"agent", "sent_at":json!(agent)["created_at"]});
    assert_eq!(cross["events"][0]["forwarded"], forwarded);

    let (status, dm_copy) = s.forward(ALICE, &source, &to_dm("bob"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{dm_copy}");
    let dms = s.get(BOB, "/v1/dms").await;
    let dm = dms["dms"][0]["id"].as_str().unwrap();
    assert_eq!(
        s.get(BOB, &format!("/v1/dms/{dm}/events")).await["events"],
        dm_copy["events"]
    );
    assert_eq!(dm_copy["events"][0]["forwarded"], forwarded);

    // Forwarding a copy attributes it to the actual source message's author
    // and never propagates the original metadata.
    let source = json!({"kind":"dm", "dm_id":dm, "event_id":dm_copy["events"][0]["id"]});
    let (status, bounced) = s.forward(ALICE, &source, &room(&b, "lobby"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{bounced}");
    assert_eq!(
        bounced["events"][0]["forwarded"],
        json!({
            "author_kind":"person", "author_name":dm_copy["events"][0]["actor"], "sent_at":dm_copy["events"][0]["created_at"]
        })
    );
    assert_eq!(bounced["events"][0]["text"], "**Reply**");

    let note = s
        .expect(
            Method::POST,
            ALICE,
            "/v1/chat/messages",
            json!({"text":"A private note", "save_note":true}),
            StatusCode::ACCEPTED,
        )
        .await;
    let chat = json!({"kind":"chat", "event_id":note["id"]});
    let (status, result) = s.forward(ALICE, &chat, &to_dm("bob"), None).await;
    assert_eq!(status, StatusCode::CREATED, "{result}");
    assert_eq!(result["events"][0]["text"], "A private note");
    assert!(result["events"][0]["forwarded"].get("same_space").is_none());
}

#[tokio::test]
async fn inaccessible_and_deleted_sources_or_destinations_write_nothing() {
    let s = Server::start().await;
    let a = s.space(ALICE).await;
    let b = s.space(BOB).await;
    let source_room = format!("/v1/spaces/{a}/rooms/lobby");
    let event = s
        .expect(
            Method::POST,
            ALICE,
            &format!("{source_room}/messages"),
            json!({"text":"secret"}),
            StatusCode::ACCEPTED,
        )
        .await;
    let src = in_room(&a, "lobby", &event["id"]);
    let dst = room(&b, "lobby");
    let source_missing = (
        StatusCode::NOT_FOUND,
        json!({"error":"no such source message","code":"forward_source_not_found"}),
    );
    let dest_missing = (
        StatusCode::NOT_FOUND,
        json!({"error":"no such destination","code":"forward_destination_not_found"}),
    );
    let activity =
        s.db.append_event(
            &a,
            "lobby",
            kind::TOOL_CALL,
            &Author {
                id: None,
                name: "Coder".into(),
            },
            "not a message",
            &Addressed::room(),
        )
        .unwrap();
    assert_eq!(
        s.forward(
            ALICE,
            &in_room(&a, "lobby", &json!(activity.id)),
            &to_dm("bob"),
            None
        )
        .await,
        source_missing
    );
    assert_eq!(
        s.forward(BOB, &src, &dst, Some("comment")).await,
        source_missing
    );
    let members = format!("/v1/spaces/{a}/members");
    s.expect(
        Method::POST,
        ALICE,
        &members,
        json!({"user_id":"bob"}),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        s.forward(ALICE, &src, &dst, Some("comment")).await,
        dest_missing
    );
    assert_eq!(
        s.forward(BOB, &src, &room(&b, "absent"), None).await,
        dest_missing
    );
    for user in ["never", "no-such-user", "Alice"] {
        assert_eq!(
            s.forward(BOB, &src, &to_dm(user), Some("comment")).await,
            dest_missing
        );
    }
    assert_eq!(
        s.forward(BOB, &src, &to_dm("bob"), None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.forward(BOB, &src, &room(&a, "lobby"), None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.forward(
            BOB,
            &src,
            &dst,
            Some(&"x".repeat(Limits::default().max_message_chars + 1))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    s.expect(
        Method::DELETE,
        ALICE,
        &members,
        json!({"user_id":"bob"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        s.forward(BOB, &src, &dst, Some("comment")).await,
        source_missing
    );
    s.expect(
        Method::DELETE,
        ALICE,
        &format!("/v1/spaces/{a}"),
        json!({"confirm_name":"Test"}),
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(
        s.forward(ALICE, &src, &to_dm("bob"), Some("comment")).await,
        source_missing
    );
    assert_eq!(s.get(BOB, "/v1/dms").await["dms"], json!([]));
    assert_eq!(
        s.get(BOB, &format!("/v1/spaces/{b}/rooms/lobby/events"))
            .await["events"],
        json!([])
    );
}

#[tokio::test]
async fn private_sources_require_chat_ownership_and_visible_dm_history() {
    let s = Server::start().await;
    s.get(BOB, "/v1/me").await;
    let chat = s
        .expect(
            Method::POST,
            ALICE,
            "/v1/chat/messages",
            json!({"text":"secret", "save_note":true}),
            StatusCode::ACCEPTED,
        )
        .await;
    let src = json!({"kind":"chat", "event_id":chat["id"]});
    assert_eq!(
        s.forward(BOB, &src, &to_dm("alice"), None).await.0,
        StatusCode::NOT_FOUND
    );
    let result = s.forward(ALICE, &src, &to_dm("bob"), None).await.1;
    let dms = s.get(BOB, "/v1/dms").await;
    let dm = dms["dms"][0]["id"].as_str().unwrap();
    let source = json!({"kind":"dm", "dm_id":dm, "event_id":result["events"][0]["id"]});
    let outsider_space = s.space(NEVER).await;
    assert_eq!(
        s.forward(NEVER, &source, &room(&outsider_space, "lobby"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let space = s.space(BOB).await;
    let target = room(&space, "lobby");
    s.expect(
        Method::DELETE,
        BOB,
        &format!("/v1/dms/{dm}"),
        Value::Null,
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(
        s.forward(BOB, &source, &target, None).await.0,
        StatusCode::NOT_FOUND
    );
    s.expect(
        Method::DELETE,
        ALICE,
        &format!("/v1/dms/{dm}"),
        Value::Null,
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(
        s.forward(BOB, &source, &target, None).await.0,
        StatusCode::NOT_FOUND
    );
}
