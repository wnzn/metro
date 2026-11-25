//! Personal profiles over real HTTP with the real dev-token authenticator
//! and managed accounts: names, bios, avatars, who may read whose profile
//! (re-checked on every request), and names that can't pose as anyone.

use std::collections::HashMap;
use std::sync::Arc;

use metro_server::auth::{AuthFailure, AuthFuture, HumanAuth, UserIdentity};
use metro_server::db::Db;
use metro_server::{AppState, Config, Limits, router};
use reqwest::StatusCode;
use serde_json::{Value, json};

// Test-only placeholders, not real credentials.
const JM: &str = "jm-dev-token-0000000000000001";
const BOB: &str = "bob-dev-token-000000000000002";
const CAROL: &str = "carol-dev-token-0000000000003";

fn config() -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        db_path: ":memory:".into(),
        model: None,
        limits: Limits::default(),
        org_admins: vec!["jm".to_owned()],
    }
}

async fn dev_auth() -> Arc<dyn HumanAuth> {
    let users = format!(r#"{{"jm":"{JM}","bob":"{BOB}","carol":"{CAROL}"}}"#);
    Arc::new(
        metro_auth::Authenticator::from_lookup(move |name| {
            Ok((name == metro_auth::env::DEV_USERS).then(|| users.clone()))
        })
        .await
        .unwrap(),
    )
}

struct Server {
    base: String,
    http: reqwest::Client,
    /// The server's avatar re-encode permits.
    encodes: Arc<tokio::sync::Semaphore>,
}

impl Server {
    async fn start_with(auth: Arc<dyn HumanAuth>) -> Self {
        let state = AppState::new(config(), Db::open_in_memory().unwrap(), auth)
            .await
            .unwrap()
            .with_env_user_ids(vec!["jm".into(), "bob".into(), "carol".into()]);
        let encodes = state.avatar_encodes.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            base,
            http: reqwest::Client::new(),
            encodes,
        }
    }

    async fn start() -> Self {
        Self::start_with(dev_auth().await).await
    }

    fn req(&self, method: reqwest::Method, token: &str, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(token)
    }

    async fn json(
        &self,
        method: reqwest::Method,
        token: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut r = self.req(method, token, path);
        if let Some(body) = body {
            r = r.json(&body);
        }
        let r = r.send().await.unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value) {
        self.json(reqwest::Method::GET, token, path, None).await
    }

    async fn patch(&self, token: &str, body: Value) -> (StatusCode, Value) {
        self.json(reqwest::Method::PATCH, token, "/v1/me/profile", Some(body))
            .await
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
        self.json(reqwest::Method::POST, token, path, Some(body))
            .await
    }

    async fn put_avatar(&self, token: &str, ty: &str, bytes: Vec<u8>) -> (StatusCode, Value) {
        let r = self
            .req(reqwest::Method::PUT, token, "/v1/me/profile/avatar")
            .header("content-type", ty)
            .body(bytes)
            .send()
            .await
            .unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn avatar(
        &self,
        token: &str,
        path: &str,
    ) -> (StatusCode, reqwest::header::HeaderMap, Vec<u8>) {
        let r = self
            .req(reqwest::Method::GET, token, path)
            .send()
            .await
            .unwrap();
        let status = r.status();
        let headers = r.headers().clone();
        (status, headers, r.bytes().await.unwrap().to_vec())
    }

    async fn space(&self, owner: &str, name: &str) -> String {
        let (status, s) = self.post(owner, "/v1/spaces", json!({"name": name})).await;
        assert_eq!(status, StatusCode::CREATED);
        s["id"].as_str().unwrap().to_owned()
    }

    async fn add(&self, owner: &str, sid: &str, user: &str) {
        let (status, body) = self
            .post(
                owner,
                &format!("/v1/spaces/{sid}/members"),
                json!({"user_id": user}),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }

    async fn remove(&self, owner: &str, sid: &str, user: &str) {
        let (status, body) = self
            .json(
                reqwest::Method::DELETE,
                owner,
                &format!("/v1/spaces/{sid}/members"),
                Some(json!({"user_id": user})),
            )
            .await;
        assert!(status.is_success(), "{body}");
    }

    /// A managed account created by jm: `(id, token)`.
    async fn managed(&self, name: &str) -> (String, String) {
        let (status, body) = self
            .post(JM, "/v1/admin/users", json!({"name": name}))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        (
            body["id"].as_str().unwrap().to_owned(),
            body["token"].as_str().unwrap().to_owned(),
        )
    }

    async fn say(&self, token: &str, sid: &str, text: &str) -> Value {
        let (status, ev) = self
            .post(
                token,
                &format!("/v1/spaces/{sid}/rooms/lobby/messages"),
                json!({"text": text}),
            )
            .await;
        assert!(status.is_success(), "{ev}");
        ev
    }
}

fn image(format: image::ImageFormat, w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([200, 40, 90]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, format)
        .unwrap();
    out.into_inner()
}

fn png(w: u32, h: u32) -> Vec<u8> {
    image(image::ImageFormat::Png, w, h)
}

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    image(image::ImageFormat::Jpeg, w, h)
}

#[tokio::test]
async fn defaults_then_edit_then_reset() {
    let s = Server::start().await;
    let (status, p) = s.get(JM, "/v1/me/profile").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        p,
        json!({"id": "jm", "display_name": "jm", "bio": null, "avatar_version": null})
    );

    let (status, p) = s
        .patch(
            JM,
            json!({"display_name": "  JM Reyes ", "bio": "Ops lead.\r\nManila"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(p["display_name"], "JM Reyes");
    assert_eq!(p["bio"], "Ops lead.\nManila");
    // /v1/me and new events use it; the stable id never changes.
    let (_, me) = s.get(JM, "/v1/me").await;
    assert_eq!(
        me,
        json!({"id": "jm", "actor": "JM Reyes", "is_org_admin": true})
    );
    let sid = s.space(JM, "Ops").await;
    let ev = s.say(JM, &sid, "hello").await;
    assert_eq!(
        (ev["actor"].as_str(), ev["actor_id"].as_str()),
        (Some("JM Reyes"), Some("jm"))
    );

    // Absent fields are kept; markup is stored as text, never interpreted.
    let (_, p) = s.patch(JM, json!({"bio": "<b>hi</b>"})).await;
    assert_eq!(p["display_name"], "JM Reyes");
    assert_eq!(p["bio"], "<b>hi</b>");
    // Own id in another case is fine.
    assert_eq!(
        s.patch(JM, json!({"display_name": "JM"})).await.0,
        StatusCode::OK
    );

    // Reset: null (and an empty bio) go back to the defaults.
    let (_, p) = s.patch(JM, json!({"display_name": null, "bio": ""})).await;
    assert_eq!(
        p,
        json!({"id": "jm", "display_name": "jm", "bio": null, "avatar_version": null})
    );
    let (_, me) = s.get(JM, "/v1/me").await;
    assert_eq!(me["actor"], "jm");
    // Earlier events keep the name they were written under.
    let (_, events) = s
        .get(JM, &format!("/v1/spaces/{sid}/rooms/lobby/events"))
        .await;
    let first = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["text"] == "hello")
        .unwrap();
    assert_eq!(first["actor"], "JM Reyes");
    assert_eq!(first["actor_id"], "jm");
}

#[tokio::test]
async fn invalid_edits_are_refused() {
    let s = Server::start().await;
    for body in [
        json!({"display_name": ""}),
        json!({"display_name": "a\nb"}),
        json!({"display_name": "a\u{202E}b"}),
        json!({"display_name": "x".repeat(65)}),
        json!({"bio": "x".repeat(281)}),
        json!({"bio": "bell\u{0007}"}),
        json!({"bio": "rtl\u{202E}"}),
    ] {
        let (status, _) = s.patch(BOB, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }
    for body in [
        json!({"id": "jm"}),
        json!({"avatar_version": "x"}),
        json!({"display_name": 5}),
    ] {
        assert!(
            s.patch(BOB, body.clone()).await.0.is_client_error(),
            "{body}"
        );
    }
    // Nothing changed.
    let (_, p) = s.get(BOB, "/v1/me/profile").await;
    assert_eq!(p["display_name"], "bob");
    assert_eq!(
        s.get("nope", "/v1/me/profile").await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn nobody_can_pose_as_someone_else() {
    let s = Server::start().await;
    let (ana, ana_token) = s.managed("Ana").await;
    let (_, p) = s.patch(CAROL, json!({"display_name": "Caz"})).await;
    assert_eq!(p["display_name"], "Caz");

    for name in [
        "agent",
        "Agent",
        "runner",
        "SYSTEM",
        "admin",
        "jm",
        "JM",
        "ana",
        "Caz",
        "caz",
        &ana,
        "mu:anything",
        "user:bob",
    ] {
        let (status, body) = s.patch(BOB, json!({"display_name": name})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{name}: {body}");
    }
    // Bob may use his own id; Carol may not.
    assert_eq!(
        s.patch(BOB, json!({"display_name": "Bob"})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        s.patch(CAROL, json!({"display_name": "BOB"})).await.0,
        StatusCode::CONFLICT
    );
    // Admins can't hand out a name someone chose, either.
    let (status, _) = s.post(JM, "/v1/admin/users", json!({"name": "caz"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = s
        .json(
            reqwest::Method::PATCH,
            JM,
            &format!("/v1/admin/users/{ana}"),
            Some(json!({"name": "Caz"})),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // A managed user's own admin-assigned name stays theirs.
    let (status, p) = s.patch(&ana_token, json!({"display_name": "ANA"})).await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(
        p,
        json!({"id": ana, "display_name": "ANA", "bio": null, "avatar_version": null})
    );
}

#[tokio::test]
async fn managed_users_have_profiles_and_reset_to_the_admin_name() {
    let s = Server::start().await;
    let (id, token) = s.managed("Ana").await;
    let (_, p) = s.get(&token, "/v1/me/profile").await;
    assert_eq!(p["display_name"], "Ana");
    let (_, p) = s.patch(&token, json!({"display_name": "Ana R."})).await;
    assert_eq!(p["display_name"], "Ana R.");
    assert_eq!(s.get(&token, "/v1/me").await.1["actor"], "Ana R.");

    // An admin rename changes the account name; the override still wins
    // until the user resets it, and then the new admin name shows.
    let (status, _) = s
        .json(
            reqwest::Method::PATCH,
            JM,
            &format!("/v1/admin/users/{id}"),
            Some(json!({"name": "Ana Reyes"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(s.get(&token, "/v1/me").await.1["actor"], "Ana R.");
    let (_, p) = s.patch(&token, json!({"display_name": null})).await;
    assert_eq!(p["display_name"], "Ana Reyes");
    assert_eq!(s.get(&token, "/v1/me").await.1["actor"], "Ana Reyes");
    // The admin list keeps the account name and never shows a token.
    let (_, list) = s.get(JM, "/v1/admin/users").await;
    assert_eq!(list["users"][0]["name"], "Ana Reyes");
}

#[tokio::test]
async fn cards_are_listed_but_bios_follow_current_space_membership() {
    let s = Server::start().await;
    let (ana, ana_token) = s.managed("Ana").await;
    s.patch(BOB, json!({"display_name": "Bobby", "bio": "hi"}))
        .await;
    assert_eq!(
        s.put_avatar(BOB, "image/png", png(32, 32)).await.0,
        StatusCode::OK
    );

    // Never-signed-in and unknown ids do not get public cards.
    for target in [
        ana.as_str(),
        "nobody",
        "mu:00000000000000000000000000000000",
    ] {
        let path = format!("/v1/profiles/{}", urlencode(target));
        assert_eq!(s.get(JM, &path).await.0, StatusCode::NOT_FOUND, "{target}");
        assert_eq!(
            s.avatar(JM, &format!("{path}/avatar")).await.0,
            StatusCode::NOT_FOUND
        );
    }
    // Strangers and admins can see only the card, in the legacy shape.
    for who in [JM, CAROL, ana_token.as_str()] {
        let (status, p) = s.get(who, "/v1/profiles/bob").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(p["display_name"], "Bobby");
        assert!(p["bio"].is_null() && p["avatar_version"].is_string());
        assert_eq!(
            s.avatar(who, "/v1/profiles/bob/avatar").await.0,
            StatusCode::OK
        );
    }
    // Yourself: always.
    assert_eq!(s.get(BOB, "/v1/profiles/bob").await.0, StatusCode::OK);

    // Share a Space: visible, both ways, with the current profile.
    let sid = s.space(JM, "Ops").await;
    s.add(JM, &sid, "bob").await;
    s.add(JM, &sid, &ana).await;
    let (status, p) = s.get(JM, "/v1/profiles/bob").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p["display_name"], "Bobby");
    assert_eq!(p["bio"], "hi");
    assert!(p["avatar_version"].is_string());
    let (status, p) = s
        .get(BOB, &format!("/v1/profiles/{}", urlencode(&ana)))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        p,
        json!({"id": ana, "display_name": "Ana", "bio": null, "avatar_version": null})
    );
    // A member who never signed in still shows a name (their id).
    s.add(JM, &sid, "dora").await;
    assert_eq!(
        s.get(BOB, "/v1/profiles/dora").await.1["display_name"],
        "dora"
    );
    let (status, h, bytes) = s.avatar(&ana_token, "/v1/profiles/bob/avatar").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h["content-type"], "image/png");
    assert!(bytes.starts_with(b"\x89PNG"));
    assert!(s.get(CAROL, "/v1/profiles/bob").await.1["bio"].is_null());

    // Removal: the next request no longer exposes the bio, even to an admin.
    s.remove(JM, &sid, "bob").await;
    for who in [JM, ana_token.as_str()] {
        let (status, p) = s.get(who, "/v1/profiles/bob").await;
        assert_eq!(status, StatusCode::OK);
        assert!(p["bio"].is_null());
    }
    assert_eq!(
        s.avatar(&ana_token, "/v1/profiles/bob/avatar").await.0,
        StatusCode::OK
    );
    assert_eq!(
        s.get(BOB, &format!("/v1/profiles/{}", urlencode(&ana)))
            .await
            .0,
        StatusCode::OK
    );
    // A revoked token reads nothing at all.
    s.json(
        reqwest::Method::DELETE,
        JM,
        &format!("/v1/admin/users/{ana}/token"),
        None,
    )
    .await;
    assert_eq!(
        s.get(&ana_token, "/v1/profiles/jm").await.0,
        StatusCode::UNAUTHORIZED
    );
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn avatars_are_validated_reencoded_and_private() {
    let s = Server::start().await;
    assert_eq!(
        s.avatar(BOB, "/v1/me/profile/avatar").await.0,
        StatusCode::NOT_FOUND
    );

    // JPEG in, clean bounded PNG out.
    let (status, p) = s.put_avatar(BOB, "image/jpeg", jpeg(800, 400)).await;
    assert_eq!(status, StatusCode::OK, "{p}");
    let v1 = p["avatar_version"].as_str().unwrap().to_owned();
    let (status, h, bytes) = s.avatar(BOB, "/v1/me/profile/avatar").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h["content-type"], "image/png");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["cache-control"], "private, no-store");
    assert!(
        h["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).unwrap();
    assert_eq!((img.width(), img.height()), (256, 128));

    // A new upload gets a new version; trailing payloads don't survive.
    let mut tricky = png(40, 40);
    tricky.extend_from_slice(b"<svg onload=alert(1)>");
    let (status, p) = s.put_avatar(BOB, "image/png", tricky).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(p["avatar_version"].as_str().unwrap(), v1);
    let (_, _, bytes) = s.avatar(BOB, "/v1/me/profile/avatar").await;
    assert!(!bytes.windows(4).any(|w| w == b"<svg"));

    // Refused: SVG, GIF, wrong or missing type, mismatched bytes, garbage,
    // too large.
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg'><script/></svg>".to_vec();
    for (ty, bytes, want) in [
        (
            "image/svg+xml",
            svg.clone(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        ("image/png", svg, StatusCode::UNPROCESSABLE_ENTITY),
        (
            "image/gif",
            b"GIF89a".to_vec(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            "application/octet-stream",
            png(8, 8),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        ("image/jpeg", png(8, 8), StatusCode::UNSUPPORTED_MEDIA_TYPE),
        (
            "image/png",
            b"\x89PNG\r\n\x1a\nnot really".to_vec(),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "image/png",
            vec![0u8; 1024 * 1024 + 1],
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ] {
        let (status, body) = s.put_avatar(BOB, ty, bytes).await;
        assert_eq!(status, want, "{ty}: {body}");
    }
    // Oversized dimensions, even when small on the wire.
    let (status, body) = s.put_avatar(BOB, "image/png", png(5000, 1)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    // The last good avatar is still there.
    let (_, p) = s.get(BOB, "/v1/me/profile").await;
    assert!(p["avatar_version"].is_string());

    // Delete: 204, idempotent, gone.
    for _ in 0..2 {
        let r = s
            .req(reqwest::Method::DELETE, BOB, "/v1/me/profile/avatar")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
    }
    assert_eq!(
        s.avatar(BOB, "/v1/me/profile/avatar").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.get(BOB, "/v1/me/profile").await.1["avatar_version"],
        Value::Null
    );
    // Unauthenticated: nothing.
    let r = s
        .http
        .get(format!("{}/v1/profiles/bob/avatar", s.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

/// An OIDC stand-in whose display claim is set per token.
struct Oidc(HashMap<&'static str, UserIdentity>);

impl HumanAuth for Oidc {
    fn identity<'a>(&'a self, bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async move {
            self.0
                .get(bearer)
                .cloned()
                .ok_or_else(|| AuthFailure::Rejected("unknown".to_owned()))
        })
    }
    fn namespace(&self) -> &str {
        "oidc:https://idp.example"
    }
}

#[tokio::test]
async fn oidc_users_have_profiles_and_names_never_impersonate() {
    let user = |id: &str, actor: &str| UserIdentity {
        id: id.into(),
        actor: actor.into(),
    };
    let s = Server::start_with(Arc::new(Oidc(HashMap::from([
        ("t1", user("auth0|111", "Alice Doe")),
        ("t2", user("auth0|222", "agent")),
    ]))))
    .await;
    let (_, p) = s.get("t1", "/v1/me/profile").await;
    assert_eq!(p["display_name"], "Alice Doe");
    // A display claim that names a server actor falls back to the id.
    let (_, me) = s.get("t2", "/v1/me").await;
    assert_eq!(me["actor"], "auth0|222");
    // Another person's IdP name is taken.
    let (status, _) = s.patch("t2", json!({"display_name": "alice doe"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, p) = s.patch("t1", json!({"display_name": "Ali"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p["id"], "auth0|111");
    // Ids with `|` work in the path once they share a Space.
    let sid = s.space("t1", "Team").await;
    s.add("t1", &sid, "auth0|222").await;
    let (status, p) = s
        .get("t2", &format!("/v1/profiles/{}", urlencode("auth0|111")))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p["display_name"], "Ali");
}

#[tokio::test]
async fn busy_avatar_reencodes_are_refused_promptly() {
    let s = Server::start().await;
    assert_eq!(
        s.encodes.available_permits(),
        metro_server::profiles::MAX_AVATAR_ENCODES
    );
    // Every slot is taken: the next upload is refused at once, not queued.
    let held = s
        .encodes
        .clone()
        .acquire_many_owned(metro_server::profiles::MAX_AVATAR_ENCODES as u32)
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let r = s
        .req(reqwest::Method::PUT, BOB, "/v1/me/profile/avatar")
        .header("content-type", "image/png")
        .body(png(64, 64))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.headers()["retry-after"], "2");
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(
        s.get(BOB, "/v1/me/profile").await.1["avatar_version"],
        Value::Null
    );
    // Cheap refusals don't need a slot.
    let (status, _) = s.put_avatar(BOB, "image/svg+xml", b"<svg/>".to_vec()).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    drop(held);
    let (status, p) = s.put_avatar(BOB, "image/png", png(64, 64)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(p["avatar_version"].is_string());
    // Finished work gives its slot back.
    assert_eq!(
        s.encodes.available_permits(),
        metro_server::profiles::MAX_AVATAR_ENCODES
    );
}
