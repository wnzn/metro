//! Semantic search over what a person can already read: their own personal
//! Chat and the rooms and threads of Spaces they are a member of now.
//!
//! Retrieval embeds messages (`message` and `agent` events only) with the
//! caller's own `embeddings` connection and ranks them by exact cosine
//! similarity. A bounded shortlist is then reranked with real System One
//! calls on the caller's `system_one` connection: one `noul` question per
//! query and candidate pair, the same rubric for every pair, so the scores
//! are comparable. Without a System One connection, or when it fails,
//! results keep their retrieval order, say so in `notice`, and are not
//! labelled as reranked.
//!
//! Access is checked when candidates are read, again right before any text
//! goes to a model, and again on the final results. Each source is sent
//! only to a model call made for that source's own destination (the Space,
//! or no Space for personal Chat), so content of one Space never travels
//! with a call made for another Space or for Chat.
//!
//! The wire contract with the desktop app is in `search/API.md`.

mod engine;
mod rerank;
mod store;
mod upstream;
pub mod vector;

#[cfg(test)]
mod tests;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::AppState;
use crate::api::ApiError;
use crate::auth::{DEVICE_TOKEN_PREFIX, Human, RUNNER_TOKEN_PREFIX};

pub use engine::{Engine, ModelError, ModelFuture, Models, Routing};

/// Longest query, in characters.
pub const MAX_QUERY_CHARS: usize = 1_000;
/// Results one search may return.
pub const MAX_LIMIT: usize = 25;
pub const DEFAULT_LIMIT: usize = 10;
/// Candidates reranked by System One, at most.
pub const SHORTLIST: usize = 10;
/// Longest event text sent to the embedding model, in characters.
pub const MAX_EMBED_CHARS: usize = 4_000;
/// Longest result text, in characters.
pub const MAX_RESULT_CHARS: usize = 1_000;
/// Longest candidate text sent to System One, in characters.
pub const MAX_RERANK_CHARS: usize = 2_000;
/// Events read (and at most embedded) per indexing batch.
pub const INDEX_BATCH: usize = 32;
/// Most Spaces one request looks at (personal Chat is extra).
pub const MAX_SPACES_PER_REQUEST: usize = 20;
/// Vectors kept per person and source.
pub const MAX_VECTORS_PER_SOURCE: usize = 20_000;
/// Vectors scored per request, across sources.
pub const MAX_SCAN_PER_REQUEST: usize = 100_000;
/// Vectors read from SQLite at a time; scoring happens outside the lock.
pub const SCAN_PAGE: usize = 512;
/// Embedding calls one search may make (indexing and query together).
pub const SEARCH_EMBED_CALLS: usize = 24;
/// Embedding calls one `POST /v1/search/index` may make.
pub const INDEX_EMBED_CALLS: usize = 16;
/// Indexing batches one source gets in one search.
pub const SEARCH_BATCHES_PER_SOURCE: usize = 1;
/// Indexing batches one source gets in one `/index` call.
pub const INDEX_BATCHES_PER_SOURCE: usize = 8;
/// System One calls in flight at once for one search.
pub const RERANK_CONCURRENCY: usize = 4;
/// Wall clock for one embedding call.
pub const EMBED_TIMEOUT: Duration = Duration::from_secs(30);
/// Wall clock for one System One call.
pub const DECIDE_TIMEOUT: Duration = Duration::from_secs(20);
/// Search and index requests in flight at once, across the server.
pub const MAX_CONCURRENT: usize = 8;

/// Create the search tables. Called from `Db::init`.
pub fn init(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    use anyhow::Context;
    store::init(conn).context("creating search tables")
}

/// `POST /v1/search`, `POST /v1/search/index`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/search", post(search))
        .route("/v1/search/index", post(index))
}

/// Where searched text comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    /// The caller's own personal Chat.
    Chat,
    /// One Space, every room and thread in it.
    Space(String),
}

impl Source {
    /// The stored key: `chat` or `space:<id>`.
    pub fn key(&self) -> String {
        match self {
            Source::Chat => "chat".to_owned(),
            Source::Space(sid) => format!("space:{sid}"),
        }
    }

    /// The destination model calls for this source are made for.
    pub fn destination(&self) -> Option<&str> {
        match self {
            Source::Chat => None,
            Source::Space(sid) => Some(sid),
        }
    }

