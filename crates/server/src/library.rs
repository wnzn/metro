//! The documents and media library: files people upload from the desktop
//! app into their own personal library or into a Space they belong to.
//!
//! Access follows the same rule as everything else in a Space: only its
//! current members see or read its files, and a personal file is visible to
//! its uploader alone. There is no admin bypass and no public link. Every
//! read checks membership in the same SQL statement that returns the rows
//! or bytes, and every write checks it inside the transaction that changes
//! them, so a member who is removed loses access immediately. Missing,
//! foreign and no-longer-readable assets all answer the same 404.
//!
//! Bytes live in SQLite (`library_blobs`), next to their metadata
//! (`library_assets`), so access checks, quotas and deletion are
//! transactional and a backup of the database file is a backup of the
//! library. Nothing is written to the filesystem and no path is ever
//! exposed. Uploads are a closed list of formats checked by MIME type,
//! extension and signature together (see [`validate`]); nothing inside a
//! file is run, rendered or followed by the server.
//!
//! The wire contract is in `docs/library.md`.

mod store;
pub mod validate;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::rejection::QueryRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::AppState;
use crate::api::ApiError;
use crate::auth::{Human, valid_id};
use crate::db::Db;

pub use store::Delete;
pub use validate::AssetKind;

/// Largest file this prototype accepts, in bytes; the configured limit can
/// only be lower.
pub const HARD_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Largest per-person or per-Space quota that may be configured.
pub const HARD_MAX_QUOTA_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// Assets in one list or enumerate page, at most.
pub const MAX_PAGE: usize = 200;
pub const DEFAULT_PAGE: usize = 50;
/// Longest configurable upload deadline, in seconds.
pub const HARD_MAX_UPLOAD_SECS: u64 = 600;

/// Configured library bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryLimits {
    /// Largest file, in bytes (at most [`HARD_MAX_FILE_BYTES`]).
    pub max_file_bytes: u64,
    /// Bytes one person may have uploaded in total, personal and Spaces.
    pub user_quota_bytes: u64,
    /// Bytes stored in one Space's library, all uploaders together.
    pub space_quota_bytes: u64,
    /// Files one person may have uploaded in total.
    pub max_assets_per_user: u64,
    /// Uploads buffered at once across the server (each up to
    /// `max_file_bytes` in memory).
    pub max_concurrent_uploads: usize,
    /// Uploads one person may have in flight at once (at most
    /// `max_concurrent_uploads`), so nobody can hold every slot.
    pub max_uploads_per_user: usize,
    /// How long an upload may wait for a free slot before 503.
    pub slot_wait: Duration,
    /// Wall clock for receiving one upload's whole body, from when it got
    /// its slot.
    pub upload_timeout: Duration,
    /// Longest gap between two pieces of one upload's body.
    pub upload_idle_timeout: Duration,
}

impl Default for LibraryLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: HARD_MAX_FILE_BYTES,
            user_quota_bytes: 256 * 1024 * 1024,
            space_quota_bytes: 1024 * 1024 * 1024,
            max_assets_per_user: 2_000,
            max_concurrent_uploads: 4,
            max_uploads_per_user: 2,
            slot_wait: Duration::from_secs(10),
            upload_timeout: Duration::from_secs(120),
            upload_idle_timeout: Duration::from_secs(20),
        }
    }
}

