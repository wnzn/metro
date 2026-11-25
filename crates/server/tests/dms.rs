use std::sync::Arc;

use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, router};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};

const ALICE: &str = "alice-test-token-0000000000000";
const BOB: &str = "bob-test-token-000000000000000";
const ADMIN: &str = "admin-test-token-0000000000000";

struct Server {
    http: Client,
    base: String,
}

impl Server {
    async fn start() -> Self {
        let users = json!({
            "alice": ALICE, "bob": BOB, "admin": ADMIN,
            "never": "never-used-test-token-00000000",
        })
        .to_string();
        let auth = metro_auth::Authenticator::from_lookup(move |name| {
            Ok((name == metro_auth::env::DEV_USERS).then(|| users.clone()))
        })
        .await
        .unwrap();
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits {
                events_page: 2,
                ..Limits::default()
            },
            org_admins: vec!["admin".into()],
        };
        let state = AppState::new(config, Db::open_in_memory().unwrap(), Arc::new(auth))
            .await
            .unwrap()
            .with_env_user_ids(
                ["alice", "bob", "admin", "never"]
                    .map(str::to_owned)
                    .to_vec(),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            http: Client::new(),
            base,
        }
    }

    async fn call(
        &self,
        method: Method,
        user: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(user);
        if !body.is_null() {
            req = req.json(&body);
        }
        let r = req.send().await.unwrap();
        (r.status(), r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, user: &str, path: &str) -> Value {
        let (status, body) = self.call(Method::GET, user, path, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    async fn create(&self, user: &str, peer: &str) -> (StatusCode, Value) {
        self.call(Method::POST, user, "/v1/dms", json!({"user_id": peer}))
            .await
    }

    async fn pair(&self) -> String {
        self.get(BOB, "/v1/me").await;
        let (status, dm) = self.create(ALICE, "bob").await;
        assert_eq!(status, StatusCode::CREATED);
        format!("/v1/dms/{}", dm["id"].as_str().unwrap())
    }

    async fn send(&self, user: &str, dm: &str, body: Value) -> Value {
        let (status, message) = self
            .call(Method::POST, user, &format!("{dm}/messages"), body)
            .await;
        assert_eq!(status, StatusCode::CREATED, "{message}");
        message
    }

    async fn hide(&self, user: &str, dm: &str) {
        assert_eq!(
            self.call(Method::DELETE, user, dm, Value::Null).await.0,
            StatusCode::NO_CONTENT
        );
    }
}

#[tokio::test]
async fn create_uses_signed_in_stable_ids_even_after_sign_out() {
    let s = Server::start().await;
    let (status, unknown) = s.create(ALICE, "nonexistent").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(s.create(ALICE, "never").await, (status, unknown.clone()));
    assert_eq!(s.create(ALICE, "alice").await.0, StatusCode::BAD_REQUEST);

    let (created, managed) = s
        .call(
            Method::POST,
            ADMIN,
            "/v1/admin/users",
            json!({"name": "Ana"}),
        )
        .await;
    assert_eq!(created, StatusCode::CREATED);
    let ana = managed["id"].as_str().unwrap();
    let token = managed["token"].as_str().unwrap();
    assert_eq!(s.create(ALICE, ana).await, (status, unknown.clone()));
    assert_eq!(s.create(ALICE, "Ana").await, (status, unknown));
    s.get(token, "/v1/me").await;
    let (revoked, _) = s
        .call(
            Method::DELETE,
            ADMIN,
            &format!("/v1/admin/users/{ana}/token"),
            Value::Null,
        )
        .await;
    assert_eq!(revoked, StatusCode::NO_CONTENT);
    assert_eq!(
        s.call(Method::GET, token, "/v1/dms", Value::Null).await.0,
        StatusCode::UNAUTHORIZED
    );
    let directory = s.get(ALICE, "/v1/directory").await;
    assert!(
        directory["users"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"] == ana)
    );
    assert!(
        !directory["users"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"] == "never")
    );
    let (created, dm) = s.create(ALICE, ana).await;
    assert_eq!(created, StatusCode::CREATED);
    assert_eq!(
        dm["peer"],
        json!({"id": ana, "display_name": "Ana", "avatar_version": null})
    );
    let path = format!("/v1/dms/{}", dm["id"].as_str().unwrap());
    assert!(dm.get("last").is_none());
    assert_eq!(
        s.get(ALICE, &format!("{path}/events")).await,
        json!({"events":[],"reactions":[]})
    );
    s.send(ALICE, &path, json!({"text":"Until you're back"}))
        .await;
}

#[tokio::test]
async fn participants_only_with_no_admin_bypass_or_implicit_agents() {
    let s = Server::start().await;
    let dm = s.pair().await;
    let (status, reverse) = s.create(BOB, "alice").await;
    assert_eq!(status, StatusCode::OK);
    assert!(dm.ends_with(reverse["id"].as_str().unwrap()));
    assert_eq!(s.get(ADMIN, "/v1/dms").await, json!({"dms":[]}));
    let message = s
        .send(ALICE, &dm, json!({"text": "  @agent **dm only**  "}))
        .await;
    assert_eq!(message["text"], "@agent **dm only**");
    assert_eq!(message["actor_id"], "alice");
    assert_eq!(message["kind"], "message");
    assert_eq!(message["mentions"], json!([]));
    assert_eq!(message["audience"], "room");
    assert!(message.get("activity").is_none());
    let eid = message["id"].as_i64().unwrap();
    for (method, suffix, body) in [
        (Method::GET, "/events", Value::Null),
        (Method::POST, "/messages", json!({"text":"denied"})),
        (Method::PUT, "/read", json!({"last_read":eid})),
        (
            Method::PUT,
            &format!("/events/{eid}/reactions"),
            json!({"emoji":"👀"}),
        ),
        (
            Method::DELETE,
            &format!("/events/{eid}/reactions"),
            json!({"emoji":"👀"}),
        ),
        (Method::DELETE, "", Value::Null),
    ] {
        let expected = s
            .call(
                method.clone(),
                ALICE,
                &format!("/v1/dms/dm-missing{suffix}"),
                body.clone(),
            )
            .await;
        assert_eq!(expected.0, StatusCode::NOT_FOUND);
        assert_eq!(
            s.call(method, ADMIN, &format!("{dm}{suffix}"), body).await,
            expected
        );
    }

    let reply = s
        .send(BOB, &dm, json!({"text": "Reply", "reply_to": eid}))
        .await;
    assert_eq!(reply["reply_to"], eid);
    let reaction = format!("{dm}/events/{eid}/reactions");
    for (method, changed) in [
        (Method::PUT, true),
        (Method::PUT, false),
        (Method::DELETE, true),
        (Method::DELETE, false),
        (Method::PUT, true),
    ] {
        assert_eq!(
            s.call(method, BOB, &reaction, json!({"emoji":"👀"})).await,
            (StatusCode::OK, json!({"changed":changed}))
        );
    }
    let events = s.get(ALICE, &format!("{dm}/events")).await;
    assert_eq!(events["events"], json!([message, reply]));
    assert_eq!(
        events["reactions"],
        json!([{
            "target_event_id":eid, "emoji":"👀", "reactor_kind":"user", "reactor_id":"bob"
        }])
    );
    assert_eq!(
        s.get(BOB, &format!("{dm}/events?after={eid}")).await,
        json!({"events":[reply],"reactions":[]})
    );
    assert_eq!(s.get(ALICE, "/v1/runs").await, json!({"runs":[]}));
    assert_eq!(s.get(BOB, "/v1/chat/events").await, json!({"events":[]}));
    for bad in [
        json!({"text":"  "}),
        json!({"text":"too long".repeat(1001)}),
        json!({"text":"bad reply","reply_to":999}),
        json!({"text":"no agents", "invoke_agent":true}),
    ] {
        let (status, _) = s
            .call(Method::POST, BOB, &format!("{dm}/messages"), bad)
            .await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[tokio::test]
async fn read_and_hide_are_per_person_until_both_hide_latest() {
    let s = Server::start().await;
    let dm = s.pair().await;
    let old = s.send(ALICE, &dm, json!({"text":"secret history"})).await;
    let old_id = old["id"].as_i64().unwrap();
    let read = format!("{dm}/read");
    assert_eq!(s.get(BOB, "/v1/dms").await["dms"][0]["unread"], 1);
    for eid in [old_id, 0] {
        assert_eq!(
            s.call(Method::PUT, BOB, &read, json!({"last_read":eid}))
                .await
                .0,
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(s.get(BOB, "/v1/dms").await["dms"][0]["unread"], 0);
    assert_eq!(
        s.call(Method::PUT, BOB, &read, json!({"last_read":99999}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );

    s.hide(ALICE, &dm).await;
    assert_eq!(s.get(ALICE, "/v1/dms").await, json!({"dms":[]}));
    assert_eq!(
        s.get(ALICE, &format!("{dm}/events")).await["events"],
        json!([])
    );
    assert_eq!(s.create(ALICE, "bob").await.0, StatusCode::OK);
    assert_eq!(s.get(ALICE, "/v1/dms").await, json!({"dms":[]}));
    assert_eq!(
        s.get(BOB, &format!("{dm}/events")).await["events"],
        json!([old])
    );
    assert_eq!(
        s.call(
            Method::PUT,
            ALICE,
            &format!("{dm}/events/{old_id}/reactions"),
            json!({"emoji":"👀"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.call(
            Method::POST,
            ALICE,
            &format!("{dm}/messages"),
            json!({"text":"hidden", "reply_to":old_id})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    let fresh = s.send(BOB, &dm, json!({"text": "界".repeat(170)})).await;
    let fresh_id = fresh["id"].as_i64().unwrap();
    let dms = s.get(ALICE, "/v1/dms").await;
    assert_eq!(dms["dms"][0]["last"]["preview"], "界".repeat(140));
    assert_eq!(dms["dms"][0]["unread"], 1);
    assert_eq!(
        s.get(ALICE, &format!("{dm}/events")).await["events"],
        json!([fresh])
    );
    assert_eq!(
        s.get(BOB, &format!("{dm}/events")).await["events"],
        json!([old, fresh])
    );
    assert_eq!(
        s.call(
            Method::PUT,
            BOB,
            &format!("{dm}/events/{fresh_id}/reactions"),
            json!({"emoji":"👍"})
        )
        .await
        .0,
        StatusCode::OK
    );

    s.hide(BOB, &dm).await; // Alice's earlier hide didn't cover the fresh message.
    assert_eq!(
        s.get(ALICE, &format!("{dm}/events")).await["events"],
        json!([fresh])
    );
    s.hide(ALICE, &dm).await;
    assert_eq!(
        s.call(Method::GET, ALICE, &format!("{dm}/events"), Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(s.get(BOB, "/v1/dms").await["dms"], json!([]));
    let (status, next) = s.create(ALICE, "bob").await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(!dm.ends_with(next["id"].as_str().unwrap()));
}

#[tokio::test]
async fn dms_grant_bio_access_without_changing_legacy_chat_and_space_messages() {
    let s = Server::start().await;
    assert_eq!(
        s.call(
            Method::PATCH,
            BOB,
            "/v1/me/profile",
            json!({"bio":"From Bob"})
        )
        .await
        .0,
        StatusCode::OK
    );
    let card = json!({"id":"bob", "display_name":"bob", "bio":null, "avatar_version":null});
    assert_eq!(s.get(ALICE, "/v1/profiles/bob").await, card);
    let dm = s.pair().await;
    assert_eq!(s.get(ALICE, "/v1/profiles/bob").await["bio"], "From Bob");
    assert_eq!(s.get(ADMIN, "/v1/profiles/bob").await, card);
    s.hide(BOB, &dm).await;
    assert_eq!(s.get(ALICE, "/v1/profiles/bob").await["bio"], "From Bob");
    s.hide(ALICE, &dm).await;
    assert_eq!(s.get(ALICE, "/v1/profiles/bob").await, card);

    let (status, space) = s
        .call(Method::POST, ALICE, "/v1/spaces", json!({"name":"Legacy"}))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let room = format!("/v1/spaces/{}/rooms/lobby", space["id"].as_str().unwrap());
    for (place, body) in [
        (room.as_str(), json!({"text":"old client space"})),
        (
            "/v1/chat",
            json!({"text":"old client note","invoke_agent":false}),
        ),
    ] {
        let (status, _) = s
            .call(
                Method::POST,
                ALICE,
                &format!("{place}/messages"),
                body.clone(),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let page = s.get(ALICE, &format!("{place}/events?after=0")).await;
        assert_eq!(page["events"][0]["text"], body["text"]);
        assert!(page["events"][0].get("reply_to").is_none());
    }
}
