//! Web devices: a person's own machines that run their web searches.
//!
//! A *web device* is a person's desktop app (or `metro-connector web`) that
//! runs `web_search` and `web_fetch` jobs with that person's own providers,
//! keys and IP address. It belongs to exactly one person, is never a model
//! connection, and is never shared or borrowed: the relay
//! ([`crate::device_relay`], provider [`DEVICE_WEB`]) dispatches to it only
//! for its owner.
//!
//! Its credential is an ordinary device token (`metro_pc_…`, shown once,
//! stored only as a hash in `device_credentials`, rotated on reissue,
//! revocable). A token claims only jobs queued for its own `wd_…` id.
//!
//! Human API (bearer session; anything not one's own is 404, with no admin
//! bypass):
//!
//! - `POST /v1/web-devices` `{label}` → `201 {id, label, token}` (token once)
//! - `GET /v1/web-devices` → `{devices:[{id, label, created_at, online, last_seen_secs}]}`
//! - `POST /v1/web-devices/{id}/token` → `201 {token}` (rotate; pending jobs fail)
//! - `DELETE /v1/web-devices/{id}` → `204` (revoke and delete; pending jobs fail)
//! - `GET|PUT /v1/web-search/prefs` `{personal_chat}`: the person's opt-in to
//!   web tools in their own personal Chat (default off).
//!
//! Which vendors a person uses, their order and their keys stay on the
//! device; the server never learns them.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody, db};
use crate::auth::Human;
use crate::device_relay::{DEVICE_WEB, DeviceConnection};

/// Most web devices one person may have.
pub const MAX_WEB_DEVICES: usize = 8;
/// Longest device label, in characters.
pub const MAX_LABEL: usize = 64;
const ID_PREFIX: &str = "wd_";
const NO_DEVICE: &str = "no such web device";

/// Whether `id` has the shape of a web device id (`wd_` + 32 hex).
pub fn is_web_device_id(id: &str) -> bool {
    id.strip_prefix(ID_PREFIX).is_some_and(|rest| {
        rest.len() == 32
            && rest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn new_id() -> String {
    format!("{ID_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

/// Create the tables. Called from `Db::init`.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS web_devices (
             id          TEXT PRIMARY KEY,
             owner_id    TEXT NOT NULL,
             label       TEXT NOT NULL,
             created_at  INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS web_devices_owner ON web_devices (owner_id, created_at);
         CREATE TABLE IF NOT EXISTS web_search_prefs (
             user_id       TEXT PRIMARY KEY,
             personal_chat INTEGER NOT NULL DEFAULT 0,
             updated_at    INTEGER NOT NULL
         );",
    )
}

/// The web device `id` as the relay sees it. Sync, on a connection the
/// caller holds.
pub fn device_connection(conn: &Connection, id: &str) -> anyhow::Result<Option<DeviceConnection>> {
    if !is_web_device_id(id) {
        return Ok(None);
    }
    let owner: Option<String> = conn
        .query_row(
            "SELECT owner_id FROM web_devices WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(owner.map(|owner_id| DeviceConnection {
        id: id.to_owned(),
        owner_id,
        provider: DEVICE_WEB.to_owned(),
    }))
}

/// `(id, label, created_at ms)` of `owner`'s web devices, oldest first.
pub fn devices_of(conn: &Connection, owner: &str) -> rusqlite::Result<Vec<(String, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT id, label, created_at FROM web_devices WHERE owner_id = ?1
         ORDER BY created_at, id",
    )?;
    stmt.query_map([owner], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect()
}

/// Whether `user` turned on web tools in their own personal Chat.
pub fn personal_chat_enabled(conn: &Connection, user: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT personal_chat FROM web_search_prefs WHERE user_id = ?1",
            [user],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some_and(|v| v != 0))
}

fn set_personal_chat(conn: &Connection, user: &str, on: bool) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO web_search_prefs (user_id, personal_chat, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (user_id) DO UPDATE SET personal_chat = excluded.personal_chat,
                                             updated_at = excluded.updated_at",
        params![user, on as i64, crate::db::now_ms()],
    )?;
    Ok(())
}

