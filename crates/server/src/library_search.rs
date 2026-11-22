//! Semantic search over a person's files: documents, images, audio and
//! video uploaded to Metro, in their personal library or the library of a
//! Space they are a member of now.
//!
//! This is separate from message search (`crate::search`, `/v1/search`),
//! which it does not change. It has its own setting, a media-capable
//! embeddings connection the person chose explicitly
//! (`GET|PUT /v1/library/settings`), and it never falls back to another
//! connection.
//!
//! - **Nothing leaves without an explicit index.** Uploading, listing and
//!   searching never send a file anywhere. A file's bytes go to a model
//!   only from `POST /v1/library/index`, for that one file, on the person's
//!   own chosen connection, made for the file's own destination (no Space
//!   for the personal library, the file's Space otherwise).
//! - **Native content.** The media model embeds the file itself (pages,
//!   images, time ranges), never only its name.
//! - **Access.** The file is checked as readable when its bytes are read,
//!   right before they are sent, and again after the model answers and
//!   before anything is stored (in the same transaction as a membership
//!   check). At search time the index is read joined with current
//!   membership, and every result is checked against the library again
//!   right before any System One call and before it is returned. A file
//!   whose bytes changed, a model whose fingerprint or dimension changed, a
//!   Space the person left: their index rows are deleted, not used.
//! - **Honest ranking.** Results are ordered by cosine similarity of
//!   unit vectors. With a System One connection, real document text
//!   excerpts from the shortlist are reranked by one `noul` call each, and
//!   only among the positions documents already held: images, audio and
//!   video are never sent to System One and keep their embedding order.
//!   Every result says which score it carries.
//!
//! The wire contract with the desktop app is in `library_search/API.md`.
//! The library lane owns files; this module reaches them only through
//! [`Library`], implemented in `upstream.rs`.

mod engine;
pub mod protocol;
mod store;
mod upstream;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Semaphore;

use crate::AppState;
use crate::api::ApiError;
use crate::auth::Human;

pub use engine::Engine;
pub use protocol::{Kind, Locator, LocatorKind};

/// Largest file sent to a media model, in bytes (the protocol's bound).
pub const MAX_ASSET_BYTES: u64 = metro_model_protocol::MEDIA_MAX_ASSET_BYTES as u64;
/// Files one `POST /v1/library/index` may index.
pub const MAX_BULK: usize = 8;
/// Results one search may return.
pub const MAX_LIMIT: usize = 25;
pub const DEFAULT_LIMIT: usize = 10;
/// Results shown from one file, at most.
pub const MAX_PER_ASSET: usize = 3;
/// Document excerpts reranked by System One, at most.
pub const SHORTLIST: usize = 10;
/// Longest excerpt sent to System One, in characters.
pub const MAX_RERANK_CHARS: usize = 1_000;
/// Segments one person may hold across their whole library index.
pub const MAX_SEGMENTS_PER_USER: usize = 50_000;
/// Segments scored per search.
pub const MAX_SCAN_PER_REQUEST: usize = 100_000;
pub const SCAN_PAGE: usize = 512;
/// Most Spaces one search looks at (the personal library is extra).
pub const MAX_SPACES_PER_REQUEST: usize = 20;
/// System One calls in flight at once for one search.
pub const RERANK_CONCURRENCY: usize = 4;
/// Wall clock for embedding one file.
pub const INDEX_TIMEOUT: Duration = Duration::from_secs(180);
/// Wall clock for embedding one query.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Wall clock for one System One call.
pub const DECIDE_TIMEOUT: Duration = Duration::from_secs(20);
/// Library search and index requests in flight at once, across the server.
pub const MAX_CONCURRENT: usize = 4;

/// Create the library search tables. Called from `Db::init`, after
/// `library::init` (index rows reference `library_assets`).
pub fn init(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    use anyhow::Context;
    store::init(conn).context("creating library search tables")
}

/// `GET|PUT /v1/library/settings`, `POST /v1/library/index`,
/// `POST /v1/library/search`. Registered by the library owner.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/library/settings",
            get(upstream::get_settings).put(upstream::put_settings),
        )
        .route("/v1/library/index", post(index))
        .route("/v1/library/search", post(search))
}

/// Hooks for the library owner. Index rows reference `library_assets(id)`
/// with `ON DELETE CASCADE`, so deleting a file (or a Space, which cascades
/// to its files) already deletes them; these are for anything that must
/// drop rows without deleting the file, such as a membership removal.
/// Rows are also dropped lazily on the next request, so a missed hook never
/// exposes anything.
pub mod hooks {
    use rusqlite::Connection;