impl LibraryLimits {
    /// Defaults overridden by `METRO_LIBRARY_MAX_FILE_BYTES`,
    /// `METRO_LIBRARY_USER_QUOTA_BYTES`, `METRO_LIBRARY_SPACE_QUOTA_BYTES`,
    /// `METRO_LIBRARY_MAX_FILES_PER_USER`,
    /// `METRO_LIBRARY_MAX_CONCURRENT_UPLOADS`,
    /// `METRO_LIBRARY_MAX_UPLOADS_PER_USER`,
    /// `METRO_LIBRARY_UPLOAD_TIMEOUT_SECS` and
    /// `METRO_LIBRARY_UPLOAD_IDLE_TIMEOUT_SECS`. Out-of-range values refuse
    /// to start rather than being clamped silently.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let d = Self::default();
        let num = |key: &str, default: u64, min: u64, max: u64| -> anyhow::Result<u64> {
            match get(key) {
                None => Ok(default),
                Some(v) if v.trim().is_empty() => Ok(default),
                Some(v) => {
                    let n: u64 = v
                        .trim()
                        .parse()
                        .map_err(|_| anyhow::anyhow!("{key} must be a whole number"))?;
                    anyhow::ensure!(
                        (min..=max).contains(&n),
                        "{key} must be between {min} and {max}"
                    );
                    Ok(n)
                }
            }
        };
        let max_file_bytes = num(
            "METRO_LIBRARY_MAX_FILE_BYTES",
            d.max_file_bytes,
            1,
            HARD_MAX_FILE_BYTES,
        )?;
        let max_concurrent_uploads = num(
            "METRO_LIBRARY_MAX_CONCURRENT_UPLOADS",
            d.max_concurrent_uploads as u64,
            1,
            64,
        )? as usize;
        let upload_timeout = num(
            "METRO_LIBRARY_UPLOAD_TIMEOUT_SECS",
            d.upload_timeout.as_secs(),
            1,
            HARD_MAX_UPLOAD_SECS,
        )?;
        let limits = Self {
            max_file_bytes,
            max_concurrent_uploads,
            max_uploads_per_user: num(
                "METRO_LIBRARY_MAX_UPLOADS_PER_USER",
                (d.max_uploads_per_user.min(max_concurrent_uploads)) as u64,
                1,
                max_concurrent_uploads as u64,
            )? as usize,
            slot_wait: d.slot_wait,
            upload_timeout: Duration::from_secs(upload_timeout),
            upload_idle_timeout: Duration::from_secs(num(
                "METRO_LIBRARY_UPLOAD_IDLE_TIMEOUT_SECS",
                d.upload_idle_timeout.as_secs().min(upload_timeout),
                1,
                upload_timeout,
            )?),
            user_quota_bytes: num(
                "METRO_LIBRARY_USER_QUOTA_BYTES",
                d.user_quota_bytes,
                max_file_bytes,
                HARD_MAX_QUOTA_BYTES,
            )?,
            space_quota_bytes: num(
                "METRO_LIBRARY_SPACE_QUOTA_BYTES",
                d.space_quota_bytes,
                max_file_bytes,
                HARD_MAX_QUOTA_BYTES,
            )?,
            max_assets_per_user: num(
                "METRO_LIBRARY_MAX_FILES_PER_USER",
                d.max_assets_per_user,
                1,
                1_000_000,
            )?,
        };
        Ok(limits)
    }
}

/// Library limits and the upload slots that bound buffered memory.
#[derive(Clone)]
pub struct Library {
    pub limits: Arc<LibraryLimits>,
    uploads: Arc<Semaphore>,
    /// Uploads in flight per person. Holds only people with an upload in
    /// flight right now, so it never has more entries than there are
    /// global slots plus waiters, and is empty when nothing is uploading.
    active: Arc<Mutex<HashMap<String, usize>>>,
}

/// One person's claim on an upload; released on drop (completion, error,
/// timeout or client disconnect alike).
struct UserSlot {
    active: Arc<Mutex<HashMap<String, usize>>>,
    user: String,
}

impl Drop for UserSlot {
    fn drop(&mut self) {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = active.get_mut(&self.user) {
            *n -= 1;
            if *n == 0 {
                active.remove(&self.user);
            }
        }
    }
}

impl Library {
    pub fn new(limits: LibraryLimits) -> Self {
        Self {
            uploads: Arc::new(Semaphore::new(limits.max_concurrent_uploads.max(1))),
            limits: Arc::new(limits),
            active: Arc::default(),
        }
    }

    /// Claim one of `user`'s upload slots, or `None` at their cap.
    fn claim(&self, user: &str) -> Option<UserSlot> {
        let mut active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        let n = active.entry(user.to_owned()).or_insert(0);
        if *n >= self.limits.max_uploads_per_user.max(1) {
            if *n == 0 {
                active.remove(user);
            }
            return None;
        }
        *n += 1;
        Some(UserSlot {
            active: self.active.clone(),
            user: user.to_owned(),
        })
    }