/// Why no web device can take a search for a person right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The person has no web device at all.
    NoDevice,
    /// None of their web devices polled recently.
    Offline,
}

async fn device_ids(
    db: &crate::db::Db,
    user: &str,
) -> Result<Vec<String>, crate::device_relay::DeviceError> {
    let u = user.to_owned();
    db.call(move |db| {
        Ok(devices_of(&db.lock(), &u)?
            .into_iter()
            .map(|(id, _, _)| id)
            .collect())
    })
    .await
    .map_err(|e| {
        tracing::error!(%e, "reading web devices");
        crate::device_relay::DeviceError::Internal
    })
}

/// Whether `user` has at least one web device (online or not).
pub(crate) async fn has_device(
    state: &AppState,
    user: &str,
) -> Result<bool, crate::device_relay::DeviceError> {
    Ok(!device_ids(&state.db, user).await?.is_empty())
}

/// The web device of `user` that should take a search now: among their own
/// devices that polled recently, the most recently seen. Never another
/// person's device, the server or a runner.
pub(crate) async fn pick_device(
    db: &crate::db::Db,
    relay: &crate::device_relay::DeviceRelay,
    user: &str,
) -> Result<String, crate::device_relay::DeviceError> {
    use crate::device_relay::DeviceError;
    let ids = device_ids(db, user).await?;
    if ids.is_empty() {
        return Err(DeviceError::NotFound);
    }
    ids.into_iter()
        .filter_map(|id| {
            let s = relay.status(&id);
            s.online.then(|| (s.last_seen_secs.unwrap_or(u64::MAX), id))
        })
        .min_by_key(|(secs, _)| *secs)
        .map(|(_, id)| id)
        .ok_or(DeviceError::Offline)
}

// ---------------------------------------------------------------------------
// HTTP.

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/web-devices", get(list).post(create))
        .route("/v1/web-devices/{id}", delete(remove))
        .route("/v1/web-devices/{id}/token", post(rotate))
        .route("/v1/web-search/prefs", get(get_prefs).put(put_prefs))
}

fn check_label(label: &str) -> ApiResult<String> {
    let label = label.trim();
    if label.is_empty()
        || label.chars().count() > MAX_LABEL
        || label.chars().any(char::is_control)
        || label.chars().any(crate::admin_users::deceptive)
    {
        return Err(ApiError::bad_request(format!(
            "label must be 1..={MAX_LABEL} printable characters"
        )));
    }
    Ok(label.to_owned())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    label: String,
}

async fn create(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<CreateBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Json(body) = body?;
    let label = check_label(&body.label)?;
    let id = new_id();
    let (owner, i, l) = (who.id.clone(), id.clone(), label.clone());
    let created = db(&state, move |db| {
        let conn = db.lock();
        let tx = conn.unchecked_transaction()?;
        let n: i64 = tx.query_row(
            "SELECT COUNT(*) FROM web_devices WHERE owner_id = ?1",
            [&owner],
            |r| r.get(0),
        )?;
        if n as usize >= MAX_WEB_DEVICES {
            return Ok(None);
        }
        let now = crate::db::now_ms();
        tx.execute(
            "INSERT INTO web_devices (id, owner_id, label, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![i, owner, l, now],
        )?;
        tx.commit()?;
        Ok(Some(now))
    })
    .await?;
    let Some(created_at) = created else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "you already have {MAX_WEB_DEVICES} computers set up for web search; remove one first"
            ),
        ));
    };
    match state.device.issue_token(&state.db, &who.id, &id).await {
        Ok(token) => {
            tracing::info!(device = %id, "web device created");
            Ok((
                StatusCode::CREATED,
                Json(json!({
                    "id": id,
                    "label": label,
                    "created_at": crate::db::rfc3339_utc(created_at),
                    "token": token,
                })),
            ))
        }
        Err(e) => {
            let i = id.clone();
            let _ = db(&state, move |db| {
                db.lock()
                    .execute("DELETE FROM web_devices WHERE id = ?1", [&i])?;
                Ok(())
            })
            .await;
            Err(e.into())
        }
    }
}