    /// A file was deleted (or replaced): drop everyone's index of it.
    pub fn asset_deleted(conn: &Connection, asset_id: &str) -> rusqlite::Result<()> {
        super::store::forget_asset(conn, asset_id).map(drop)
    }

    /// A Space was deleted: drop everyone's index of its files.
    pub fn space_deleted(conn: &Connection, space_id: &str) -> rusqlite::Result<()> {
        super::store::forget_space(conn, space_id).map(drop)
    }

    /// Someone left or was removed from a Space.
    pub fn member_removed(
        conn: &Connection,
        space_id: &str,
        user_id: &str,
    ) -> rusqlite::Result<()> {
        super::store::forget_member(conn, space_id, user_id).map(drop)
    }

    /// For the provider owner: connection `connection_id` was reconfigured
    /// (model, endpoint, key, adapter) or deleted. Drops every person's
    /// index built with it. The fingerprint check would drop them on next
    /// use anyway; this makes it immediate.
    pub fn connection_changed(conn: &Connection, connection_id: &str) -> rusqlite::Result<()> {
        super::store::forget_connection(conn, connection_id, None).map(drop)
    }

    /// For the provider owner: a share of `connection_id` (owned by
    /// `owner_id`) was revoked or narrowed. Drops every index other people
    /// built with it. It may drop more than strictly needed (someone who
    /// still has another share reindexes), never less.
    pub fn connection_access_changed(
        conn: &Connection,
        connection_id: &str,
        owner_id: &str,
    ) -> rusqlite::Result<()> {
        super::store::forget_connection(conn, connection_id, Some(owner_id)).map(drop)
    }
}

// ---- Seams ----

/// A file as the library describes it to one reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetInfo {
    pub id: String,
    /// The uploader's stable user id.
    pub owner_id: String,
    /// `None`: the owner's personal library. Otherwise the Space whose
    /// library holds it.
    pub space_id: Option<String>,
    pub name: String,
    pub mime_type: String,
    pub size: u64,
    /// Hex SHA-256 of the file's bytes.
    pub sha256: String,
}

pub type LibFuture<'a, T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>;

/// The library owner's files, as `user` may read them now. `None` for a
/// file that does not exist or that `user` may not read; the two are not
/// told apart.
pub trait Library: Send + Sync {
    fn visible<'a>(&'a self, user: &'a str, asset_id: &'a str) -> LibFuture<'a, Option<AssetInfo>>;
    fn bytes<'a>(
        &'a self,
        user: &'a str,
        asset_id: &'a str,
    ) -> LibFuture<'a, Option<(AssetInfo, Vec<u8>)>>;
}

/// A failed model or connection call. `message` is shown to people, so it
/// never holds key material. `gone` says the failure is definitive for the
/// index (the connection was deleted, its share revoked, it no longer
/// serves media embeddings): index rows built with it are deleted. A
/// transient failure (busy, offline, timeout, endpoint error) only skips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaError {
    pub message: String,
    pub gone: bool,
}

impl MediaError {
    pub fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            gone: false,
        }
    }

    pub fn gone(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            gone: true,
        }
    }
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub type ModelFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, MediaError>> + Send + 'a>>;

/// A synchronous grant check, run on the database connection: the key of
/// what connection `id` resolves to for the caller in `dest` right now
/// (row, provider, model, config revision, endpoint, grant), or why it may
/// not be used. Run before the bytes leave and again inside the
/// transaction that stores the result: a different key, or an error, means
/// nothing is stored.
pub type GrantCheck = std::sync::Arc<
    dyn Fn(&rusqlite::Connection, Option<&str>, &str) -> Result<String, MediaError> + Send + Sync,
>;

/// The caller's model calls. `dest` is the destination a call is made for:
/// `None` for the personal library, `Some(space_id)` for a Space.
pub trait MediaModels: Send + Sync {
    /// Whether the connection embeds media (documents, images, audio,
    /// video) natively for `dest`. An error means it may not be used there.
    fn media_capable<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
    ) -> ModelFuture<'a, bool>;
    /// Identifies what the connection computes for `dest`; it changes when
    /// the connection is reconfigured. An error means it may not be used
    /// for `dest`.
    fn fingerprint<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
    ) -> ModelFuture<'a, String>;
    /// One media embedding request (see [`protocol`]); returns the answer.
    fn embed<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
        input: Value,
    ) -> ModelFuture<'a, Value>;
    /// One System One request.
    fn decide<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
        state: Value,
        questions: Value,
    ) -> ModelFuture<'a, Value>;
    /// The caller's grant check (see [`GrantCheck`]).
    fn grant(&self) -> GrantCheck;
}

/// The caller's chosen connections.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Routing {
    /// The media embeddings connection from `/v1/library/settings`.
    pub embedding: Option<String>,
    /// The System One connection from `/v1/model-routing`.
    pub classifier: Option<String>,
}

