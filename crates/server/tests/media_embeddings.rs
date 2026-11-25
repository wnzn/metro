//! Media embeddings (EmbeddingGemma 2 adapters) over real HTTP, against a
//! loopback fixture that implements the Metro media contract
//! (`GET /v1/models`, `POST /v1/media/embeddings`, `POST /v1/embeddings`):
//!
//! - the server adapter on an operator-approved endpoint: non-text bytes
//!   arrive intact with the saved model, queries and documents keep their
//!   role as the request's shape, text search embeddings still work, model
//!   ids come from the endpoint, and selection shows `media_inputs`;
//! - text-only and chat connections refuse media before anything is sent;
//! - borrowed use only in the shared Space, stopped by revocation and by
//!   leaving the Space, and an answer withheld when the grant ends mid-call;
//! - the device adapter through this server's real device routes and the
//!   real `metro-connector` client and job runner, with an asset larger
//!   than the text relay bound.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::DefaultBodyLimit;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use base64::Engine;
use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::device_relay::DeviceError;
use metro_server::providers::secret::MasterKey;
use metro_server::providers::{self, ProviderError, ProviderService, UseError};
use metro_server::{AppState, Config, Limits, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ALICE: &str = "alice-token";
const BOB: &str = "bob-token";
const CAROL: &str = "carol-token";
const MEDIA_KEY: &str = "media-key-never-shown-0123456789";
const GEMMA: &str = "google/embeddinggemma-2";
const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

struct FakeAuth(HashMap<String, UserIdentity>);

impl HumanAuth for FakeAuth {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            self.0
                .get(bearer)
                .cloned()
                .ok_or_else(|| AuthFailure::Rejected("unknown token".to_owned()))
        })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

fn auth() -> Arc<FakeAuth> {
    let user = |id: &str| UserIdentity {
        id: id.to_owned(),
        actor: id.to_owned(),
    };
    Arc::new(FakeAuth(HashMap::from([
        (ALICE.to_owned(), user("alice")),
        (BOB.to_owned(), user("bob")),
        (CAROL.to_owned(), user("carol")),
    ])))
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Not text: PNG signature, NUL, every byte value.
fn binary(extra: usize) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00];
    b.extend((0..256 + extra).map(|i| (i % 256) as u8));
    b
}

fn asset_input(bytes: &[u8]) -> Value {
    json!({"asset": {"name": "scan.png", "mime_type": "Image/PNG", "data_base64": b64(bytes)}})
}

// ---- The model service fixture ----

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    authorization: Option<String>,
    body: Value,
}

struct Fixture {
    base_url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Media answers wait for a read lock: a test holding the write lock
    /// keeps them pending after the request arrived.
    gate: Arc<tokio::sync::RwLock<()>>,
}

impl Fixture {
    async fn start() -> Self {
        let seen = Arc::new(Mutex::new(Vec::<Seen>::new()));
        let gate = Arc::new(tokio::sync::RwLock::new(()));
        let record = |seen: &Arc<Mutex<Vec<Seen>>>, path: &str, h: &HeaderMap, body: Value| {
            seen.lock().unwrap().push(Seen {
                path: path.to_owned(),
                authorization: h
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_owned()),
                body,
            });
        };
        let (s1, s2, s3, g) = (seen.clone(), seen.clone(), seen.clone(), gate.clone());
        let app = axum::Router::new()
            .route(
                "/v1/models",
                get(move |h: HeaderMap| {
                    let seen = s1.clone();
                    async move {
                        record(&seen, "/v1/models", &h, Value::Null);
                        Json(json!({"object": "list", "data": [
                            {"id": GEMMA}, {"id": "acme/other-media-embedder"}]}))
                    }
                }),
            )
            .route(
                "/v1/media/embeddings",
                post(move |h: HeaderMap, Json(body): Json<Value>| {
                    let (seen, gate) = (s2.clone(), g.clone());
                    async move {
                        let query = body.get("query").is_some();
                        let mime = body["asset"]["mime_type"].as_str().unwrap_or("").to_owned();
                        record(&seen, "/v1/media/embeddings", &h, body);
                        let _open = gate.read().await;
                        if query {
                            return Json(json!({"model": GEMMA, "partial": false, "data": [
                                {"index": 0, "embedding": [0.1, 0.9, 0.0],
                                 "locator": {"kind": "query"}}]}));
                        }
                        if mime == DOCX {
                            return Json(json!({"model": GEMMA, "partial": false, "data": [
                                {"index": 0, "embedding": [0.3, 0.3, 0.9], "excerpt": "Minutes",
                                 "locator": {"kind": "text", "start": 0, "end": 7}}]}));
                        }
                        Json(
                            json!({"model": GEMMA, "partial": true, "debug": "dropped", "data": [
                            {"index": 1, "embedding": [0.0, 1.0, 0.0], "excerpt": "page two",
                             "locator": {"kind": "page", "page": 2}},
                            {"index": 0, "embedding": [1.0, 0.0, 0.0], "excerpt": "page one",
                             "locator": {"kind": "page", "page": 1}}]}),
                        )
                    }
                })
                .layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
            )
            .route(
                "/v1/embeddings",
                post(move |h: HeaderMap, Json(body): Json<Value>| {
                    let seen = s3.clone();
                    async move {
                        let n = body["input"].as_array().unwrap().len();
                        record(&seen, "/v1/embeddings", &h, body);
                        let data: Vec<Value> = (0..n)
                            .map(|i| json!({"index": i, "embedding": [i as f64 + 1.0, 0.5]}))
                            .collect();
                        Json(json!({"object": "list", "data": data}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base_url: format!("http://{addr}/v1"),
            seen,
            gate,
        }
    }

    fn seen(&self, path: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.path == path)
            .cloned()
            .collect()
    }
}

// ---- Metro ----

struct Server {
    base: String,
    http: reqwest::Client,
    state: AppState,
    fixture: Fixture,
    /// For file-backed servers: the database file and its directory.
    db_file: Option<(tempfile::TempDir, std::path::PathBuf)>,
}

impl Server {
    async fn start() -> Self {
        Self::start_with(None).await
    }

    /// A server on a database file, so a test can read its tables.
    async fn start_on_file() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metro.db");
        Self::start_with(Some((dir, path))).await
    }

    fn sqlite(&self) -> rusqlite::Connection {
        let conn =
            rusqlite::Connection::open(&self.db_file.as_ref().expect("file-backed").1).unwrap();
        // This test connection only seeds index rows without uploads.
        conn.execute_batch("PRAGMA foreign_keys = OFF; PRAGMA busy_timeout = 5000;")
            .unwrap();
        conn
    }

    async fn start_with(db_file: Option<(tempfile::TempDir, std::path::PathBuf)>) -> Self {
        let fixture = Fixture::start().await;
        let db = match &db_file {
            Some((_, path)) => Db::open(path).unwrap(),
            None => Db::open_in_memory().unwrap(),
        };
        let config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path: ":memory:".into(),
            model: None,
            limits: Limits {
                agent_timeout: Duration::from_secs(20),
                ..Limits::default()
            },
            org_admins: vec![],
        };
        let mut state = AppState::new(config, db, auth()).await.unwrap();
        let master =
            MasterKey::parse(&base64::engine::general_purpose::STANDARD.encode([9u8; 32])).unwrap();
        state.providers =
            ProviderService::new(Some(master), vec![fixture.base_url.clone()]).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            state,
            fixture,
            db_file,
        }
    }