    pub fn space_id(&self) -> Option<String> {
        self.destination().map(str::to_owned)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Chat,
    Space,
    All,
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexBody {
    pub scope: Scope,
    #[serde(default)]
    pub space_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResult {
    pub space_id: Option<String>,
    /// A Space's room or thread; for personal Chat (`space_id` null) its
    /// Chat thread, or null for the main Chat.
    pub room_id: Option<String>,
    pub event_id: i64,
    pub actor: String,
    pub text: String,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Ranking {
    Classifier,
    Embeddings,
    Mixed,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub ranking: Ranking,
    pub partial: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IndexResponse {
    pub indexed: u64,
    pub skipped: u64,
    pub remaining: u64,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

/// Why a request could not be served at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    BadRequest(String),
    NotFound,
    NoEmbeddingModel,
    /// Every requested source failed upstream; the message is provider-safe.
    Upstream(String),
    Busy,
    Internal(String),
}

impl From<SearchError> for ApiError {
    fn from(e: SearchError) -> Self {
        match e {
            SearchError::BadRequest(m) => ApiError::new(StatusCode::BAD_REQUEST, m),
            SearchError::NotFound => ApiError::new(StatusCode::NOT_FOUND, "space not found"),
            SearchError::NoEmbeddingModel => ApiError::new(
                StatusCode::CONFLICT,
                "choose an embedding model in Settings to use search",
            ),
            SearchError::Upstream(m) => ApiError::new(StatusCode::BAD_GATEWAY, m),
            SearchError::Busy => ApiError::unavailable("search is busy; try again shortly"),
            SearchError::Internal(m) => ApiError::internal(anyhow::anyhow!(m)),
        }
    }
}

impl From<anyhow::Error> for SearchError {
    fn from(e: anyhow::Error) -> Self {
        SearchError::Internal(format!("{e:#}"))
    }
}

/// The trimmed query, or why it is refused.
pub fn check_query(query: &str) -> Result<String, SearchError> {
    let q = query.trim();
    if q.is_empty() {
        return Err(SearchError::BadRequest("query must not be empty".into()));
    }
    if q.chars().count() > MAX_QUERY_CHARS {
        return Err(SearchError::BadRequest(format!(
            "query is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    Ok(q.to_owned())
}

pub fn check_limit(limit: Option<usize>) -> Result<usize, SearchError> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(n),
        Some(_) => Err(SearchError::BadRequest(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        ))),
    }
}

/// Whether an event text may be indexed at all: not blank, and not
/// carrying a Metro credential someone pasted by mistake.
pub fn indexable(text: &str) -> bool {
    !text.trim().is_empty()
        && !text.contains(RUNNER_TOKEN_PREFIX)
        && !text.contains(DEVICE_TOKEN_PREFIX)
}

/// The text embedded (and hashed) for an event: at most
/// [`MAX_EMBED_CHARS`] characters.
pub fn embed_text(text: &str) -> String {
    clip(text.trim(), MAX_EMBED_CHARS, "")
}

/// `text` cut to `max` characters on a char boundary, `tail` appended if cut.
pub fn clip(text: &str, max: usize, tail: &str) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((at, _)) => format!("{}{tail}", &text[..at]),
    }
}

/// The sources a request covers, as `(sources, spaces left out by the
/// per-request bound)`. A Space the caller cannot see is `NotFound`.
pub async fn resolve_sources(
    db: &crate::db::Db,
    user: &str,
    scope: Scope,
    space_id: Option<String>,
) -> Result<(Vec<Source>, usize), SearchError> {
    match (scope, space_id) {
        (Scope::Space, None) => Err(SearchError::BadRequest(
            "scope \"space\" needs a space_id".into(),
        )),
        (Scope::Chat | Scope::All, Some(_)) => Err(SearchError::BadRequest(
            "space_id is only allowed with scope \"space\"".into(),
        )),
        (Scope::Chat, None) => Ok((vec![Source::Chat], 0)),
        (Scope::Space, Some(sid)) => {
            if !crate::auth::valid_id(&sid) {
                return Err(SearchError::NotFound);
            }
            let (u, s) = (user.to_owned(), sid.clone());
            let member = db.call(move |db| db.membership(&s, &u)).await?;
            match member {
                Some(_) => Ok((vec![Source::Space(sid)], 0)),
                None => Err(SearchError::NotFound),
            }
        }
        (Scope::All, None) => {
            let u = user.to_owned();
            let spaces = db.call(move |db| db.list_spaces(&u)).await?;
            let left_out = spaces.len().saturating_sub(MAX_SPACES_PER_REQUEST);
            let mut sources = vec![Source::Chat];
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

/// Semantic search of `sources` as `who`, for Metro in Chat
/// ([`crate::meta`]): the same engine, permits, bounds and access checks
/// as `POST /v1/search`.
pub(crate) async fn search_as(
    state: &AppState,
    who: &Human,
    query: &str,
    sources: &[Source],
    limit: usize,
) -> Result<SearchResponse, SearchError> {
    let _permit = permits()
        .try_acquire_owned()
        .map_err(|_| SearchError::Busy)?;
    let routing = upstream::routing(state, who).await?;
    let models = upstream::ProviderModels::new(state, who);
    Engine::new(&state.db, &who.id, &models, routing)
        .search(query, sources, 0, limit)
        .await
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
        .map_err(|_| SearchError::Busy)?;
    let routing = upstream::routing(&state, &who).await?;
    let models = upstream::ProviderModels::new(&state, &who);
    let engine = Engine::new(&state.db, &who.id, &models, routing);
    Ok(Json(
        engine.search(&query, &sources, left_out, limit).await?,
    ))
}

async fn index(
    State(state): State<AppState>,
    who: Human,
    body: Result<Json<IndexBody>, JsonRejection>,
) -> Result<Json<IndexResponse>, ApiError> {
    let Json(body) = body?;
    let (sources, left_out) =
        resolve_sources(&state.db, &who.id, body.scope, body.space_id).await?;
    let _permit = permits()
        .try_acquire_owned()
        .map_err(|_| SearchError::Busy)?;
    let routing = upstream::routing(&state, &who).await?;
    let models = upstream::ProviderModels::new(&state, &who);
    let engine = Engine::new(&state.db, &who.id, &models, routing);
    Ok(Json(engine.index(&sources, left_out).await?))
}