    /// People with an upload in flight, and free global upload slots.
    pub fn upload_load(&self) -> (usize, usize) {
        let active = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        (active.len(), self.uploads.available_permits())
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self::new(LibraryLimits::from_env()?))
    }
}

impl Default for Library {
    fn default() -> Self {
        Self::new(LibraryLimits::default())
    }
}

/// One library file's metadata, as its readers see it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Asset {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub kind: AssetKind,
    pub size_bytes: u64,
    /// `None` for the uploader's personal library.
    pub space_id: Option<String>,
    pub uploader_id: String,
    /// Lowercase hex SHA-256 of the bytes.
    pub sha256: String,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Insert order, increasing; for incremental indexing.
    #[serde(skip)]
    pub seq: i64,
}

/// Which assets to enumerate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The caller's personal library.
    Personal,
    /// One Space's library; the caller must be a member now.
    Space(String),
    /// Personal and every Space the caller is a member of now.
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub assets: Vec<Asset>,
    /// `seq` to continue after (or before, newest-first) when more may follow.
    pub next_after: Option<i64>,
}

/// Create the library tables. Called from `Db::init`, before any table
/// that references `library_assets`.
pub fn init(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    use anyhow::Context;
    store::init(conn).context("creating library tables")
}

// ---- Crate API (sync; run inside `Db::call`) ----

/// `id`'s metadata if `user` may read it right now.
pub fn visible(db: &Db, user: &str, id: &str) -> anyhow::Result<Option<Asset>> {
    store::visible(db, user, id)
}

/// `id`'s metadata and full bytes if `user` may read it right now, in one
/// statement.
pub fn bytes_for(db: &Db, user: &str, id: &str) -> anyhow::Result<Option<(Asset, Vec<u8>)>> {
    store::bytes_for(db, user, id)
}

/// Assets readable by `user` now with `seq > after_seq`, ascending, at
/// most `limit` (clamped to `1..=MAX_PAGE`). `None` when `scope` is a
/// Space `user` is not a member of now.
pub fn enumerate(
    db: &Db,
    user: &str,
    scope: &Scope,
    after_seq: i64,
    limit: usize,
) -> anyhow::Result<Option<Page>> {
    store::page(
        db,
        user,
        scope,
        Some(after_seq),
        true,
        limit.clamp(1, MAX_PAGE),
    )
}

/// Newest-first assets readable by `user` now whose name contains the
/// `LIKE` pattern `pattern` (escape `\`), limited to `kinds` when given;
/// names containing any of `excluded` never match. Takes a connection the
/// caller holds (inside `Db::call`), so one lock can cover several reads.
/// Metadata only: never reads bytes.
pub fn find_by_name(
    conn: &rusqlite::Connection,
    user: &str,
    scope: &Scope,
    pattern: &str,
    kinds: Option<&[AssetKind]>,
    excluded: &[&str],
    limit: usize,
) -> anyhow::Result<Vec<Asset>> {
    store::find_by_name(
        conn,
        user,
        scope,
        pattern,
        kinds,
        excluded,
        limit.clamp(1, MAX_PAGE),
    )
}

/// The subset of `ids` that `user` may read right now.
pub fn filter_visible(db: &Db, user: &str, ids: &[String]) -> anyhow::Result<HashSet<String>> {
    store::filter_visible(db, user, ids)
}

// ---- HTTP ----