async fn list(State(state): State<AppState>, who: Human) -> ApiResult<Json<Value>> {
    let owner = who.id.clone();
    let rows = db(&state, move |db| Ok(devices_of(&db.lock(), &owner)?)).await?;
    let devices: Vec<Value> = rows
        .into_iter()
        .map(|(id, label, created_at)| {
            let s = state.device.status(&id);
            json!({
                "id": id,
                "label": label,
                "created_at": crate::db::rfc3339_utc(created_at),
                "online": s.online,
                "last_seen_secs": s.last_seen_secs,
            })
        })
        .collect();
    Ok(Json(json!({ "devices": devices })))
}

/// `id` when it is one of `who`'s web devices; 404 otherwise.
async fn own(state: &AppState, who: &Human, id: &str) -> ApiResult<()> {
    if !is_web_device_id(id) {
        return Err(ApiError::not_found(NO_DEVICE));
    }
    let (owner, i) = (who.id.clone(), id.to_owned());
    let found = db(state, move |db| {
        Ok(device_connection(&db.lock(), &i)?.is_some_and(|d| d.owner_id == owner))
    })
    .await?;
    if found {
        Ok(())
    } else {
        Err(ApiError::not_found(NO_DEVICE))
    }
}

async fn rotate(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    own(&state, &who, &id).await?;
    let token = state.device.issue_token(&state.db, &who.id, &id).await?;
    tracing::info!(device = %id, "web device token rotated");
    Ok((StatusCode::CREATED, Json(json!({ "token": token }))))
}