// ---- Wire types ----

/// Where searched files come from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Personal,
    Space(String),
}

impl Source {
    pub fn key(&self) -> String {
        match self {
            Source::Personal => "personal".to_owned(),
            Source::Space(sid) => format!("space:{sid}"),
        }
    }

    pub fn destination(&self) -> Option<&str> {
        match self {
            Source::Personal => None,
            Source::Space(sid) => Some(sid),
        }
    }

    pub fn of(asset: &AssetInfo) -> Source {
        match &asset.space_id {
            None => Source::Personal,
            Some(sid) => Source::Space(sid.clone()),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Personal,
    Space,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub embedding_id: Option<String>,
}

/// `GET /v1/library/settings`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SettingsView {
    /// The chosen media embedding connection; `null` until the person
    /// chooses one. Never filled in automatically.
    pub embedding_id: Option<String>,
    /// The person's message-search embedding connection, offered only
    /// when it is confirmed media-capable. A suggestion to show; it is not
    /// used until saved with `PUT`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexBody {
    #[serde(default)]
    pub asset_id: Option<String>,
    #[serde(default)]
    pub asset_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Indexed,
    NotFound,
    Unsupported,
    TooLarge,
    Failed,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IndexItem {
    pub asset_id: String,
    pub status: ItemStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    pub indexed_segments: usize,
    pub partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IndexResponse {
    pub indexed_segments: usize,
    pub partial: bool,
    pub items: Vec<IndexItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchBody {
    pub query: String,
    pub scope: Scope,
    #[serde(default)]
    pub space_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub kind: Option<Kind>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Ranking {
    Classifier,
    Embeddings,
    Mixed,
}

/// Which number a result's `score` is.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScoreKind {
    /// A System One `noul` for the document excerpt.
    Classifier,
    /// Cosine similarity of the query and segment embeddings, clamped to 0..1.
    Cosine,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResult {
    pub asset_id: String,
    pub name: String,
    pub mime_type: String,
    pub kind: Kind,
    pub space_id: Option<String>,
    pub locator: Locator,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    pub score: f64,
    pub score_kind: ScoreKind,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub ranking: Ranking,
    pub partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

// ---- Errors ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibrarySearchError {
    BadRequest(String),
    /// A file or Space the caller cannot read (or that does not exist).
    NotFound(&'static str),
    NoMediaModel,
    /// The connection is not a media embeddings connection.
    NotMediaCapable,
    /// The connection may not be used for that destination.
    Unusable(String),
    Unsupported(String),
    TooLarge,
    /// The caller's library index is full.
    Full,
    Upstream(String),
    Busy,
    Internal(String),
}

impl std::fmt::Display for LibrarySearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl LibrarySearchError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::NoMediaModel | Self::Unusable(_) | Self::Full => StatusCode::CONFLICT,
            Self::NotMediaCapable => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Unsupported(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Busy => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::BadRequest(m) | Self::Unusable(m) | Self::Unsupported(m) | Self::Upstream(m) => {
                m.clone()
            }
            Self::NotFound(what) => format!("{what} not found"),
            Self::NoMediaModel => {
                "choose a media embedding model in Settings to search your files".into()
            }
            Self::NotMediaCapable => {
                "that connection cannot embed documents, images, audio or video".into()
            }
            Self::TooLarge => format!(
                "the file is larger than {} MiB, the most Metro sends to a media model",
                MAX_ASSET_BYTES / (1024 * 1024)
            ),
            Self::Full => format!(
                "your file index is full ({MAX_SEGMENTS_PER_USER} segments); remove files from it first"
            ),
            Self::Busy => "file search is busy; try again shortly".into(),
            Self::Internal(_) => "internal error".into(),
        }
    }
}

impl From<LibrarySearchError> for ApiError {
    fn from(e: LibrarySearchError) -> Self {
        match e {
            LibrarySearchError::Internal(m) => ApiError::internal(anyhow::anyhow!(m)),
            LibrarySearchError::Busy => ApiError::unavailable(e.message()),
            e => ApiError::new(e.status(), e.message()),
        }
    }
}

impl From<anyhow::Error> for LibrarySearchError {
    fn from(e: anyhow::Error) -> Self {
        LibrarySearchError::Internal(format!("{e:#}"))
    }
}

impl From<rusqlite::Error> for LibrarySearchError {
    fn from(e: rusqlite::Error) -> Self {
        LibrarySearchError::Internal(e.to_string())
    }
}

// ---- Request checks ----

pub fn check_query(query: &str) -> Result<String, LibrarySearchError> {
    crate::search::check_query(query).map_err(|_| {
        LibrarySearchError::BadRequest(format!(
            "query must be 1 to {} characters",
            crate::search::MAX_QUERY_CHARS
        ))
    })
}

pub fn check_limit(limit: Option<usize>) -> Result<usize, LibrarySearchError> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        Some(_) => Err(LibrarySearchError::BadRequest(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        ))),
    }
}

/// The asset ids of an index request: exactly one of `asset_id` and
/// `asset_ids` (1 to [`MAX_BULK`] distinct ids).
pub fn check_index_body(body: IndexBody) -> Result<(Vec<String>, bool), LibrarySearchError> {
    let ids = match (body.asset_id, body.asset_ids) {
        (Some(id), None) => return Ok((vec![id], false)),
        (None, Some(ids)) => ids,
        _ => {
            return Err(LibrarySearchError::BadRequest(
                "send exactly one of asset_id and asset_ids".into(),
            ));
        }
    };
    if ids.is_empty() || ids.len() > MAX_BULK {
        return Err(LibrarySearchError::BadRequest(format!(
            "asset_ids must hold 1 to {MAX_BULK} ids"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    if !ids.iter().all(|id| seen.insert(id.as_str())) {
        return Err(LibrarySearchError::BadRequest(
            "asset_ids must not repeat an id".into(),
        ));
    }
    Ok((ids, true))
}

/// The sources a search covers, as `(sources, Spaces left out by the
/// per-request bound)`. A Space the caller is not a member of is
/// `NotFound`; nothing is inferred from the scope or an asset id.
pub async fn resolve_sources(
    db: &crate::db::Db,
    user: &str,
    scope: Scope,
    space_id: Option<String>,
) -> Result<(Vec<Source>, usize), LibrarySearchError> {
    match (scope, space_id) {
        (Scope::Space, None) => Err(LibrarySearchError::BadRequest(
            "scope \"space\" needs a space_id".into(),
        )),
        (Scope::Personal | Scope::All, Some(_)) => Err(LibrarySearchError::BadRequest(
            "space_id is only allowed with scope \"space\"".into(),
        )),
        (Scope::Personal, None) => Ok((vec![Source::Personal], 0)),
        (Scope::Space, Some(sid)) => {
            if !crate::auth::valid_id(&sid) {
                return Err(LibrarySearchError::NotFound("space"));
            }
            let (u, s) = (user.to_owned(), sid.clone());
            match db.call(move |db| db.membership(&s, &u)).await? {
                Some(_) => Ok((vec![Source::Space(sid)], 0)),
                None => Err(LibrarySearchError::NotFound("space")),
            }
        }
        (Scope::All, None) => {
            let u = user.to_owned();
            let spaces = db.call(move |db| db.list_spaces(&u)).await?;
            let left_out = spaces.len().saturating_sub(MAX_SPACES_PER_REQUEST);
            let mut sources = vec![Source::Personal];
            sources.extend(
                spaces
                    .into_iter()
                    .take(MAX_SPACES_PER_REQUEST)
                    .map(|s| Source::Space(s.id)),
            );
            Ok((sources, left_out))
        }
    }
}

fn permits() -> Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT)))
        .clone()
}

// ---- Handlers ----

async fn index(
    State(state): State<AppState>,
    who: Human,
    body: Result<Json<IndexBody>, JsonRejection>,
) -> Result<Json<IndexResponse>, ApiError> {
    let Json(body) = body?;
    let (ids, bulk) = check_index_body(body)?;
    let _permit = permits()
        .try_acquire_owned()
        .map_err(|_| LibrarySearchError::Busy)?;
    let routing = upstream::routing(&state, &who).await?;
    let (library, models) = upstream::seams(&state, &who);
    let engine = Engine::new(&state.db, &who.id, &library, &models, routing);
    Ok(Json(engine.index(&ids, bulk).await?))
}

async fn search(
    State(state): State<AppState>,
    who: Human,
    body: Result<Json<SearchBody>, JsonRejection>,
) -> Result<Json<SearchResponse>, ApiError> {
    let Json(body) = body?;
    let query = check_query(&body.query)?;
    let limit = check_limit(body.limit)?;
    let (sources, left_out) =
        resolve_sources(&state.db, &who.id, body.scope, body.space_id).await?;
    let _permit = permits()
        .try_acquire_owned()
        .map_err(|_| LibrarySearchError::Busy)?;
    let routing = upstream::routing(&state, &who).await?;
    let (library, models) = upstream::seams(&state, &who);
    let engine = Engine::new(&state.db, &who.id, &library, &models, routing);
    Ok(Json(
        engine
            .search(&query, &sources, left_out, limit, body.kind)
            .await?,
    ))
}