/// `/v1/library/assets` and `/v1/library/assets/{id}[/content]`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/library/assets",
            get(list)
                .post(upload)
                .layer(DefaultBodyLimit::max(HARD_MAX_FILE_BYTES as usize + 1)),
        )
        .route("/v1/library/assets/{id}", get(metadata).delete(remove))
        .route("/v1/library/assets/{id}/content", get(content))
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum WireScope {
    Personal,
    Space,
    All,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    #[serde(default)]
    scope: Option<WireScope>,
    #[serde(default)]
    space_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadQuery {
    scope: WireScope,
    #[serde(default)]
    space_id: Option<String>,
    name: String,
}

fn not_found() -> ApiError {
    ApiError::not_found("asset not found")
}

fn space_not_found() -> ApiError {
    ApiError::not_found("space not found")
}

fn query<T>(q: Result<Query<T>, QueryRejection>) -> Result<T, ApiError> {
    q.map(|Query(q)| q)
        .map_err(|e| ApiError::bad_request(e.body_text()))
}

/// The scope a request names. `space_id` belongs to `scope=space` only.
fn scope_of(scope: WireScope, space_id: Option<String>) -> Result<Scope, ApiError> {
    match (scope, space_id) {
        (WireScope::Space, Some(sid)) if valid_id(&sid) => Ok(Scope::Space(sid)),
        (WireScope::Space, Some(_)) => Err(space_not_found()),
        (WireScope::Space, None) => Err(ApiError::bad_request("scope=space needs space_id")),
        (_, Some(_)) => Err(ApiError::bad_request("space_id is only for scope=space")),
        (WireScope::Personal, None) => Ok(Scope::Personal),
        (WireScope::All, None) => Ok(Scope::All),
    }
}

/// Asset ids are server-made UUIDs; anything else cannot exist.
fn plausible_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// `GET /v1/library/assets?scope=personal|space|all&space_id=&limit=&cursor=`
async fn list(
    human: Human,
    State(state): State<AppState>,
    q: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    let scope = scope_of(q.scope.unwrap_or(WireScope::All), q.space_id)?;
    let limit = q.limit.unwrap_or(DEFAULT_PAGE);
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_PAGE}"
        )));
    }
    let cursor = match q.cursor.as_deref() {
        None | Some("") => None,
        Some(c) => Some(
            c.parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| ApiError::bad_request("invalid cursor"))?,
        ),
    };
    let user = human.id.clone();
    let page = state
        .db
        .call(move |db| store::page(db, &user, &scope, cursor, false, limit))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(space_not_found)?;
    let limits = &state.library.limits;
    Ok(no_store(
        Json(json!({
            "assets": page.assets,
            "next_cursor": page.next_after.map(|s| s.to_string()),
            "max_file_bytes": limits.max_file_bytes,
        }))
        .into_response(),
    ))
}

/// `POST /v1/library/assets?scope=personal|space&space_id=&name=` with the
/// raw file as the body and its MIME type as `Content-Type`.
async fn upload(
    human: Human,
    State(state): State<AppState>,
    q: Result<Query<UploadQuery>, QueryRejection>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let q = query(q)?;
    let space = match scope_of(q.scope, q.space_id)? {
        Scope::Personal => None,
        Scope::Space(sid) => Some(sid),
        Scope::All => {
            return Err(ApiError::bad_request(
                "upload to scope=personal or scope=space",
            ));
        }
    };
    let name = validate::sanitize_name(&q.name).map_err(|e| ApiError::bad_request(e.0))?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Content-Type must be the file's MIME type",
            )
        })?
        .to_owned();
    // Outsiders learn nothing and send nothing more: membership is checked
    // before the body is read, and again when the row is written.
    if let Some(sid) = space.clone() {
        let user = human.id.clone();
        let member = state
            .db
            .call(move |db| db.membership(&sid, &user))
            .await
            .map_err(ApiError::internal)?;
        if member.is_none() {
            return Err(space_not_found());
        }
    }
    let limits = state.library.limits.clone();
    let too_large = || {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("files are limited to {} bytes", limits.max_file_bytes),
        )
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > limits.max_file_bytes) {
        return Err(too_large());
    }
    // Per-person first, so one person waiting cannot queue ahead of others
    // for more than their share; both are released when this returns or
    // the client goes away.
    let _mine = state.library.claim(&human.id).ok_or_else(|| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "you already have the most uploads in progress; wait for one to finish",
        )
    })?;
    let _slot = tokio::time::timeout(
        limits.slot_wait,
        state.library.uploads.clone().acquire_owned(),
    )
    .await
    .map_err(|_| ApiError::unavailable("too many uploads in progress; try again"))?
    .map_err(|_| ApiError::unavailable("uploads are unavailable"))?;
    let bytes = read_body(body, &limits).await.map_err(|e| match e {
        BodyError::TooLarge => too_large(),
        BodyError::TimedOut => ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "the upload was too slow and was stopped",
        ),
        BodyError::Failed => ApiError::bad_request("the upload was interrupted"),
    })?;
    let kind = validate::classify(&name, &content_type, &bytes)
        .map_err(|e| ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, e.0))?;
    let sha256 = hex(&Sha256::digest(&bytes));
    let id = uuid::Uuid::new_v4().to_string();
    let uploader = human.id.clone();
    let result = state
        .db
        .call(move |db| {
            Ok(store::insert(
                db,
                &store::NewAsset {
                    id: &id,
                    uploader: &uploader,
                    space: space.as_deref(),
                    name: &name,
                    kind,
                    sha256: &sha256,
                    bytes: &bytes,
                    created_at: crate::db::now_ms(),
                },
                &limits,
            ))
        })
        .await
        .map_err(ApiError::internal)?;
    match result {
        Ok(asset) => Ok(no_store((StatusCode::CREATED, Json(asset)).into_response())),
        Err(store::InsertError::NotFound) => Err(space_not_found()),
        Err(store::InsertError::Quota(m)) => Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, m)),
        Err(store::InsertError::Db(e)) => Err(ApiError::internal(e)),
    }
}