    async fn send(
        &self,
        method: reqwest::Method,
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
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(reqwest::Method::POST, token, path, Some(body))
            .await
    }

    async fn create(&self, token: &str, body: Value) -> Value {
        let (status, c) = self.post(token, "/v1/providers", body).await;
        assert_eq!(status, StatusCode::CREATED, "{c}");
        c
    }

    async fn share(&self, token: &str, id: &str, body: Value) {
        let (status, b) = self
            .send(
                reqwest::Method::PUT,
                token,
                &format!("/v1/providers/{id}/sharing"),
                Some(body),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{b}");
    }

    async fn space(&self, token: &str, name: &str, members: &[&str]) -> String {
        let (status, space) = self.post(token, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(status, StatusCode::CREATED, "{space}");
        let sid = space["id"].as_str().unwrap().to_owned();
        for m in members {
            let (status, body) = self
                .post(
                    token,
                    &format!("/v1/spaces/{sid}/members"),
                    json!({"user_id": m}),
                )
                .await;
            assert!(status.is_success(), "{status} {body}");
        }
        sid
    }

    async fn available(&self, token: &str, query: &str) -> Vec<Value> {
        let (status, body) = self
            .send(
                reqwest::Method::GET,
                token,
                &format!("/v1/models/available{query}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["models"].as_array().cloned().unwrap_or_default()
    }

    async fn media(
        &self,
        user: &str,
        space: Option<&str>,
        id: &str,
        input: Value,
    ) -> Result<Value, ProviderError> {
        providers::media_embed(&self.state, user, space, id, input).await
    }
}

fn find<'a>(models: &'a [Value], id: &str) -> Option<&'a Value> {
    models.iter().find(|m| m["connection_id"] == id)
}

#[tokio::test]
async fn the_server_adapter_embeds_media_on_its_approved_endpoint() {
    let s = Server::start().await;
    let f = &s.fixture;
    // No model given: the suggested one. The endpoint must be approved.
    let (status, _) = s
        .post(
            ALICE,
            "/v1/providers",
            json!({"name": "gemma", "provider_id": "embeddinggemma-2",
                   "base_url": "http://127.0.0.1:1/v1"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let gemma = s
        .create(
            ALICE,
            json!({"name": "gemma", "provider_id": "embeddinggemma-2", "base_url": f.base_url}),
        )
        .await;
    assert_eq!(gemma["model"], GEMMA);
    assert_eq!(gemma["media_inputs"], true);
    let gid = gemma["id"].as_str().unwrap().to_owned();
    let text = s
        .create(
            ALICE,
            json!({"name": "text", "provider_id": "custom-embeddings",
                   "base_url": f.base_url, "model": "emb-small"}),
        )
        .await;
    assert_eq!(text["media_inputs"], false);
    let tid = text["id"].as_str().unwrap().to_owned();
    let chat = s
        .create(
            ALICE,
            json!({"name": "chat", "provider_id": "custom", "base_url": f.base_url,
                   "model": "llama3"}),
        )
        .await;
    let cid = chat["id"].as_str().unwrap().to_owned();

    // Selection shows which connections take media.
    let models = s.available(ALICE, "").await;
    let g = find(&models, &gid).unwrap();
    assert_eq!(g["media_inputs"], true);
    assert_eq!(g["capability"], "embeddings");
    assert_eq!(g["modalities"], json!(["text", "image", "audio", "video"]));
    assert_eq!(find(&models, &tid).unwrap()["media_inputs"], false);
    assert!(find(&models, &tid).unwrap().get("modalities").is_none());
    assert_eq!(find(&models, &cid).unwrap()["media_inputs"], false);
    let (_, catalog) = s
        .send(reqwest::Method::GET, ALICE, "/v1/provider-catalog", None)
        .await;
    let entry = catalog["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "embeddinggemma-2")
        .unwrap();
    assert_eq!(entry["media_inputs"], true);
    assert_eq!(entry["default_model"], GEMMA);

    // Exact model ids from the endpoint.
    let (status, listed) = s
        .post(ALICE, &format!("/v1/providers/{gid}/models"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(
        listed["models"],
        json!(["acme/other-media-embedder", GEMMA])
    );

    // Capability checks before any call.
    assert_eq!(
        providers::media_capable(&s.state, "alice", None, &gid).await,
        Ok(true)
    );
    assert_eq!(
        providers::media_capable(&s.state, "alice", None, &tid).await,
        Ok(false)
    );
    assert_eq!(
        providers::media_capable(&s.state, "alice", None, &cid).await,
        Ok(false)
    );
    assert_eq!(
        providers::media_capable(&s.state, "bob", None, &gid).await,
        Err(ProviderError::Use(UseError::NotFound))
    );

    // A document: non-text bytes arrive intact, with the saved model.
    let bytes = binary(4096);
    let out = s
        .media("alice", None, &gid, asset_input(&bytes))
        .await
        .unwrap();
    assert_eq!(out["partial"], true);
    assert_eq!(
        out["data"][0]["locator"],
        json!({"kind": "page", "page": 1})
    );
    assert_eq!(out["data"][1]["excerpt"], "page two");
    assert!(out.get("debug").is_none());
    let sent = f.seen("/v1/media/embeddings");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].body["model"], GEMMA);
    assert!(sent[0].authorization.is_none(), "no key, no header");
    assert_eq!(sent[0].body["asset"]["mime_type"], "image/png");
    let got = base64::engine::general_purpose::STANDARD
        .decode(sent[0].body["asset"]["data_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(got, bytes);
    // A query: the role is the request's shape; prompts are the service's.
    let q = s
        .media("alice", None, &gid, json!({"query": "the red bike"}))
        .await
        .unwrap();
    assert_eq!(q["data"].as_array().unwrap().len(), 1);
    assert_eq!(q["data"][0]["locator"]["kind"], "query");
    assert_eq!(
        f.seen("/v1/media/embeddings")[1].body,
        json!({"model": GEMMA, "query": "the red bike"})
    );

    // PDF and DOCX go as bytes for the service to prepare (pages, text).
    let pdf = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let out = s
        .media(
            "alice",
            None,
            &gid,
            json!({"asset": {"name": "invoice.pdf", "mime_type": "application/pdf",
                             "data_base64": b64(&pdf)}}),
        )
        .await
        .unwrap();
    assert_eq!(out["data"][0]["locator"]["kind"], "page");
    let docx = [b"PK\x03\x04".as_slice(), &[0u8, 0xff], b"word/document.xml"].concat();
    let out = s
        .media(
            "alice",
            None,
            &gid,
            json!({"asset": {"name": "minutes.docx", "mime_type": DOCX,
                             "data_base64": b64(&docx)}}),
        )
        .await
        .unwrap();
    assert_eq!(
        out["data"][0]["locator"],
        json!({"kind": "text", "start": 0, "end": 7})
    );
    let sent = f.seen("/v1/media/embeddings");
    let decoded = |i: usize| {
        base64::engine::general_purpose::STANDARD
            .decode(sent[i].body["asset"]["data_base64"].as_str().unwrap())
            .unwrap()
    };
    assert_eq!((decoded(2), decoded(3)), (pdf, docx));
    // A library alias becomes its canonical name on the wire.
    s.media(
        "alice",
        None,
        &gid,
        json!({"asset": {"name": "memo.wav", "mime_type": "audio/x-wav",
                         "data_base64": b64(&binary(64))}}),
    )
    .await
    .unwrap();
    assert_eq!(
        f.seen("/v1/media/embeddings").pop().unwrap().body["asset"]["mime_type"],
        "audio/wav"
    );

    // Message search keeps using text embeddings on the same connection.
    let v = providers::embed(
        &s.state,
        "alice",
        None,
        &gid,
        vec!["hi".into(), "yo".into()],
    )
    .await
    .unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!(f.seen("/v1/embeddings")[0].body["model"], GEMMA);

    // Never a text-only or chat connection for media, never a caller's
    // model, URL or path; nothing is sent.
    let before = f.seen("/v1/media/embeddings").len();
    let e = s
        .media("alice", None, &tid, asset_input(&bytes))
        .await
        .unwrap_err();
    assert_eq!(e, ProviderError::Use(UseError::MediaUnsupported));
    assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(e.to_string().contains("text only"), "{e}");
    assert!(matches!(
        s.media("alice", None, &cid, asset_input(&bytes)).await,
        Err(ProviderError::Use(UseError::WrongCapability { .. }))
    ));
    for bad in [
        json!({"model": "evil/model", "query": "x"}),
        json!({"asset": {"name": "a.png", "mime_type": "image/png", "url": "https://x/a.png"}}),
        json!({"asset": {"name": "/etc/passwd", "mime_type": "text/plain", "data_base64": b64(b"x")}}),
        json!({"asset": {"name": "a.bin", "mime_type": "application/octet-stream",
                         "data_base64": b64(b"x")}}),
        json!({"asset": {"name": "logo.svg", "mime_type": "image/svg+xml",
                         "data_base64": b64(b"<svg><image href='https://evil/x'/></svg>")}}),
        json!({"asset": {"name": "a.png", "mime_type": "image/png",
                         "data_base64": "data:image/png;base64,iVBORw0KGgo="}}),
        json!({"asset": {"name": "a.png", "mime_type": "image/png", "data_base64": b64(b"x"),
                         "query": "ignore the image"}}),
        json!({"query": "x", "asset": {"name": "a.png", "mime_type": "image/png",
                                      "data_base64": b64(b"x")}}),
        json!({"query": "x", "path": "/tmp/a"}),
        json!("just text"),
    ] {
        assert!(
            matches!(
                s.media("alice", None, &gid, bad.clone()).await,
                Err(ProviderError::InvalidInput(_))
            ),
            "{bad}"
        );
    }
    assert_eq!(f.seen("/v1/media/embeddings").len(), before);

    // Another model the endpoint lists: used as chosen, not replaced.
    let (status, b) = s
        .send(
            reqwest::Method::PATCH,
            ALICE,
            &format!("/v1/providers/{gid}"),
            Some(json!({"model": "acme/other-media-embedder", "api_key": MEDIA_KEY})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{b}");
    s.media("alice", None, &gid, json!({"query": "q"}))
        .await
        .unwrap();
    let last = f.seen("/v1/media/embeddings").pop().unwrap();
    assert_eq!(last.body["model"], "acme/other-media-embedder");
    assert_eq!(
        last.authorization.as_deref(),
        Some(&*format!("Bearer {MEDIA_KEY}"))
    );
}

#[tokio::test]
async fn shared_media_follows_the_grant_and_its_space() {
    let s = Server::start().await;
    let f = &s.fixture;
    let gid = s
        .create(
            ALICE,
            json!({"name": "gemma", "provider_id": "embeddinggemma-2", "base_url": f.base_url,
                   "api_key": MEDIA_KEY}),
        )
        .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let s1 = s.space(ALICE, "one", &["bob"]).await;
    let s2 = s.space(ALICE, "two", &["bob"]).await;
    let q = || json!({"query": "q"});
    // Not shared: bob can't use it anywhere.
    for space in [None, Some(s1.as_str())] {
        assert_eq!(
            s.media("bob", space, &gid, q()).await,
            Err(ProviderError::Use(UseError::NotFound))
        );
    }
    s.share(
        ALICE,
        &gid,
        json!({"visibility": "spaces", "space_ids": [s1]}),
    )
    .await;
    // Shared in s1 only: there it runs on alice's endpoint and key.
    let out = s.media("bob", Some(&s1), &gid, q()).await.unwrap();
    assert_eq!(out["data"][0]["locator"]["kind"], "query");
    assert_eq!(
        f.seen("/v1/media/embeddings")[0].authorization.as_deref(),
        Some(&*format!("Bearer {MEDIA_KEY}"))
    );
    for space in [None, Some(s2.as_str())] {
        assert_eq!(
            s.media("bob", space, &gid, q()).await,
            Err(ProviderError::Use(UseError::NotFound))
        );
    }
    assert_eq!(
        s.media("carol", Some(&s1), &gid, q()).await,
        Err(ProviderError::Use(UseError::NotFound))
    );
    // The picker says so, without the key or endpoint.
    let models = s.available(BOB, &format!("?space_id={s1}")).await;
    let m = find(&models, &gid).unwrap();
    assert_eq!(m["media_inputs"], true);
    assert_eq!(m["runs_on"], "owner_connection");
    assert!(!m.to_string().contains(MEDIA_KEY) && !m.to_string().contains(&f.base_url));
    // A preference saved without a scope: valid because it is usable in s1;
    // each use still names its destination.
    assert_eq!(
        providers::media_capable(&s.state, "bob", None, &gid).await,
        Err(ProviderError::Use(UseError::NotFound))
    );
    assert_eq!(
        providers::media_capable_in_any_scope(&s.state, "bob", &gid).await,
        Ok(true)
    );
    assert_eq!(
        providers::media_capable_in_any_scope(&s.state, "carol", &gid).await,
        Err(ProviderError::Use(UseError::NotFound))
    );

    // The grant ends while a call runs: the answer is withheld.
    let gate = f.gate.write().await;
    let state = s.state.clone();
    let (sid, id) = (s1.clone(), gid.clone());
    let pending = tokio::spawn(async move {
        providers::media_embed(&state, "bob", Some(&sid), &id, json!({"query": "late"})).await
    });
    for _ in 0..200 {
        if f.seen("/v1/media/embeddings").len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    s.share(ALICE, &gid, json!({"visibility": "personal"}))
        .await;
    drop(gate);
    assert_eq!(
        pending.await.unwrap(),
        Err(ProviderError::Use(UseError::Revoked))
    );
    // Revoked: nothing more is sent.
    let before = f.seen("/v1/media/embeddings").len();
    assert_eq!(
        s.media("bob", Some(&s1), &gid, q()).await,
        Err(ProviderError::Use(UseError::NotFound))
    );
    // Shared again, then bob leaves the Space: refused again.
    s.share(
        ALICE,
        &gid,
        json!({"visibility": "spaces", "space_ids": [s1]}),
    )
    .await;
    s.media("bob", Some(&s1), &gid, q()).await.unwrap();
    let (status, b) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/spaces/{s1}/members"),
            Some(json!({"user_id": "bob"})),
        )
        .await;
    assert!(status.is_success(), "{status} {b}");
    assert_eq!(
        s.media("bob", Some(&s1), &gid, q()).await,
        Err(ProviderError::Use(UseError::NotFound))
    );
    assert_eq!(f.seen("/v1/media/embeddings").len(), before + 1);
}

#[tokio::test]
async fn media_calls_are_shared_fairly_between_people_and_connections() {
    let s = Server::start().await;
    let f = &s.fixture;
    let gemma = |token: &'static str| {
        let s = &s;
        async move {
            s.create(
                token,
                json!({"name": "gemma", "provider_id": "embeddinggemma-2", "base_url": f.base_url}),
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let alice_g = gemma(ALICE).await;
    let carol_g = gemma(CAROL).await;
    s.share(ALICE, &alice_g, json!({"visibility": "organization"}))
        .await;
    let doc = || asset_input(&binary(3 * 1024 * 1024));
    let gate = f.gate.write().await;
    let call = |user: &'static str, id: &str| {
        let (state, id, input) = (s.state.clone(), id.to_owned(), doc());
        tokio::spawn(async move { providers::media_embed(&state, user, None, &id, input).await })
    };
    let arrived = |n: usize| async move {
        for _ in 0..400 {
            if f.seen("/v1/media/embeddings").len() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("expected {n} media requests");
    };
    // Alice has two in flight: her third is refused at once, nothing sent.
    let a = [call("alice", &alice_g), call("alice", &alice_g)];
    arrived(2).await;
    assert_eq!(
        s.media("alice", None, &alice_g, doc()).await,
        Err(ProviderError::Busy)
    );
    // Bob is not held back by her, on the same shared connection.
    let b = call("bob", &alice_g);
    arrived(3).await;
    // That connection now serves three: carol is refused there...
    assert_eq!(
        s.media("carol", None, &alice_g, doc()).await,
        Err(ProviderError::Busy)
    );
    // ...and goes through on her own connection (the last server slot).
    let c = call("carol", &carol_g);
    arrived(4).await;
    // Every server slot is now taken.
    assert_eq!(
        s.media("bob", None, &carol_g, json!({"query": "q"})).await,
        Err(ProviderError::Use(UseError::NotFound))
    );
    assert_eq!(
        s.media("carol", None, &carol_g, doc()).await,
        Err(ProviderError::Busy)
    );
    assert_eq!(f.seen("/v1/media/embeddings").len(), 4);
    drop(gate);
    for h in a.into_iter().chain([b, c]) {
        assert!(h.await.unwrap().is_ok());
    }
    // Slots are given back: everyone can go again.
    s.media("alice", None, &alice_g, json!({"query": "q"}))
        .await
        .unwrap();
    s.media("carol", None, &alice_g, json!({"query": "q"}))
        .await
        .unwrap();

    // A call cancelled mid-flight gives its slot back too.
    let gate = f.gate.write().await;
    let a = [call("alice", &alice_g), call("alice", &alice_g)];
    arrived(8).await;
    for h in &a {
        h.abort();
    }
    for h in a {
        let _ = h.await;
    }
    drop(gate);
    s.media("alice", None, &alice_g, json!({"query": "q"}))
        .await
        .unwrap();
}

// ---- Library index purges ----

/// Seed one library index row (and one segment) for `user` built with
/// `connection`, as library search stores them, through a second sqlite
/// connection to the server's database file (foreign keys are switched
/// off on that connection, so no file upload is needed for this check).
fn seed_index(s: &Server, user: &str, asset: &str, connection: &str) {
    let conn = s.sqlite();
    conn.execute(
        "INSERT INTO library_search_assets (user_id, asset_id, source, space_id,
             connection_id, kind, fingerprint, content_hash, model, dims, segments,
             partial, write_seq, indexed_at)
         VALUES (?1, ?2, 'personal', NULL, ?3, 'document', 'fp', 'h', 'm', 2, 1, 0, 1, 0)",
        rusqlite::params![user, asset, connection],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO library_search_segments (user_id, asset_id, seq, source, space_id,
             connection_id, kind, fingerprint, content_hash, write_seq, locator,
             excerpt, vector)
         VALUES (?1, ?2, 0, 'personal', NULL, ?3, 'document', 'fp', 'h', 1,
             '{\"kind\":\"text\"}', 'x', x'00000000')",
        rusqlite::params![user, asset, connection],
    )
    .unwrap();
}

/// `(user, asset)` index rows left, and the number of segments.
fn index_rows(s: &Server) -> (Vec<(String, String)>, i64) {
    let conn = s.sqlite();
    let mut stmt = conn
        .prepare("SELECT user_id, asset_id FROM library_search_assets ORDER BY user_id, asset_id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<(String, String)>, _>>()
        .unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM library_search_segments", [], |r| {
            r.get(0)
        })
        .unwrap();
    (rows, n)
}

fn rows(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(u, a)| ((*u).to_owned(), (*a).to_owned()))
        .collect()
}

#[tokio::test]
async fn library_model_compare_and_save_preserves_the_index_on_stale_writes() {
    let s = Server::start_on_file().await;
    let create = |name: &'static str| {
        let s = &s;
        async move {
            s.create(
                ALICE,
                json!({"name": name, "provider_id": "embeddinggemma-2", "base_url": s.fixture.base_url}),
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let (a, b) = (create("A").await, create("B").await);
    let path = "/v1/library/settings";
    let guarded_path = "/v1/library/settings/selection";
    let read = || s.send(reqwest::Method::GET, ALICE, path, None);
    let put = |body| s.send(reqwest::Method::PUT, ALICE, guarded_path, Some(body));
    let (status, initial) = read().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initial["selection_precondition"], true);
    assert_eq!(initial["embedding_id"], Value::Null);

    // A's view still has no model; a legacy window can save B as before.
    let (status, saved) = s
        .send(
            reqwest::Method::PUT,
            ALICE,
            path,
            Some(json!({"embedding_id": b})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["embedding_id"], b);
    seed_index(&s, "alice", "alice-index", &b);
    seed_index(&s, "bob", "bob-index", &b);
    let intact = (rows(&[("alice", "alice-index"), ("bob", "bob-index")]), 2);

    for body in [
        json!({"embedding_id": a, "expected_embedding_id": null}),
        json!({"embedding_id": null, "expected_embedding_id": a}),
        json!({"embedding_id": b, "expected_embedding_id": a}),
    ] {
        let (status, error) = put(body).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            error["error"],
            "the saved media model changed; reload it before saving"
        );
        assert_eq!(read().await.1["embedding_id"], b);
        assert_eq!(index_rows(&s), intact);
    }
    for body in [
        json!({"embedding_id": null}),
        json!({"expected_embedding_id": b}),
        json!({"embedding_id": null, "expected_embedding_id": 1}),
        json!({"embedding_id": null, "expected_embedding_id": b, "unknown": true}),
    ] {
        assert_eq!(put(body).await.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(index_rows(&s), intact);
    }
    assert_eq!(
        put(json!({"embedding_id": b, "expected_embedding_id": b}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(index_rows(&s), intact);

    let shared = s.space(ALICE, "Shared", &["bob"]).await;
    s.share(
        ALICE,
        &a,
        json!({"visibility": "spaces", "space_ids": [shared]}),
    )
    .await;
    assert_eq!(
        s.send(
            reqwest::Method::PUT,
            BOB,
            guarded_path,
            Some(json!({"embedding_id": a, "expected_embedding_id": null})),
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(index_rows(&s), intact);

    let (status, replaced) = put(json!({"embedding_id": a, "expected_embedding_id": b})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replaced["embedding_id"], a);
    let others = (rows(&[("bob", "bob-index")]), 1);
    assert_eq!(index_rows(&s), others);
    seed_index(&s, "alice", "alice-index", &a);
    let (status, cleared) = put(json!({"embedding_id": null, "expected_embedding_id": a})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["embedding_id"], Value::Null);
    assert_eq!(index_rows(&s), others);
    assert_eq!(
        put(json!({"embedding_id": b, "expected_embedding_id": null}))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn library_operations_reject_stale_selection_and_never_publish_after_aba() {
    let s = Server::start_on_file().await;
    let path = "/v1/library/settings";
    let put = |id: String| {
        s.send(
            reqwest::Method::PUT,
            ALICE,
            path,
            Some(json!({"embedding_id": id})),
        )
    };
    let read = || s.send(reqwest::Method::GET, ALICE, path, None);
    let index = |asset: String, rev: Value| {
        s.post(
            ALICE,
            "/v1/library/index/selection",
            json!({
                "asset_id": asset, "selection_revision": rev,
            }),
        )
    };
    let search = |rev: Value| {
        s.post(
            ALICE,
            "/v1/library/search/selection",
            json!({
                "query": "private query", "scope": "personal", "selection_revision": rev,
            }),
        )
    };
    let create = |name| {
        s.create(
            ALICE,
            json!({
                "name": name, "provider_id": "embeddinggemma-2", "base_url": s.fixture.base_url,
            }),
        )
    };
    let a = create("A").await["id"].as_str().unwrap().to_owned();
    let b = create("B").await["id"].as_str().unwrap().to_owned();
    let empty = read().await.1["selection_revision"].clone();
    let saved_a = put(a.clone()).await;
    assert_eq!(saved_a.0, StatusCode::OK);
    let saved_b = put(b.clone()).await;
    assert_eq!(saved_b.0, StatusCode::OK);
    let b_rev = saved_b.1["selection_revision"].clone();
    let uploaded = s
        .http
        .post(format!(
            "{}/v1/library/assets?scope=personal&name=private.pdf",
            s.base
        ))
        .bearer_auth(ALICE)
        .header("content-type", "application/pdf")
        .body("%PDF-1.7\nprivate PDF fixture")
        .send()
        .await
        .unwrap();
    let status = uploaded.status();
    let asset: Value = uploaded.json().await.unwrap();
    assert_eq!(status, StatusCode::CREATED, "{asset}");
    let id = asset["id"].as_str().unwrap().to_owned();
    let (status, answer) = index(id.clone(), b_rev.clone()).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let n = s.fixture.seen("/v1/media/embeddings").len();
    assert_eq!(n, 1);
    for stale in [empty, saved_a.1["selection_revision"].clone()] {
        for (status, error) in [index(id.clone(), stale.clone()).await, search(stale).await] {
            assert_eq!(status, StatusCode::CONFLICT, "{error}");
            assert_eq!(error["code"], "library_selection_changed");
        }
    }
    assert_eq!(
        s.fixture.seen("/v1/media/embeddings").len(),
        n,
        "no file or query reached B"
    );
    let (status, answer) = search(b_rev.clone()).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(s.fixture.seen("/v1/media/embeddings").len(), n + 1);

    let gate = s.fixture.gate.write().await;
    let late = index(id, b_rev);
    let switch = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            while s.fixture.seen("/v1/media/embeddings").len() < n + 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(put(a).await.0, StatusCode::OK);
        assert_eq!(put(b).await.0, StatusCode::OK);
        drop(gate);
    };
    let ((status, answer), ()) = tokio::join!(late, switch);
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["code"], "library_selection_changed");
    assert_eq!(index_rows(&s), (vec![], 0));
}

#[tokio::test]
async fn definitive_provider_changes_purge_library_vectors() {
    let s = Server::start_on_file().await;
    let f = &s.fixture;
    let create = |provider: &'static str, model: &'static str| {
        let s = &s;
        async move {
            s.create(
                ALICE,
                json!({"name": provider, "provider_id": provider, "base_url": f.base_url,
                       "model": model}),
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let gid = create("embeddinggemma-2", GEMMA).await;
    let tid = create("custom-embeddings", "emb-small").await;
    let s1 = s.space(ALICE, "one", &["bob", "carol"]).await;
    let s2 = s.space(ALICE, "two", &["bob"]).await;
    s.share(
        ALICE,
        &gid,
        json!({"visibility": "spaces", "space_ids": [s1, s2]}),
    )
    .await;
    for (user, asset) in [("alice", "a1"), ("bob", "b1"), ("carol", "c1")] {
        seed_index(&s, user, asset, &gid);
    }
    seed_index(&s, "bob", "t1", &tid);
    let patch = |id: &str, body: Value| {
        let (s, path) = (&s, format!("/v1/providers/{id}"));
        async move {
            let (status, b) = s
                .send(reqwest::Method::PATCH, ALICE, &path, Some(body))
                .await;
            assert_eq!(status, StatusCode::OK, "{b}");
        }
    };
    let all = rows(&[
        ("alice", "a1"),
        ("bob", "b1"),
        ("bob", "t1"),
        ("carol", "c1"),
    ]);

    // Not definitive: a share that widens or stays, a rename, a key, the
    // default flag. Nothing is purged.
    s.share(ALICE, &gid, json!({"visibility": "organization"}))
        .await;
    patch(
        &gid,
        json!({"name": "renamed", "api_key": MEDIA_KEY, "is_default": true}),
    )
    .await;
    patch(&gid, json!({"model": GEMMA})).await;
    assert_eq!(index_rows(&s), (all.clone(), 4));

    // Narrowed (organization to one Space): other people's vectors built
    // with it go; the owner's stay, and so do vectors of other connections.
    s.share(
        ALICE,
        &gid,
        json!({"visibility": "spaces", "space_ids": [s1]}),
    )
    .await;
    assert_eq!(index_rows(&s), (rows(&[("alice", "a1"), ("bob", "t1")]), 2));
    // Revoked from a Space they kept: again only non-owner rows.
    seed_index(&s, "carol", "c2", &gid);
    s.share(ALICE, &gid, json!({"visibility": "personal"}))
        .await;
    assert_eq!(index_rows(&s), (rows(&[("alice", "a1"), ("bob", "t1")]), 2));

    // A text-only connection's changes are not library business.
    patch(&tid, json!({"model": "emb-large"})).await;
    assert_eq!(index_rows(&s).0.len(), 2);

    // Another model: it computes something else; every vector goes.
    seed_index(&s, "bob", "b2", &gid);
    patch(&gid, json!({"model": "acme/other-media-embedder"})).await;
    assert_eq!(index_rows(&s), (rows(&[("bob", "t1")]), 1));

    // Deleted: its vectors go with it.
    seed_index(&s, "alice", "a3", &gid);
    let (status, _) = s
        .send(
            reqwest::Method::DELETE,
            ALICE,
            &format!("/v1/providers/{gid}"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(index_rows(&s), (rows(&[("bob", "t1")]), 1));
}

// ---- Through a device ----

mod device {
    use super::*;
    use metro_connector::Secret;
    use metro_connector::metro::{Claim, MetroClient};
    use metro_connector::profile::{Kind, Profile, ProfileConfig};
    use metro_connector::provider::Provider;

    /// Claim jobs until one arrives, run it with the real connector policy
    /// and adapter, and post its result.
    async fn serve_one(client: &MetroClient, provider: &Provider<'_>) -> String {
        for _ in 0..500 {
            match client.claim().await.expect("claim") {
                Claim::Job(job) => {
                    let deadline = std::time::Instant::now() + Duration::from_secs(60);
                    let body = metro_connector::run::handle_job(provider, &job, deadline).await;
                    client
                        .post_result(&job.id, &body, Duration::from_secs(20))
                        .await
                        .expect("result delivered");
                    return job.operation;
                }
                Claim::Idle => tokio::time::sleep(Duration::from_millis(10)).await,
                Claim::Invalid { reason, .. } => panic!("invalid job: {reason}"),
            }
        }
        panic!("no job arrived");
    }

    #[tokio::test]
    async fn device_media_goes_through_the_real_relay_and_connector() {
        let s = Server::start().await;
        let f = &s.fixture;
        let created = s
            .create(
                ALICE,
                json!({"name": "gemma here", "provider_id": "device-embeddinggemma-2"}),
            )
            .await;
        assert_eq!(created["model"], GEMMA);
        assert_eq!(created["media_inputs"], true);
        let id = created["id"].as_str().unwrap().to_owned();
        let (status, t) = s
            .post(
                ALICE,
                &format!("/v1/providers/{id}/device-token"),
                json!({}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{t}");
        let token = t["token"].as_str().unwrap().to_owned();

        // The person's local profile: their endpoint, their allowlist.
        let home = tempfile::tempdir().unwrap();
        let server_url =
            metro_connector::netpolicy::validate_base_url(&s.base, "server_url").unwrap();
        let profile = Profile::open(home.path(), "gemma").unwrap();
        let config = ProfileConfig {
            version: 1,
            kind: Kind::MediaEmbeddings,
            server_url: server_url.clone(),
            models: vec![GEMMA.to_owned()],
            base_url: Some(f.base_url.clone()),
            request_timeout_secs: 120,
        };
        profile.save(&config).unwrap();
        profile
            .save_device_token(&server_url, &Secret::new(&token))
            .unwrap();
        let provider = Provider {
            profile: &profile,
            config: &config,
        };
        let client = MetroClient::new(
            &server_url,
            profile.device_token_for(&server_url).unwrap(),
            Duration::from_secs(120),
        )
        .unwrap()
        .with_media_claims();
        assert_eq!(client.claim().await.unwrap(), Claim::Idle, "now online");

        // 3 MiB and exactly 8 MiB of binary: over the 4 MiB text bound
        // (as base64), within the media one, through the real connector.
        for size in [3 * 1024 * 1024, 8 * 1024 * 1024] {
            let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let state = s.state.clone();
            let (cid, input) = (id.clone(), asset_input(&bytes));
            let call = tokio::spawn(async move {
                providers::media_embed(&state, "alice", None, &cid, input).await
            });
            assert_eq!(serve_one(&client, &provider).await, "media_embed");
            assert!(call.await.unwrap().is_ok(), "{size} bytes");
            let last = f.seen("/v1/media/embeddings").pop().unwrap();
            let got = base64::engine::general_purpose::STANDARD
                .decode(last.body["asset"]["data_base64"].as_str().unwrap())
                .unwrap();
            assert!(got == bytes, "{size} bytes arrive intact");
        }

        // 6 MiB of binary with a PNG signature.
        let bytes = binary(6 * 1024 * 1024);
        let state = s.state.clone();
        let (cid, input) = (id.clone(), asset_input(&bytes));
        let call = tokio::spawn(async move {
            providers::media_embed(&state, "alice", None, &cid, input).await
        });
        assert_eq!(serve_one(&client, &provider).await, "media_embed");
        let out = call.await.unwrap().unwrap();
        assert_eq!(out["data"].as_array().unwrap().len(), 2);
        assert_eq!(
            out["data"][0]["locator"],
            json!({"kind": "page", "page": 1})
        );
        assert_eq!(out["partial"], true);
        let sent = f.seen("/v1/media/embeddings");
        assert_eq!(sent[2].body["model"], GEMMA);
        let got = base64::engine::general_purpose::STANDARD
            .decode(sent[2].body["asset"]["data_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(got, bytes, "the bytes reach the model service intact");

        // A query and text embeddings on the same device.
        let state = s.state.clone();
        let cid = id.clone();
        let call = tokio::spawn(async move {
            providers::media_embed(&state, "alice", None, &cid, json!({"query": "bike"})).await
        });
        serve_one(&client, &provider).await;
        assert_eq!(
            call.await.unwrap().unwrap()["data"][0]["locator"]["kind"],
            "query"
        );
        let state = s.state.clone();
        let cid = id.clone();
        let call = tokio::spawn(async move {
            providers::embed(&state, "alice", None, &cid, vec!["hello".into()]).await
        });
        assert_eq!(serve_one(&client, &provider).await, "embed");
        assert_eq!(call.await.unwrap().unwrap().len(), 1);

        // The connector refuses what the server would never send, even if a
        // job carried it: SVG, data URLs, paths, a model it didn't approve.
        let before = f.seen("/v1/media/embeddings").len();
        for bad in [
            json!({"model": GEMMA, "asset": {"name": "logo.svg", "mime_type": "image/svg+xml",
                   "data_base64": b64(b"<svg><image href='https://evil/x'/></svg>")}}),
            json!({"model": GEMMA, "asset": {"name": "a.png", "mime_type": "image/png",
                   "data_base64": "data:image/png;base64,iVBORw0KGgo="}}),
            json!({"model": GEMMA, "asset": {"name": "a.png", "mime_type": "image/png",
                   "path": "/etc/passwd"}}),
            json!({"model": "not-approved", "query": "q"}),
        ] {
            let relay = s.state.device.clone();
            let db = s.state.db.clone();
            let cid = id.clone();
            let call = tokio::spawn(async move {
                relay
                    .dispatch_as(
                        &db,
                        &metro_server::device_relay::Caller::personal("alice"),
                        &cid,
                        metro_server::device_relay::Operation::MediaEmbed,
                        bad,
                    )
                    .await
            });
            serve_one(&client, &provider).await;
            assert!(
                matches!(call.await.unwrap(), Err(DeviceError::Device(_))),
                "the device refused it"
            );
        }
        assert_eq!(f.seen("/v1/media/embeddings").len(), before, "nothing sent");

        // Over the media bound: refused before it is queued.
        let too_big = asset_input(&vec![1u8; 8 * 1024 * 1024 + 1]);
        assert!(matches!(
            s.media("alice", None, &id, too_big).await,
            Err(ProviderError::InvalidInput(_))
        ));

        // Someone else's device only through a share for that Space.
        let s1 = s.space(ALICE, "one", &["bob"]).await;
        assert_eq!(
            s.media("bob", Some(&s1), &id, json!({"query": "q"})).await,
            Err(ProviderError::Use(UseError::NotFound))
        );
        s.share(
            ALICE,
            &id,
            json!({"visibility": "spaces", "space_ids": [s1]}),
        )
        .await;
        let state = s.state.clone();
        let (cid, sid) = (id.clone(), s1.clone());
        let call = tokio::spawn(async move {
            providers::media_embed(&state, "bob", Some(&sid), &cid, json!({"query": "q"})).await
        });
        serve_one(&client, &provider).await;
        assert!(call.await.unwrap().is_ok());
        assert_eq!(
            s.media("bob", None, &id, json!({"query": "q"})).await,
            Err(ProviderError::Use(UseError::NotFound))
        );

        // A text-only device connection never takes media.
        let text = s
            .create(
                ALICE,
                json!({"name": "text dev", "provider_id": "device-embeddings", "model": "nomic"}),
            )
            .await;
        assert_eq!(
            s.media(
                "alice",
                None,
                text["id"].as_str().unwrap(),
                json!({"query": "q"})
            )
            .await,
            Err(ProviderError::Use(UseError::MediaUnsupported))
        );
        // Unpaired relay errors stay device errors.
        let other = s
            .create(
                ALICE,
                json!({"name": "unpaired", "provider_id": "device-embeddinggemma-2"}),
            )
            .await;
        assert_eq!(
            s.media(
                "alice",
                None,
                other["id"].as_str().unwrap(),
                json!({"query": "q"})
            )
            .await,
            Err(ProviderError::Device(DeviceError::NotPaired))
        );
    }
}
