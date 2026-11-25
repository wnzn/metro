//! `POST /v1/search/quick` over real HTTP: literal matching of messages
//! (personal Chat, rooms and threads) and library file names, scoped by
//! the caller's own Chat and library and their current Space membership,
//! with no model, no index and no file bytes.

use std::collections::HashMap;
use std::sync::Arc;

use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::{Addressed, Author, Db};
use metro_server::{AppState, Config, Limits, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

struct FakeAuth;

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            let users = HashMap::from([("alice", "alice"), ("bob", "bob"), ("carol", "carol")]);
            users
                .get(bearer)
                .map(|id| UserIdentity {
                    id: (*id).to_owned(),
                    actor: (*id).to_owned(),
                })
                .ok_or_else(|| AuthFailure::Rejected("unknown token".to_owned()))
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

struct App {
    base: String,
    http: reqwest::Client,
    state: AppState,
    db_path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn app() -> App {
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        db_path: ":memory:".into(),
        model: None,
        limits: Limits::default(),
        org_admins: vec![],
    };
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("metro.sqlite3");
    let state = AppState::new(config, Db::open(&db_path).unwrap(), Arc::new(FakeAuth))
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    App {
        base,
        http: reqwest::Client::new(),
        state,
        db_path,
        _dir: dir,
    }
}

impl App {
    async fn call(
        &self,
        method: reqwest::Method,
        token: Option<&str>,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .json(&body);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let r = req.send().await.unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::POST, Some(token), path, body)
            .await
    }

    /// A successful lookup's response.
    async fn quick(&self, token: &str, body: Value) -> Value {
        let (s, v) = self.post(token, "/v1/search/quick", body).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    async fn status(&self, token: Option<&str>, body: Value) -> StatusCode {
        self.call(reqwest::Method::POST, token, "/v1/search/quick", body)
            .await
            .0
    }

    async fn space(&self, owner: &str, name: &str, members: &[&str]) -> String {
        let (s, v) = self.post(owner, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let sid = v["id"].as_str().unwrap().to_owned();
        for m in members {
            let (s, v) = self
                .post(
                    owner,
                    &format!("/v1/spaces/{sid}/members"),
                    json!({"user_id": m}),
                )
                .await;
            assert_eq!(s, StatusCode::CREATED, "{v}");
        }
        sid
    }

    async fn say(&self, who: &str, sid: &str, room: &str, text: &str) -> i64 {
        let (s, v) = self
            .post(
                who,
                &format!("/v1/spaces/{sid}/rooms/{room}/messages"),
                json!({"text": text}),
            )
            .await;
        assert!(s.is_success(), "{s} {v}");
        v["id"]
            .as_i64()
            .or_else(|| v["event"]["id"].as_i64())
            .unwrap_or(0)
    }

    async fn chat(&self, who: &str, text: &str) {
        let (s, v) = self
            .post(
                who,
                "/v1/chat/messages",
                json!({"text": text, "save_note": true}),
            )
            .await;
        assert!(s.is_success(), "{s} {v}");
    }

    async fn upload(&self, who: &str, query: &str, mime: &str, bytes: &[u8]) -> String {
        let r = self
            .http
            .post(format!("{}/v1/library/assets?{query}", self.base))
            .bearer_auth(who)
            .header("content-type", mime)
            .body(bytes.to_vec())
            .send()
            .await
            .unwrap();
        let s = r.status();
        let v: Value = r.json().await.unwrap();
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["id"].as_str().unwrap().to_owned()
    }

    async fn thread(&self, sid: &str, name: &str) -> String {
        let (sid, name) = (sid.to_owned(), name.to_owned());
        self.state
            .db
            .call(move |db| {
                db.create_room(&sid, &name, Some("lobby"), 100, 100)
                    .map_err(|e| anyhow::anyhow!("{e:?}"))
            })
            .await
            .unwrap()
            .id
    }

    async fn event(&self, sid: &str, room: &str, kind: &str, text: &str) {
        let (sid, room, kind, text) = (
            sid.to_owned(),
            room.to_owned(),
            kind.to_owned(),
            text.to_owned(),
        );
        self.state
            .db
            .call(move |db| {
                db.append_event(
                    &sid,
                    &room,
                    &kind,
                    &Author::server("agent"),
                    &text,
                    &Addressed::room(),
                )
            })
            .await
            .unwrap();
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

fn texts(v: &Value) -> Vec<String> {
    v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["text"].as_str().unwrap().to_owned())
        .collect()
}
fn names(v: &Value) -> Vec<String> {
    v["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap().to_owned())
        .collect()
}
fn all(q: &str) -> Value {
    json!({"query": q, "scope": "all", "mode": "all"})
}

#[tokio::test]
async fn requests_are_authenticated_and_validated() {
    let app = app().await;
    let sid = app.space("alice", "Acme", &[]).await;
    assert_eq!(app.status(None, all("x")).await, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.status(Some("mallory"), all("x")).await,
        StatusCode::UNAUTHORIZED
    );
    let bad = [
        json!({"query": "   ", "scope": "all", "mode": "all"}),
        json!({"query": "x".repeat(201), "scope": "all", "mode": "all"}),
        json!({"query": "x", "scope": "space", "mode": "all"}),
        json!({"query": "x", "scope": "chat", "space_id": sid, "mode": "all"}),
        json!({"query": "x", "scope": "all", "mode": "conversations", "kind": "image"}),
    ];
    for b in bad {
        assert_eq!(
            app.status(Some("alice"), b.clone()).await,
            StatusCode::BAD_REQUEST,
            "{b}"
        );
    }
    let rejected = [
        json!({"query": "x", "scope": "everything", "mode": "all"}),
        json!({"query": "x", "scope": "all", "mode": "fuzzy"}),
        json!({"query": "x", "scope": "all", "mode": "all", "kind": "spreadsheet"}),
        json!({"query": "x", "scope": "all", "mode": "all", "limit": 50}),
        json!({"query": "x", "scope": "all"}),
    ];
    for b in rejected {
        assert!(
            app.status(Some("alice"), b.clone()).await.is_client_error(),
            "{b}"
        );
    }
    // Not a member, unknown, or not an id: the same 404.
    for space in [sid.as_str(), "s-nope", "../etc"] {
        let b = json!({"query": "x", "scope": "space", "space_id": space, "mode": "all"});
        assert_eq!(
            app.status(Some("bob"), b).await,
            StatusCode::NOT_FOUND,
            "{space}"
        );
    }
    let r = app
        .http
        .post(format!("{}/v1/search/quick", app.base))
        .bearer_auth("alice")
        .json(&all("x"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn messages_come_from_the_callers_chat_and_spaces_rooms_and_threads_only() {
    let app = app().await;
    let acme = app.space("alice", "Acme", &["bob"]).await;
    let other = app.space("carol", "Other", &[]).await;
    let thread = app.thread(&acme, "audit").await;
    app.chat("alice", "kiwi in alice's chat").await;
    app.chat("bob", "kiwi in bob's chat").await;
    app.say("bob", &acme, "lobby", "kiwi in the lobby").await;
    app.say("alice", &acme, &thread, "kiwi in the audit thread")
        .await;
    app.say("carol", &other, "lobby", "kiwi where alice isn't")
        .await;
    app.event(&acme, "lobby", "agent", "kiwi from the agent")
        .await;
    // Never matched: tool, activity and status events, and credential-like text.
    app.event(&acme, "lobby", "tool_result", "workspace.list ok\nkiwi.txt")
        .await;
    app.event(&acme, "lobby", "status", "kiwi status").await;
    app.say("alice", &acme, "lobby", "kiwi metro_rt_v2_secretsecret")
        .await;
    app.chat("alice", "kiwi metro_pc_secret").await;
    app.chat("alice", "kiwi metro_ut_secret").await;

    let v = app.quick("alice", all("KIWI")).await;
    let mut got = texts(&v);
    got.sort();
    assert_eq!(
        got,
        [
            "kiwi from the agent",
            "kiwi in alice's chat",
            "kiwi in the audit thread",
            "kiwi in the lobby"
        ]
    );
    assert!(!v.to_string().contains("metro_"), "{v}");
    let thread_hit = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["text"] == "kiwi in the audit thread")
        .unwrap();
    assert_eq!(thread_hit["space_id"], acme.as_str());
    assert_eq!(thread_hit["room_id"], thread.as_str());
    assert!(thread_hit["event_id"].as_i64().unwrap() > 0);
    assert_eq!(thread_hit["actor"], "alice");
    let chat_hit = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["text"] == "kiwi in alice's chat")
        .unwrap();
    assert_eq!(chat_hit["space_id"], Value::Null);
    assert_eq!(chat_hit["room_id"], Value::Null);
    // Only the documented fields.
    let keys: Vec<_> = chat_hit.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys.len(), 5, "{keys:?}");

    // Scopes never widen.
    let chat = app
        .quick(
            "alice",
            json!({"query": "kiwi", "scope": "chat", "mode": "all"}),
        )
        .await;
    assert_eq!(texts(&chat), ["kiwi in alice's chat"]);
    let space = app
        .quick(
            "alice",
            json!({"query": "kiwi", "scope": "space", "space_id": acme, "mode": "all"}),
        )
        .await;
    assert_eq!(space["messages"].as_array().unwrap().len(), 3);
    assert!(texts(&space).iter().all(|t| !t.contains("chat")));
    let personal = app
        .quick(
            "alice",
            json!({"query": "kiwi", "scope": "personal", "mode": "all"}),
        )
        .await;
    assert_eq!(personal["messages"], json!([]));
    let library = app
        .quick(
            "alice",
            json!({"query": "kiwi", "scope": "all", "mode": "library"}),
        )
        .await;
    assert_eq!(library["messages"], json!([]));
}

#[tokio::test]
async fn file_names_and_extensions_match_literally_within_visible_libraries() {
    let app = app().await;
    let acme = app.space("alice", "Acme", &["bob"]).await;
    let other = app.space("carol", "Other", &[]).await;
    app.upload(
        "alice",
        "scope=personal&name=budget.pdf",
        "application/pdf",
        b"%PDF-1.7\n",
    )
    .await;
    app.upload(
        "alice",
        &format!("scope=space&space_id={acme}&name=Q3%20budget.md"),
        "text/markdown",
        b"# q3",
    )
    .await;
    app.upload(
        "alice",
        &format!("scope=space&space_id={acme}&name=logo.png"),
        "image/png",
        PNG,
    )
    .await;
    app.upload(
        "bob",
        "scope=personal&name=bob-budget.txt",
        "text/plain",
        b"hi",
    )
    .await;
    app.upload(
        "carol",
        &format!("scope=space&space_id={other}&name=carol-budget.txt"),
        "text/plain",
        b"hi",
    )
    .await;
    app.upload(
        "alice",
        "scope=personal&name=pdf-notes.txt",
        "text/plain",
        b"hi",
    )
    .await;
    app.upload(
        "alice",
        "scope=personal&name=50%25_off.txt",
        "text/plain",
        b"hi",
    )
    .await;
    app.upload(
        "alice",
        "scope=personal&name=500xoff.txt",
        "text/plain",
        b"hi",
    )
    .await;

    let mut got = names(&app.quick("alice", all("budget")).await);
    got.sort();
    assert_eq!(got, ["Q3 budget.md", "budget.pdf"]);
    // Extension: the literal ".pdf", not any name containing "pdf".
    assert_eq!(
        names(&app.quick("alice", all(".PDF")).await),
        ["budget.pdf"]
    );
    // Wildcards are literal.
    assert_eq!(
        names(&app.quick("alice", all("50%_")).await),
        ["50%_off.txt"]
    );
    // "_" is not "any character": "500xoff.txt" does not match "0_o".
    assert_eq!(
        names(&app.quick("alice", all("0_o")).await),
        Vec::<String>::new()
    );
    assert_eq!(
        app.quick("alice", all("%")).await["assets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let asset = &app.quick("alice", all("logo")).await["assets"][0];
    for key in [
        "id",
        "name",
        "mime_type",
        "kind",
        "size_bytes",
        "space_id",
        "uploader_id",
        "sha256",
        "created_at",
    ] {
        assert!(asset.get(key).is_some(), "{key}: {asset}");
    }
    assert_eq!(asset["space_id"], acme.as_str());

    // Scopes and kinds narrow; never widen.
    let personal = app
        .quick(
            "alice",
            json!({"query": "budget", "scope": "personal", "mode": "all"}),
        )
        .await;
    assert_eq!(names(&personal), ["budget.pdf"]);
    let space = app
        .quick(
            "alice",
            json!({"query": "budget", "scope": "space", "space_id": acme, "mode": "library"}),
        )
        .await;
    assert_eq!(names(&space), ["Q3 budget.md"]);
    let chat = app
        .quick(
            "alice",
            json!({"query": "budget", "scope": "chat", "mode": "all"}),
        )
        .await;
    assert_eq!(chat["assets"], json!([]));
    let conversations = app
        .quick(
            "alice",
            json!({"query": "budget", "scope": "all", "mode": "conversations"}),
        )
        .await;
    assert_eq!(conversations["assets"], json!([]));
    let images = app
        .quick(
            "alice",
            json!({"query": ".", "scope": "all", "mode": "library", "kind": "image"}),
        )
        .await;
    assert_eq!(names(&images), ["logo.png"]);
    let docs = app
        .quick(
            "alice",
            json!({"query": "budget", "scope": "all", "mode": "all", "kind": "document"}),
        )
        .await;
    assert_eq!(docs["assets"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn an_older_file_is_still_found_behind_newer_ones() {
    let app = app().await;
    let old = app
        .upload(
            "alice",
            "scope=personal&name=archive-2019.txt",
            "text/plain",
            b"old",
        )
        .await;
    {
        let conn = rusqlite::Connection::open(&app.db_path).unwrap();
        conn.execute(
            "UPDATE library_assets SET created_at = 1546300800000 WHERE id = ?1",
            [&old],
        )
        .unwrap();
    }
    for i in 0..12 {
        app.upload(
            "alice",
            &format!("scope=personal&name=new-{i}.txt"),
            "text/plain",
            b"new",
        )
        .await;
    }
    let v = app.quick("alice", all("archive")).await;
    assert_eq!(names(&v), ["archive-2019.txt"]);
    assert_eq!(v["assets"][0]["created_at"], 1546300800000_i64);
}

#[tokio::test]
async fn removal_and_deletion_take_effect_on_the_next_lookup() {
    let app = app().await;
    let acme = app.space("alice", "Acme", &["bob"]).await;
    app.say("alice", &acme, "lobby", "plum plans").await;
    let file = app
        .upload(
            "alice",
            &format!("scope=space&space_id={acme}&name=plum.txt"),
            "text/plain",
            b"p",
        )
        .await;
    let v = app.quick("bob", all("plum")).await;
    assert_eq!((texts(&v).len(), names(&v).len()), (1, 1));

    let (s, v) = app
        .call(
            reqwest::Method::DELETE,
            Some("alice"),
            &format!("/v1/spaces/{acme}/members"),
            json!({"user_id": "bob"}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let v = app.quick("bob", all("plum")).await;
    assert_eq!(v, json!({"assets": [], "messages": []}));
    let b = json!({"query": "plum", "scope": "space", "space_id": acme, "mode": "all"});
    assert_eq!(app.status(Some("bob"), b).await, StatusCode::NOT_FOUND);

    let r = app
        .http
        .delete(format!("{}/v1/library/assets/{file}", app.base))
        .bearer_auth("alice")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    assert_eq!(
        names(&app.quick("alice", all("plum")).await),
        Vec::<String>::new()
    );
    assert_eq!(
        texts(&app.quick("alice", all("plum")).await),
        ["plum plans"]
    );
}

#[tokio::test]
async fn results_are_capped_shared_newest_first_and_clipped() {
    let app = app().await;
    let acme = app.space("alice", "Acme", &[]).await;
    for i in 0..10 {
        app.say("alice", &acme, "lobby", &format!("fig note {i}"))
            .await;
        app.upload(
            "alice",
            &format!("scope=personal&name=fig-{i}.txt"),
            "text/plain",
            b"f",
        )
        .await;
    }
    let v = app.quick("alice", all("fig")).await;
    assert_eq!((texts(&v).len(), names(&v).len()), (4, 4));
    assert_eq!(texts(&v)[0], "fig note 9");
    let only = app
        .quick(
            "alice",
            json!({"query": "fig", "scope": "all", "mode": "conversations"}),
        )
        .await;
    assert_eq!(texts(&only).len(), 8);
    assert_eq!(texts(&only)[..2], ["fig note 9", "fig note 8"]);
    // The same data gives the same answer.
    assert_eq!(app.quick("alice", all("fig")).await, v);

    let long = format!("{} fig {}", "a".repeat(500), "b".repeat(500));
    app.say("alice", &acme, "lobby", &long).await;
    let v = app
        .quick(
            "alice",
            json!({"query": "fig", "scope": "all", "mode": "conversations"}),
        )
        .await;
    let first = &texts(&v)[0];
    assert!(
        first.contains("fig") && first.starts_with('…') && first.ends_with('…'),
        "{first}"
    );
    assert!(first.chars().count() <= 162, "{}", first.chars().count());
}

#[tokio::test]
async fn unrelated_traffic_never_pushes_a_callers_messages_out_of_reach() {
    let app = app().await;
    let acme = app.space("alice", "Acme", &[]).await;
    let busy = app.space("carol", "Busy", &[]).await;
    app.chat("alice", "quince in alice's chat").await;
    app.say("alice", &acme, "lobby", "quince in the lobby")
        .await;
    // Millions of other people's events later (an id gap far beyond any window).
    {
        let conn = rusqlite::Connection::open(&app.db_path).unwrap();
        let n = conn
            .execute(
                "UPDATE sqlite_sequence SET seq = seq + 5000000
                 WHERE name IN ('personal_events', 'space_events')",
                [],
            )
            .unwrap();
        assert_eq!(n, 2);
    }
    app.chat("carol", "quince elsewhere").await;
    app.say("carol", &busy, "lobby", "quince elsewhere").await;
    let mut got = texts(&app.quick("alice", all("quince")).await);
    got.sort();
    assert_eq!(got, ["quince in alice's chat", "quince in the lobby"]);
    let space = app
        .quick(
            "alice",
            json!({"query": "quince", "scope": "space", "space_id": acme, "mode": "conversations"}),
        )
        .await;
    assert_eq!(texts(&space), ["quince in the lobby"]);
}

#[tokio::test]
async fn credential_like_names_never_hide_an_older_valid_file() {
    let app = app().await;
    app.upload(
        "alice",
        "scope=personal&name=report-final.txt",
        "text/plain",
        b"ok",
    )
    .await;
    for i in 0..20 {
        app.upload(
            "alice",
            &format!("scope=personal&name=report-metro_rt_{i}.txt"),
            "text/plain",
            b"x",
        )
        .await;
    }
    let v = app
        .quick(
            "alice",
            json!({"query": "report", "scope": "all", "mode": "library"}),
        )
        .await;
    assert_eq!(names(&v), ["report-final.txt"]);
}