enum BodyError {
    TooLarge,
    TimedOut,
    Failed,
}

/// The whole body, at most `max_file_bytes`, within `upload_timeout` in
/// total and with no gap longer than `upload_idle_timeout`.
async fn read_body(body: Body, limits: &LibraryLimits) -> Result<Vec<u8>, BodyError> {
    use futures::StreamExt;
    let max = limits.max_file_bytes as usize;
    let deadline = tokio::time::Instant::now() + limits.upload_timeout;
    let mut stream = body.into_data_stream();
    let mut out = Vec::new();
    loop {
        let idle = tokio::time::Instant::now() + limits.upload_idle_timeout;
        let next = tokio::time::timeout_at(idle.min(deadline), stream.next())
            .await
            .map_err(|_| BodyError::TimedOut)?;
        match next {
            None => return Ok(out),
            // A broken or reset connection.
            Some(Err(_)) => return Err(BodyError::Failed),
            Some(Ok(chunk)) => {
                if out.len() + chunk.len() > max {
                    return Err(BodyError::TooLarge);
                }
                out.extend_from_slice(&chunk);
            }
        }
    }
}

/// `GET /v1/library/assets/{id}`
async fn metadata(
    human: Human,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    if !plausible_id(&id) {
        return Err(not_found());
    }
    let user = human.id;
    let asset = state
        .db
        .call(move |db| store::visible(db, &user, &id))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(not_found)?;
    Ok(no_store(Json(asset).into_response()))
}

/// `DELETE /v1/library/assets/{id}`
async fn remove(
    human: Human,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !plausible_id(&id) {
        return Err(not_found());
    }
    let user = human.id;
    match state
        .db
        .call(move |db| store::delete(db, &user, &id))
        .await
        .map_err(ApiError::internal)?
    {
        Delete::Deleted => Ok(StatusCode::NO_CONTENT),
        Delete::Forbidden => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "only the uploader or the Space owner can delete this file",
        )),
        Delete::NotFound => Err(not_found()),
    }
}

/// A single byte range from a `Range` header, or `None` to serve the whole
/// file (no header, several ranges or a syntax this server ignores).
#[derive(Debug, PartialEq, Eq)]
enum Wanted {
    Whole,
    Part { start: u64, end: u64 },
    Unsatisfiable,
}

fn wanted(header: Option<&HeaderValue>, size: u64) -> Wanted {
    let Some(spec) = header
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().strip_prefix("bytes="))
    else {
        return Wanted::Whole;
    };
    let spec = spec.trim();
    if spec.contains(',') {
        return Wanted::Whole;
    }
    let Some((a, b)) = spec.split_once('-') else {
        return Wanted::Whole;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    let (a, b) = (a.trim(), b.trim());
    match (digits(a), digits(b), b.is_empty()) {
        // bytes=-N: the last N bytes.
        (false, true, _) if a.is_empty() => match b.parse::<u64>() {
            Ok(0) => Wanted::Unsatisfiable,
            Ok(n) => Wanted::Part {
                start: size.saturating_sub(n),
                end: size - 1,
            },
            Err(_) => Wanted::Whole,
        },
        // bytes=A- or bytes=A-B.
        (true, digit_b, open) if digit_b || open => {
            let Ok(start) = a.parse::<u64>() else {
                return Wanted::Whole;
            };
            let end = if open {
                size.saturating_sub(1)
            } else {
                match b.parse::<u64>() {
                    Ok(e) if e >= start => e.min(size.saturating_sub(1)),
                    _ => return Wanted::Whole,
                }
            };
            if start >= size {
                Wanted::Unsatisfiable
            } else {
                Wanted::Part { start, end }
            }
        }
        _ => Wanted::Whole,
    }
}