async fn remove(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    own(&state, &who, &id).await?;
    // The token and the row go together; pending jobs then fail
    // (`CredentialChanged`).
    let removed = state
        .device
        .remove_web_device(&state.db, &who.id, &id)
        .await
        .map_err(ApiError::internal)?;
    if !removed {
        return Err(ApiError::not_found(NO_DEVICE));
    }
    tracing::info!(device = %id, "web device removed");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefsBody {
    personal_chat: bool,
}

async fn get_prefs(State(state): State<AppState>, who: Human) -> ApiResult<Json<Value>> {
    let u = who.id.clone();
    let on = db(&state, move |db| Ok(personal_chat_enabled(&db.lock(), &u)?)).await?;
    Ok(Json(json!({ "personal_chat": on })))
}

async fn put_prefs(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<PrefsBody>,
) -> ApiResult<Json<Value>> {
    let Json(body) = body?;
    let u = who.id.clone();
    let on = body.personal_chat;
    db(&state, move |db| Ok(set_personal_chat(&db.lock(), &u, on)?)).await?;
    Ok(Json(json!({ "personal_chat": on })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_relay::{DEVICE_PROVIDERS, Operation, serves};

    #[tokio::test]
    async fn device_selection_distinguishes_a_database_fault_from_no_device() {
        use crate::device_relay::{DeviceError, DeviceRelay};
        let db = crate::db::Db::open_in_memory().unwrap();
        let relay = DeviceRelay::new(
            Default::default(),
            std::sync::Arc::new(crate::providers::DeviceDirectory),
        );
        assert_eq!(
            pick_device(&db, &relay, "alice").await.unwrap_err(),
            DeviceError::NotFound
        );
        db.lock().execute("DROP TABLE web_devices", []).unwrap();
        assert_eq!(
            pick_device(&db, &relay, "alice").await.unwrap_err(),
            DeviceError::Internal
        );
    }

    #[test]
    fn web_devices_serve_only_web_operations_and_nothing_else_serves_them() {
        for op in [Operation::WebSearch, Operation::WebFetch] {
            assert!(serves(DEVICE_WEB, op));
            for p in DEVICE_PROVIDERS {
                assert!(!serves(p, op), "{p}");
            }
        }
        for op in [
            Operation::Models,
            Operation::Complete,
            Operation::Decide,
            Operation::Embed,
            Operation::MediaEmbed,
        ] {
            assert!(!serves(DEVICE_WEB, op));
        }
        assert!(!DEVICE_PROVIDERS.contains(&DEVICE_WEB));
    }

    #[tokio::test]
    async fn removal_drops_token_and_row_together_and_fails_pending_jobs() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let id = new_id();
        let i = id.clone();
        db.call(move |db| {
            db.lock().execute(
                "INSERT INTO web_devices (id, owner_id, label, created_at) VALUES (?1, 'alice', 'l', 0)",
                [&i],
            )?;
            set_personal_chat(&db.lock(), "alice", true)?;
            Ok(())
        })
        .await
        .unwrap();
        let relay = crate::device_relay::DeviceRelay::new(
            Default::default(),
            std::sync::Arc::new(crate::providers::DeviceDirectory),
        );
        relay.issue_token(&db, "alice", &id).await.unwrap();
        let device = crate::device_relay::Device {
            connection: id.clone(),
            owner: "alice".into(),
            generation: 1,
        };
        assert!(relay.claim(&device).is_none(), "online, nothing queued");
        let pending = {
            let (relay, db, id) = (relay.clone(), db.clone(), id.clone());
            tokio::spawn(async move {
                relay
                    .dispatch_web(
                        &db,
                        &crate::web_tools::WebGrant::personal_chat("alice"),
                        &id,
                        Operation::WebSearch,
                        json!({"v": 1}),
                        std::time::Duration::from_secs(5),
                    )
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // Not the owner: nothing changes.
        assert!(!relay.remove_web_device(&db, "bob", &id).await.unwrap());
        assert!(relay.remove_web_device(&db, "alice", &id).await.unwrap());
        assert_eq!(
            pending.await.unwrap().unwrap_err(),
            crate::device_relay::DeviceError::CredentialChanged
        );
        // The revoked generation is published before removal returns: a poll
        // authenticated just before (old generation) claims nothing and
        // doesn't bring the device back online, and a dispatch that read
        // the old generation can't queue under it.
        assert_eq!(relay.known_generation(&id), 2);
        assert!(relay.claim(&device).is_none());
        assert!(!relay.status(&id).online);
        let i = id.clone();
        let (rows, creds): (i64, i64) = db
            .call(move |db| {
                let conn = db.lock();
                Ok((
                    conn.query_row(
                        "SELECT COUNT(*) FROM web_devices WHERE id = ?1",
                        [&i],
                        |r| r.get(0),
                    )?,
                    conn.query_row(
                        "SELECT COUNT(*) FROM device_credentials WHERE connection_id = ?1",
                        [&i],
                        |r| r.get(0),
                    )?,
                ))
            })
            .await
            .unwrap();
        assert_eq!((rows, creds), (0, 0));
    }

    #[tokio::test]
    async fn a_late_older_publication_leaves_newer_work_alone() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let id = new_id();
        let i = id.clone();
        db.call(move |db| {
            db.lock().execute(
                "INSERT INTO web_devices (id, owner_id, label, created_at) VALUES (?1, 'alice', 'l', 0)",
                [&i],
            )?;
            set_personal_chat(&db.lock(), "alice", true)?;
            Ok(())
        })
        .await
        .unwrap();
        let relay = crate::device_relay::DeviceRelay::new(
            Default::default(),
            std::sync::Arc::new(crate::providers::DeviceDirectory),
        );
        relay.issue_token(&db, "alice", &id).await.unwrap();
        relay.issue_token(&db, "alice", &id).await.unwrap();
        assert_eq!(relay.known_generation(&id), 2);
        let device = crate::device_relay::Device {
            connection: id.clone(),
            owner: "alice".into(),
            generation: 2,
        };
        assert!(relay.claim(&device).is_none());
        let pending = {
            let (relay, db, id) = (relay.clone(), db.clone(), id.clone());
            tokio::spawn(async move {
                relay
                    .dispatch_web(
                        &db,
                        &crate::web_tools::WebGrant::personal_chat("alice"),
                        &id,
                        Operation::WebSearch,
                        json!({"v": 1}),
                        std::time::Duration::from_secs(5),
                    )
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // The revocation of generation 1 is published only now.
        relay.retire(&id, 1);
        assert_eq!(relay.known_generation(&id), 2);
        let job = relay
            .claim_checked(&db, &device)
            .await
            .expect("the newer job survived");
        assert_eq!(job.operation, Operation::WebSearch);
        relay
            .complete_job(&device, &job.id, Ok(json!({"v": 1, "attempts": []})))
            .unwrap();
        assert!(pending.await.unwrap().is_ok());
        // A newer publication invalidates older work and polls.
        relay.retire(&id, 3);
        assert!(relay.claim(&device).is_none());
        assert!(!relay.status(&id).online);
    }

    #[test]
    fn ids_have_one_shape() {
        assert!(is_web_device_id(&new_id()));
        for bad in [
            "wd_",
            "wd_0123",
            "wd_0123456789ABCDEF0123456789abcdef",
            "pc_0123456789abcdef0123456789abcdef",
            "wd_0123456789abcdef0123456789abcdef0",
        ] {
            assert!(!is_web_device_id(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn tokens_are_stored_as_hashes_and_never_borrowed() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let id = new_id();
        let i = id.clone();
        db.call(move |db| {
            db.lock().execute(
                "INSERT INTO web_devices (id, owner_id, label, created_at) VALUES (?1, 'alice', 'l', 0)",
                [&i],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let relay = crate::device_relay::DeviceRelay::new(
            Default::default(),
            std::sync::Arc::new(crate::providers::DeviceDirectory),
        );
        assert!(relay.issue_token(&db, "bob", &id).await.is_err());
        let token = relay.issue_token(&db, "alice", &id).await.unwrap();
        let (t, i) = (token.clone(), id.clone());
        let (plain, hashed): (i64, i64) = db
            .call(move |db| {
                let conn = db.lock();
                let plain = conn.query_row(
                    "SELECT COUNT(*) FROM device_credentials WHERE token_hash = ?1",
                    [t.as_bytes()],
                    |r| r.get(0),
                )?;
                let hashed = conn.query_row(
                    "SELECT COUNT(*) FROM device_credentials WHERE connection_id = ?1 AND token_hash = ?2",
                    rusqlite::params![i, crate::device_relay::device_token_hash(&t)],
                    |r| r.get(0),
                )?;
                Ok((plain, hashed))
            })
            .await
            .unwrap();
        assert_eq!((plain, hashed), (0, 1));
        // Nobody else can queue on it, as a borrower or otherwise.
        let caller = crate::device_relay::Caller::in_space("bob", "s1");
        let err = relay
            .dispatch_as(&db, &caller, &id, Operation::WebSearch, json!({"v": 1}))
            .await
            .unwrap_err();
        assert_eq!(err, crate::device_relay::DeviceError::NotFound);
        let err = relay
            .dispatch_web(
                &db,
                &crate::web_tools::WebGrant::personal_chat("bob"),
                &id,
                Operation::WebSearch,
                json!({"v": 1}),
                std::time::Duration::from_secs(1),
            )
            .await
            .unwrap_err();
        assert_eq!(err, crate::device_relay::DeviceError::NotFound);
        // The owner's own device, not yet polled: offline at once.
        let err = relay
            .dispatch_web(
                &db,
                &crate::web_tools::WebGrant::personal_chat("alice"),
                &id,
                Operation::WebSearch,
                json!({"v": 1}),
                std::time::Duration::from_secs(1),
            )
            .await
            .unwrap_err();
        assert_eq!(err, crate::device_relay::DeviceError::Offline);
    }
}
