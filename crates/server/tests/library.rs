//! Documents and media library over real HTTP: personal and Space scopes,
//! uniform 404s for outsiders and guessed ids, revocation, quotas and size
//! limits, single byte ranges, response headers and delete cleanup.

use std::collections::HashMap;
use std::sync::Arc;

use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::library::{self, Library, LibraryLimits, Scope};
use metro_server::{AppState, Config, Limits, router};
use reqwest::{Method, StatusCode};
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

async fn app_with(limits: Option<LibraryLimits>) -> App {
    let config = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        db_path: ":memory:".into(),
        model: None,
        limits: Limits::default(),
        org_admins: vec!["alice".to_owned()],
    };
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("metro.sqlite3");
    let mut state = AppState::new(config, Db::open(&db_path).unwrap(), Arc::new(FakeAuth))
        .await
        .unwrap();
    if let Some(limits) = limits {
        state.library = Library::new(limits);
    }
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
    /// Rows in `library_assets` and `library_blobs`, read by a separate connection.
    fn rows(&self) -> (i64, i64) {
        let conn = rusqlite::Connection::open(&self.db_path).unwrap();
        let count = |t: &str| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                .unwrap()
        };
        (count("library_assets"), count("library_blobs"))
    }

    /// A chunked upload as bob (no Content-Length); the response status.
    async fn chunked(&self, path: &str, mime: &str, chunks: &[&[u8]]) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let host = self.base.trim_start_matches("http://");
        let mut tcp = tokio::net::TcpStream::connect(host).await.unwrap();
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer bob\r\n\
             Content-Type: {mime}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        tcp.write_all(head.as_bytes()).await.unwrap();
        for c in chunks {
            // The server may answer and close before every chunk is sent.
            if tcp
                .write_all(format!("{:x}\r\n", c.len()).as_bytes())
                .await
                .is_err()
                || tcp.write_all(c).await.is_err()
                || tcp.write_all(b"\r\n").await.is_err()
            {
                break;
            }
        }
        let _ = tcp.write_all(b"0\r\n\r\n").await;
        let mut out = Vec::new();
        let _ = tcp.read_to_end(&mut out).await;
        let text = String::from_utf8_lossy(&out);
        text.split_whitespace().nth(1).unwrap().parse().unwrap()
    }

    async fn json(
        &self,
        method: Method,
        token: &str,
        path: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let r = self
            .http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn upload(
        &self,
        token: &str,
        query: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> (StatusCode, Value) {
        let r = self
            .http
            .post(format!("{}/v1/library/assets?{query}", self.base))
            .bearer_auth(token)
            .header("content-type", mime)
            .body(bytes)
            .send()
            .await
            .unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn content(
        &self,
        method: Method,
        token: Option<&str>,
        id: &str,
        range: Option<&str>,
    ) -> reqwest::Response {
        let mut req = self.http.request(
            method,
            format!("{}/v1/library/assets/{id}/content", self.base),
        );
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(r) = range {
            req = req.header("range", r);
        }
        req.send().await.unwrap()
    }

    async fn delete(&self, token: &str, id: &str) -> StatusCode {
        self.http
            .delete(format!("{}/v1/library/assets/{id}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status()
    }

    /// A Space owned by `owner` with `members` added.
    async fn space(&self, owner: &str, name: &str, members: &[&str]) -> String {
        let (s, v) = self
            .json(Method::POST, owner, "/v1/spaces", json!({"name": name}))
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let sid = v["id"].as_str().unwrap().to_owned();
        for m in members {
            let (s, v) = self
                .json(
                    Method::POST,
                    owner,
                    &format!("/v1/spaces/{sid}/members"),
                    json!({"user_id": m}),
                )
                .await;
            assert_eq!(s, StatusCode::CREATED, "{v}");
        }
        sid
    }

    async fn remove_member(&self, owner: &str, sid: &str, user: &str) {
        let (s, v) = self
            .json(
                Method::DELETE,
                owner,
                &format!("/v1/spaces/{sid}/members"),
                json!({"user_id": user}),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
    }

    async fn ids(&self, token: &str, query: &str) -> Vec<String> {
        let (s, v) = self
            .get(token, &format!("/v1/library/assets?{query}"))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v["assets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap().to_owned())
            .collect()
    }
}

fn txt(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

#[tokio::test]
async fn personal_assets_are_the_uploaders_alone() {
    let app = app_with(None).await;
    let (s, a) = app
        .upload(
            "bob",
            "scope=personal&name=notes.md",
            "text/markdown",
            txt("# Bob's notes"),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    let id = a["id"].as_str().unwrap().to_owned();
    assert_eq!(a["name"], "notes.md");
    assert_eq!(a["mime_type"], "text/markdown");
    assert_eq!(a["size_bytes"], 13);
    assert_eq!(a["space_id"], Value::Null);
    assert_eq!(a["uploader_id"], "bob");
    assert!(a.get("seq").is_none() && a.get("path").is_none());

    assert_eq!(app.ids("bob", "scope=personal").await, vec![id.clone()]);
    assert_eq!(app.ids("bob", "scope=all").await, vec![id.clone()]);
    // Alice is an org admin: still no bypass.
    assert!(app.ids("alice", "scope=all").await.is_empty());
    assert!(app.ids("alice", "scope=personal").await.is_empty());
    let (s, v) = app.get("alice", &format!("/v1/library/assets/{id}")).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("asset not found"))
    );
    for method in [Method::GET, Method::HEAD] {
        let r = app
            .content(method, Some("alice"), &id, Some("bytes=0-1"))
            .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }
    assert_eq!(app.delete("alice", &id).await, StatusCode::NOT_FOUND);

    // Owner reads, with protective headers.
    let r = app.content(Method::GET, Some("bob"), &id, None).await;
    assert_eq!(r.status(), StatusCode::OK);
    let h = r.headers();
    assert_eq!(h["content-type"], "text/markdown; charset=utf-8");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["cache-control"], "no-store");
    assert_eq!(h["accept-ranges"], "bytes");
    assert_eq!(h["content-security-policy"], "default-src 'none'; sandbox");
    assert_eq!(
        h["content-disposition"],
        "attachment; filename=\"notes.md\"; filename*=UTF-8''notes.md"
    );
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"# Bob's notes");

    // Unauthenticated is 401 everywhere, before anything else.
    let r = app.content(Method::GET, None, &id, Some("bytes=0-1")).await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = app
        .http
        .get(format!("{}/v1/library/assets", app.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = app
        .http
        .post(format!(
            "{}/v1/library/assets?scope=personal&name=a.txt",
            app.base
        ))
        .header("content-type", "text/plain")
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn space_assets_follow_current_membership() {
    let app = app_with(None).await;
    let sid = app.space("alice", "team", &["bob"]).await;
    let other = app.space("carol", "elsewhere", &[]).await;
    let q = format!("scope=space&space_id={sid}&name=brief.pdf");
    let (s, a) = app
        .upload("bob", &q, "application/pdf", txt("%PDF-1.7\n..."))
        .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    let id = a["id"].as_str().unwrap().to_owned();
    assert_eq!(a["space_id"], sid.as_str());

    // Members list and read; outsiders cannot list, upload, read or guess.
    assert_eq!(
        app.ids("alice", &format!("scope=space&space_id={sid}"))
            .await,
        vec![id.clone()]
    );
    assert_eq!(app.ids("alice", "scope=all").await, vec![id.clone()]);
    assert!(app.ids("carol", "scope=all").await.is_empty());
    let (s, _) = app
        .get(
            "carol",
            &format!("/v1/library/assets?scope=space&space_id={sid}"),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .get("carol", "/v1/library/assets?scope=space&space_id=nope")
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = app
        .upload(
            "carol",
            &format!("scope=space&space_id={sid}&name=x.txt"),
            "text/plain",
            txt("x"),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A member of another Space naming this asset under their own Space still gets 404.
    let (s, _) = app.get("carol", &format!("/v1/library/assets/{id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let r = app
        .content(Method::GET, Some("carol"), &id, Some("bytes=999-"))
        .await;
    assert_eq!(
        r.status(),
        StatusCode::NOT_FOUND,
        "range is not interpreted before access"
    );
    let (s, _) = app
        .get(
            "carol",
            &format!("/v1/library/assets?scope=space&space_id={other}"),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    // Guessed and malformed ids look the same as foreign ones.
    for guess in ["00000000-0000-4000-8000-000000000000", "../etc/passwd", "x"] {
        let (s, _) = app.get("bob", &format!("/v1/library/assets/{guess}")).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{guess}");
    }

    // Revocation: bob loses every surface at once; the asset stays for the Space.
    app.remove_member("alice", &sid, "bob").await;
    assert!(app.ids("bob", "scope=all").await.is_empty());
    let (s, _) = app.get("bob", &format!("/v1/library/assets/{id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    for method in [Method::GET, Method::HEAD] {
        let r = app.content(method, Some("bob"), &id, None).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }
    assert_eq!(
        app.delete("bob", &id).await,
        StatusCode::NOT_FOUND,
        "uploader no longer a member"
    );
    assert_eq!(app.ids("alice", "scope=all").await, vec![id.clone()]);

    // Crate API used by library search applies the same check.
    let db = app.state.db.clone();
    let (sid2, id2) = (sid.clone(), id.clone());
    let (bob_sees, alice_bytes, bob_page, alice_page, filtered) = db
        .call(move |db| {
            Ok((
                library::visible(db, "bob", &id2)?,
                library::bytes_for(db, "alice", &id2)?,
                library::enumerate(db, "bob", &Scope::Space(sid2.clone()), 0, 10)?,
                library::enumerate(db, "alice", &Scope::All, 0, 10)?,
                library::filter_visible(db, "bob", std::slice::from_ref(&id2))?,
            ))
        })
        .await
        .unwrap();
    assert!(bob_sees.is_none() && bob_page.is_none() && filtered.is_empty());
    assert_eq!(alice_bytes.unwrap().1, b"%PDF-1.7\n...");
    let alice_page = alice_page.unwrap();
    assert_eq!(alice_page.assets.len(), 1);
    assert_eq!(alice_page.assets[0].space_id.as_deref(), Some(sid.as_str()));
}

#[tokio::test]
async fn delete_rights_and_cleanup() {
    let app = app_with(None).await;
    assert_eq!(app.rows(), (0, 0));
    let sid = app.space("alice", "team", &["bob", "carol"]).await;
    let up = |who: &'static str, name: &'static str| {
        let q = format!("scope=space&space_id={sid}&name={name}");
        let app = &app;
        async move {
            let (s, a) = app.upload(who, &q, "text/plain", txt("hello")).await;
            assert_eq!(s, StatusCode::CREATED, "{a}");
            a["id"].as_str().unwrap().to_owned()
        }
    };
    let bobs = up("bob", "b.txt").await;
    let carols = up("carol", "c.txt").await;
    assert_eq!(app.rows(), (2, 2));
    // A fellow member who did not upload it may not delete it.
    assert_eq!(app.delete("carol", &bobs).await, StatusCode::FORBIDDEN);
    // The uploader may; the Space owner may delete anyone's.
    assert_eq!(app.delete("bob", &bobs).await, StatusCode::NO_CONTENT);
    assert_eq!(app.delete("alice", &carols).await, StatusCode::NO_CONTENT);
    for id in [&bobs, &carols] {
        assert_eq!(app.delete("alice", id).await, StatusCode::NOT_FOUND);
        let (s, _) = app.get("alice", &format!("/v1/library/assets/{id}")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let r = app.content(Method::GET, Some("alice"), id, None).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }
    assert_eq!(app.rows(), (0, 0), "metadata and bytes are both gone");
    assert!(app.ids("alice", "scope=all").await.is_empty());
    // Personal delete by its owner.
    let (_, a) = app
        .upload(
            "carol",
            "scope=personal&name=mine.txt",
            "text/plain",
            txt("x"),
        )
        .await;
    let mine = a["id"].as_str().unwrap();
    assert_eq!(app.delete("alice", mine).await, StatusCode::NOT_FOUND);
    assert_eq!(app.delete("carol", mine).await, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn validation_and_size_limits() {
    let limits = LibraryLimits {
        max_file_bytes: 1024,
        user_quota_bytes: 2048,
        space_quota_bytes: 1500,
        max_assets_per_user: 5,
        max_concurrent_uploads: 2,
        ..LibraryLimits::default()
    };
    let app = app_with(Some(limits)).await;
    let sid = app.space("alice", "team", &["bob"]).await;
    let png = |n: usize| {
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        b.resize(n, 0);
        b
    };
    // Mismatches and unsupported formats: 415; bad names and scopes: 400.
    let cases = [
        (
            "scope=personal&name=a.png",
            "image/jpeg",
            png(16),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "scope=personal&name=a.png",
            "image/png",
            txt("%PDF-1.7"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "scope=personal&name=a.html",
            "text/html",
            txt("<script>"),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "scope=personal&name=a.txt",
            "text/plain",
            vec![0xC3, 0x28],
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "scope=personal&name=..%2Fa.txt",
            "text/plain",
            txt("x"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "scope=personal&name=.env",
            "text/plain",
            txt("x"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "scope=all&name=a.txt",
            "text/plain",
            txt("x"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "scope=space&name=a.txt",
            "text/plain",
            txt("x"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "scope=personal&space_id=x&name=a.txt",
            "text/plain",
            txt("x"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "scope=personal&name=a.txt&url=http://x",
            "text/plain",
            txt("x"),
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (q, mime, bytes, want) in cases {
        let (s, v) = app.upload("bob", q, mime, bytes).await;
        assert_eq!(s, want, "{q} {mime}: {v}");
    }
    // Over the per-file limit: 413, whether declared or streamed.
    let (s, _) = app
        .upload("bob", "scope=personal&name=big.png", "image/png", png(1025))
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    // Chunked, with no Content-Length: refused once the stream passes the limit.
    let status = app
        .chunked(
            "/v1/library/assets?scope=personal&name=s.png",
            "image/png",
            &[&png(600), &[0; 600]],
        )
        .await;
    assert_eq!(status, 413);
    // The hard cap holds even above the human JSON limit.
    let (s, _) = app
        .upload("bob", "scope=personal&name=a.png", "image/png", png(1000))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    // Space quota (1500): a second 1000-byte file in the Space is refused.
    let q = format!("scope=space&space_id={sid}&name=s.png");
    let (s, _) = app.upload("alice", &q, "image/png", png(1000)).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, v) = app.upload("bob", &q, "image/png", png(600)).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert!(v["error"].as_str().unwrap().contains("Space"));
    // Personal quota (2048 per uploader): bob has 1000.
    let (s, _) = app
        .upload("bob", "scope=personal&name=b.png", "image/png", png(1000))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, v) = app
        .upload("bob", "scope=personal&name=c.png", "image/png", png(100))
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    // File count.
    for i in 0..4 {
        let (s, v) = app
            .upload(
                "carol",
                &format!("scope=personal&name={i}.txt"),
                "text/plain",
                txt("x"),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
    }
    let (s, _) = app
        .upload("carol", "scope=personal&name=5.txt", "text/plain", txt("x"))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = app
        .upload("carol", "scope=personal&name=6.txt", "text/plain", txt("x"))
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn default_limit_is_eight_mebibytes() {
    let app = app_with(None).await;
    let mut big = b"\x89PNG\r\n\x1a\n".to_vec();
    big.resize(8 * 1024 * 1024, 7);
    let (s, a) = app
        .upload(
            "bob",
            "scope=personal&name=big.png",
            "image/png",
            big.clone(),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    let r = app
        .content(Method::GET, Some("bob"), a["id"].as_str().unwrap(), None)
        .await;
    assert_eq!(r.bytes().await.unwrap().len(), big.len());
    big.push(0);
    let (s, _) = app
        .upload("bob", "scope=personal&name=big2.png", "image/png", big)
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn byte_ranges() {
    let app = app_with(None).await;
    let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    wav.extend((0u8..=99).collect::<Vec<_>>());
    let size = wav.len();
    let (s, a) = app
        .upload(
            "bob",
            "scope=personal&name=clip.wav",
            "audio/wav",
            wav.clone(),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    assert_eq!(a["mime_type"], "audio/wav");
    let id = a["id"].as_str().unwrap();

    let r = app
        .content(Method::GET, Some("bob"), id, Some("bytes=4-11"))
        .await;
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        r.headers()["content-range"],
        format!("bytes 4-11/{size}").as_str()
    );
    assert_eq!(r.headers()["content-length"], "8");
    assert_eq!(r.headers()["content-type"], "audio/wav");
    assert_eq!(r.bytes().await.unwrap().as_ref(), &wav[4..12]);

    let r = app
        .content(Method::GET, Some("bob"), id, Some("bytes=-5"))
        .await;
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.bytes().await.unwrap().as_ref(), &wav[size - 5..]);

    let r = app
        .content(Method::GET, Some("bob"), id, Some("bytes=100-"))
        .await;
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.bytes().await.unwrap().as_ref(), &wav[100..]);

    let r = app
        .content(
            Method::GET,
            Some("bob"),
            id,
            Some(&format!("bytes={size}-")),
        )
        .await;
    assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        r.headers()["content-range"],
        format!("bytes */{size}").as_str()
    );

    // Several ranges: the whole file.
    let r = app
        .content(Method::GET, Some("bob"), id, Some("bytes=0-1,4-5"))
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.bytes().await.unwrap().len(), size);

    let r = app.content(Method::HEAD, Some("bob"), id, None).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-length"], size.to_string().as_str());
    let r = app
        .content(Method::HEAD, Some("bob"), id, Some("bytes=0-9"))
        .await;
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.headers()["content-length"], "10");
}

#[tokio::test]
async fn pagination_and_incremental_enumeration() {
    let app = app_with(None).await;
    let mut uploaded = Vec::new();
    for i in 0..5 {
        let (s, a) = app
            .upload(
                "bob",
                &format!("scope=personal&name=n{i}.txt"),
                "text/plain",
                txt("x"),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED);
        uploaded.push(a["id"].as_str().unwrap().to_owned());
    }
    // Newest first, two per page.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let q = match &cursor {
            Some(c) => format!("/v1/library/assets?scope=personal&limit=2&cursor={c}"),
            None => "/v1/library/assets?scope=personal&limit=2".to_owned(),
        };
        let (s, v) = app.get("bob", &q).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        for a in v["assets"].as_array().unwrap() {
            seen.push(a["id"].as_str().unwrap().to_owned());
        }
        match v["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    let mut newest_first = uploaded.clone();
    newest_first.reverse();
    assert_eq!(seen, newest_first);
    let (s, _) = app.get("bob", "/v1/library/assets?limit=0").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = app.get("bob", "/v1/library/assets?limit=201").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = app.get("bob", "/v1/library/assets?cursor=abc").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Ascending enumeration after a seq, for incremental indexing.
    let pages = app
        .state
        .db
        .call(|db| {
            let first = library::enumerate(db, "bob", &Scope::Personal, 0, 3)?.unwrap();
            let rest =
                library::enumerate(db, "bob", &Scope::Personal, first.next_after.unwrap(), 3)?
                    .unwrap();
            Ok((first, rest))
        })
        .await
        .unwrap();
    let ids: Vec<_> = pages
        .0
        .assets
        .iter()
        .chain(pages.1.assets.iter())
        .map(|a| a.id.clone())
        .collect();
    assert_eq!(ids, uploaded);
    assert!(pages.1.next_after.is_none());
}

#[tokio::test]
async fn docx_and_media_round_trip() {
    let app = app_with(None).await;
    // A real Word package (deflated parts), made with Python's zipfile.
    let docx = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/library/min.docx"
    ))
    .unwrap();
    {
        let (s, v) = app
            .upload(
                "bob",
                "scope=personal&name=Spec%20%C3%BCber.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                docx,
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        assert_eq!(v["name"], "Spec über.docx");
        let r = app
            .content(Method::GET, Some("bob"), v["id"].as_str().unwrap(), None)
            .await;
        assert_eq!(
            r.headers()["content-disposition"],
            "attachment; filename=\"Spec _ber.docx\"; filename*=UTF-8''Spec%20%C3%BCber.docx"
        );
    }
    let mp4 = b"\0\0\0\x18ftypmp42\0\0\0\0mp42isom".to_vec();
    let (s, v) = app
        .upload("bob", "scope=personal&name=v.mp4", "video/mp4", mp4)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (s, v) = app
        .upload(
            "bob",
            "scope=personal&name=v.webm",
            "video/webm",
            b"\x1a\x45\xdf\xa3".to_vec(),
        )
        .await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{v}");
}

/// Seed one library search index row (and one segment) for `user` and
/// `asset`, with the column list the search owner's indexer writes
/// (from their handoff).
fn seed_index(app: &App, user: &str, asset: &str, space: Option<&str>) {
    let conn = rusqlite::Connection::open(&app.db_path).unwrap();
    let source = space.map_or("personal".to_owned(), |s| format!("space:{s}"));
    conn.execute(
        "INSERT INTO library_search_assets (user_id, asset_id, source, space_id, connection_id, kind,
             fingerprint, content_hash, model, dims, segments, partial, write_seq, indexed_at)
         VALUES (?1, ?2, ?3, ?4, 'pc', 'document', 'fp', 'h', 'm', 2, 1, 0, 1, 0)",
        rusqlite::params![user, asset, source, space],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO library_search_segments (user_id, asset_id, seq, source, space_id, connection_id,
             kind, fingerprint, content_hash, write_seq, locator, excerpt, vector)
         VALUES (?1, ?2, 0, ?3, ?4, 'pc', 'document', 'fp', 'h', 1, '{\"kind\":\"text\"}', 'x',
                 x'00000000')",
        rusqlite::params![user, asset, source, space],
    )
    .unwrap();
}

/// `(user, asset)` pairs with an index row, and the segment count.
fn index_rows(app: &App) -> (Vec<(String, String)>, i64) {
    let conn = rusqlite::Connection::open(&app.db_path).unwrap();
    let mut stmt = conn
        .prepare("SELECT user_id, asset_id FROM library_search_assets ORDER BY user_id, asset_id")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let segments = conn
        .query_row("SELECT COUNT(*) FROM library_search_segments", [], |r| {
            r.get(0)
        })
        .unwrap();
    (rows, segments)
}

#[tokio::test]
async fn removal_drops_only_the_removed_members_index_and_keeps_files() {
    let app = app_with(None).await;
    let a = app.space("alice", "team-a", &["bob", "carol"]).await;
    let b = app.space("alice", "team-b", &["bob"]).await;
    let up = |who: &'static str, q: String| {
        let app = &app;
        async move {
            let (s, v) = app.upload(who, &q, "text/plain", txt("shared")).await;
            assert_eq!(s, StatusCode::CREATED, "{v}");
            v["id"].as_str().unwrap().to_owned()
        }
    };
    let in_a = up("carol", format!("scope=space&space_id={a}&name=a.txt")).await;
    let bobs_in_a = up("bob", format!("scope=space&space_id={a}&name=mine.txt")).await;
    let in_b = up("bob", format!("scope=space&space_id={b}&name=b.txt")).await;
    let personal = up("bob", "scope=personal&name=p.txt".to_owned()).await;
    for (user, asset, space) in [
        ("bob", &in_a, Some(a.as_str())),
        ("bob", &bobs_in_a, Some(a.as_str())),
        ("bob", &in_b, Some(b.as_str())),
        ("bob", &personal, None),
        ("carol", &in_a, Some(a.as_str())),
        ("alice", &bobs_in_a, Some(a.as_str())),
    ] {
        seed_index(&app, user, asset, space);
    }
    let pair = |u: &str, id: &str| (u.to_owned(), id.to_owned());
    let sorted = |mut v: Vec<(String, String)>| {
        v.sort();
        v
    };

    // Refused removals change nothing: the owner, and someone not a member.
    let (s, _) = app
        .json(
            Method::DELETE,
            "alice",
            &format!("/v1/spaces/{a}/members"),
            json!({"user_id": "alice"}),
        )
        .await;
    assert!(s.is_client_error(), "{s}");
    let (s, _) = app
        .json(
            Method::DELETE,
            "alice",
            &format!("/v1/spaces/{a}/members"),
            json!({"user_id": "dave"}),
        )
        .await;
    assert!(s.is_client_error(), "{s}");
    assert_eq!(index_rows(&app).1, 6);

    app.remove_member("alice", &a, "bob").await;

    // Only bob's rows for Space A are gone; his Space B and personal index
    // and everyone else's rows (including for the file bob uploaded) stay.
    let (rows, segments) = index_rows(&app);
    assert_eq!(
        rows,
        sorted(vec![
            pair("alice", &bobs_in_a),
            pair("bob", &in_b),
            pair("bob", &personal),
            pair("carol", &in_a),
        ])
    );
    assert_eq!(segments, 4);
    // The files themselves all stay; bob's upload belongs to the Space.
    assert_eq!(app.rows(), (4, 4));
    for id in [&in_a, &bobs_in_a] {
        let (s, _) = app.get("bob", &format!("/v1/library/assets/{id}")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let r = app.content(Method::GET, Some("carol"), id, None).await;
        assert_eq!(r.status(), StatusCode::OK);
    }
    let r = app.content(Method::GET, Some("bob"), &in_b, None).await;
    assert_eq!(r.status(), StatusCode::OK);

    // Re-adding bob restores read access, not the dropped index.
    let (s, _) = app
        .json(
            Method::POST,
            "alice",
            &format!("/v1/spaces/{a}/members"),
            json!({"user_id": "bob"}),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = app.get("bob", &format!("/v1/library/assets/{in_a}")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(index_rows(&app).0.len(), 4);

    // Deleting a file drops every reader's index of it, and only of it.
    assert_eq!(
        app.delete("alice", &bobs_in_a).await,
        StatusCode::NO_CONTENT
    );
    let (rows, segments) = index_rows(&app);
    assert_eq!(
        rows,
        sorted(vec![
            pair("bob", &in_b),
            pair("bob", &personal),
            pair("carol", &in_a)
        ])
    );
    assert_eq!(segments, 3);
    assert_eq!(app.rows(), (3, 3));
}

// ---- Upload slots: deadlines, per-person cap, reclaiming ----

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A chunked personal PNG upload that sends its headers and first chunk,
/// then nothing more until the caller writes or drops the stream.
async fn held_upload(app: &App, token: &str) -> tokio::net::TcpStream {
    held_upload_to(app, token, "scope=personal&name=held.png").await
}

/// [`held_upload`] with its own query.
async fn held_upload_to(app: &App, token: &str, query: &str) -> tokio::net::TcpStream {
    use tokio::io::AsyncWriteExt;
    let host = app.base.trim_start_matches("http://").to_owned();
    let mut tcp = tokio::net::TcpStream::connect(&host).await.unwrap();
    let head = format!(
        "POST /v1/library/assets?{query} HTTP/1.1\r\nHost: {host}\r\n\
         Authorization: Bearer {token}\r\nContent-Type: image/png\r\n\
         Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
        PNG.len()
    );
    tcp.write_all(head.as_bytes()).await.unwrap();
    tcp.write_all(PNG).await.unwrap();
    tcp.write_all(b"\r\n").await.unwrap();
    tcp
}

/// The status line's code of whatever the server answered on `tcp`.
async fn status_of(mut tcp: tokio::net::TcpStream) -> u16 {
    use tokio::io::AsyncReadExt;
    let mut out = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), tcp.read_to_end(&mut out))
        .await
        .expect("the server answered in time")
        .unwrap();
    let text = String::from_utf8_lossy(&out);
    text.split_whitespace().nth(1).unwrap().parse().unwrap()
}

/// Wait until the library reports `(people uploading, free global slots)`.
async fn wait_load(app: &App, want: (usize, usize)) {
    for _ in 0..300 {
        if app.state.library.upload_load() == want {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!(
        "upload load stayed {:?}, wanted {want:?}",
        app.state.library.upload_load()
    );
}

fn slot_limits(global: usize) -> LibraryLimits {
    use std::time::Duration;
    LibraryLimits {
        max_concurrent_uploads: global,
        max_uploads_per_user: 1,
        slot_wait: Duration::from_millis(200),
        upload_timeout: Duration::from_millis(900),
        upload_idle_timeout: Duration::from_millis(300),
        ..LibraryLimits::default()
    }
}

#[tokio::test]
async fn stalled_upload_times_out_and_frees_its_slots() {
    let app = app_with(Some(slot_limits(2))).await;
    let held = held_upload(&app, "bob").await;
    wait_load(&app, (1, 1)).await;

    // Bob is at his own cap; Carol still has a slot.
    let (s, v) = app
        .upload(
            "bob",
            "scope=personal&name=b.png",
            "image/png",
            PNG.to_vec(),
        )
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{v}");
    let (s, v) = app
        .upload(
            "carol",
            "scope=personal&name=c.png",
            "image/png",
            PNG.to_vec(),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");

    // The stalled body is stopped by the idle deadline; both slots return.
    assert_eq!(status_of(held).await, 408);
    wait_load(&app, (0, 2)).await;
    let (s, v) = app
        .upload(
            "bob",
            "scope=personal&name=b.png",
            "image/png",
            PNG.to_vec(),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(app.state.library.upload_load(), (0, 2));
    // Nothing of the stopped upload was stored.
    assert_eq!(app.rows(), (2, 2));
}

#[tokio::test]
async fn trickled_upload_hits_the_total_deadline() {
    use tokio::io::AsyncWriteExt;
    let app = app_with(Some(slot_limits(2))).await;
    let mut held = held_upload(&app, "bob").await;
    wait_load(&app, (1, 1)).await;
    // One byte every 100 ms never trips the 300 ms idle deadline, but the
    // 900 ms total deadline still ends it.
    let started = std::time::Instant::now();
    let writer = tokio::spawn(async move {
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if held.write_all(b"1\r\n\0\r\n").await.is_err() {
                break;
            }
        }
        held
    });
    wait_load(&app, (0, 2)).await;
    let elapsed = started.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(3), "{elapsed:?}");
    assert_eq!(status_of(writer.await.unwrap()).await, 408);
    assert_eq!(app.rows(), (0, 0));
}

#[tokio::test]
async fn disconnect_and_slot_wait_release_the_global_slot() {
    let app = app_with(Some(slot_limits(1))).await;
    let held = held_upload(&app, "bob").await;
    wait_load(&app, (1, 0)).await;

    // The only global slot is held: Carol waits briefly, then 503.
    let (s, v) = app
        .upload(
            "carol",
            "scope=personal&name=c.png",
            "image/png",
            PNG.to_vec(),
        )
        .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    // Her wait left no entry behind.
    assert_eq!(app.state.library.upload_load(), (1, 0));

    // Bob's client goes away: the slot comes back well before any deadline.
    drop(held);
    wait_load(&app, (0, 1)).await;
    let (s, v) = app
        .upload(
            "carol",
            "scope=personal&name=c.png",
            "image/png",
            PNG.to_vec(),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(app.state.library.upload_load(), (0, 1));
}

#[tokio::test]
async fn removal_during_an_upload_stores_nothing() {
    use tokio::io::AsyncWriteExt;
    let app = app_with(None).await;
    let sid = app.space("alice", "team", &["bob"]).await;
    let mut held = held_upload_to(
        &app,
        "bob",
        &format!("scope=space&space_id={sid}&name=late.png"),
    )
    .await;
    // Bob passed the first membership check and holds a slot...
    wait_load(&app, (1, 3)).await;
    // ...then is removed before his body finishes.
    app.remove_member("alice", &sid, "bob").await;
    held.write_all(b"0\r\n\r\n").await.unwrap();
    assert_eq!(status_of(held).await, 404, "the write re-checks membership");
    assert_eq!(app.rows(), (0, 0));
    assert!(app.ids("alice", "scope=all").await.is_empty());
    assert_eq!(app.state.library.upload_load(), (0, 4));
}