/// `GET|HEAD /v1/library/assets/{id}/content`, with single-range support.
async fn content(
    human: Human,
    State(state): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !plausible_id(&id) {
        return Err(not_found());
    }
    let user = human.id;
    let (lookup_user, lookup_id) = (user.clone(), id.clone());
    // Access first: ranges are only interpreted for a readable asset.
    let asset = state
        .db
        .call(move |db| store::visible(db, &lookup_user, &lookup_id))
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(not_found)?;
    let size = asset.size_bytes;
    let range = wanted(headers.get(header::RANGE), size);
    if range == Wanted::Unsatisfiable {
        let mut r = (
            StatusCode::RANGE_NOT_SATISFIABLE,
            Json(json!({"error": "range not satisfiable"})),
        )
            .into_response();
        set(&mut r, header::CONTENT_RANGE, &format!("bytes */{size}"));
        return Ok(protect(r));
    }
    let head = method == Method::HEAD;
    let (status, asset, bytes, content_range) = match range {
        Wanted::Part { start, end } => {
            let len = end - start + 1;
            let fetched = if head {
                Some((asset, Vec::new()))
            } else {
                state
                    .db
                    .call(move |db| store::range_for(db, &user, &id, start, len))
                    .await
                    .map_err(ApiError::internal)?
            };
            let (asset, bytes) = fetched.ok_or_else(not_found)?;
            (
                StatusCode::PARTIAL_CONTENT,
                asset,
                (bytes, len),
                Some(format!("bytes {start}-{end}/{size}")),
            )
        }
        _ => {
            let fetched = if head {
                Some((asset, Vec::new()))
            } else {
                state
                    .db
                    .call(move |db| store::bytes_for(db, &user, &id))
                    .await
                    .map_err(ApiError::internal)?
            };
            let (asset, bytes) = fetched.ok_or_else(not_found)?;
            let len = asset.size_bytes;
            (StatusCode::OK, asset, (bytes, len), None)
        }
    };
    let (bytes, len) = bytes;
    let mut r = (
        status,
        if head {
            Body::empty()
        } else {
            Body::from(bytes)
        },
    )
        .into_response();
    set(&mut r, header::CONTENT_TYPE, asset.kind.content_type());
    set(&mut r, header::CONTENT_LENGTH, &len.to_string());
    set(&mut r, header::ACCEPT_RANGES, "bytes");
    set(
        &mut r,
        header::CONTENT_DISPOSITION,
        &content_disposition(&asset.name),
    );
    if let Some(cr) = content_range {
        set(&mut r, header::CONTENT_RANGE, &cr);
    }
    Ok(protect(r))
}

fn set(r: &mut Response, name: header::HeaderName, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        r.headers_mut().insert(name, v);
    }
}

fn no_store(mut r: Response) -> Response {
    set(&mut r, header::CACHE_CONTROL, "no-store");
    set(&mut r, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    r
}

/// Headers on every file response: never cached, never sniffed, never run
/// as a page, never embedded by another origin.
fn protect(r: Response) -> Response {
    let mut r = no_store(r);
    set(
        &mut r,
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; sandbox",
    );
    set(&mut r, header::REFERRER_POLICY, "no-referrer");
    set(
        &mut r,
        header::HeaderName::from_static("cross-origin-resource-policy"),
        "same-origin",
    );
    r
}

/// `attachment` with an ASCII fallback name and the exact UTF-8 name per
/// RFC 6266 / RFC 8187.
fn content_disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
