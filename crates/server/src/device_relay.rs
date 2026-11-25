//! Personal device connections: model inference relayed to the owner's own
//! `metro-connector`.
//!
//! A *device connection* is a personal provider connection whose provider is
//! one of [`DEVICE_PROVIDERS`]. The vendor sign-in, API key or CLI login stays
//! on the owner's machine. Metro stores only the connection's id (in the
//! providers table, owned by `crate::providers`) and, here, the SHA-256 digest
//! of one *device token* per connection: `metro_pc_` plus 256 random bits as
//! hex, issued to the connection's owner, shown once, rotated on reissue and
//! revocable.
//!
//! The connector holds the token and only makes outbound requests:
//!
//! - `POST /v1/device/claim` returns `204`, or one job
//!   `{"id","operation":"models"|"complete"|"decide"|"embed"|"media_embed","request":{..}}`;
//! - `POST /v1/device/jobs/{id}/result` takes `{"ok":true,"response":{..}}`
//!   or `{"ok":false,"error":".."}`.
//!
//! `complete` requests and responses are plain OpenAI Chat Completions JSON
//! (with `tool_calls`), without streaming. `models` takes `{}` and answers
//! `{"models":[..]}`. Tools are never run by the device: tool calls come
//! back to Metro's agent loop, which runs them on the Space runner as usual.
//!
//! Each connection serves one capability: chat providers run `complete`,
//! `device-systemone` runs `decide` (a System One request
//! `{model,state,questions}`) and `device-embeddings` runs `embed`
//! (`{model,input}`); `models` works for every kind.
//! `device-embeddinggemma-2` runs `embed` (text, like `device-embeddings`)
//! and is the only provider that runs `media_embed` (`{model, query}` or
//! `{model, asset}`, see `metro_model_protocol::check_media_request`). Any
//! other pairing is refused as if the connection didn't exist. Typed
//! requests and responses are checked in full with `metro-model-protocol`,
//! here and again on the device.
//!
//! **Sizes.** Text operations keep `max_request_bytes`. Only `media_embed`
//! may carry up to `max_media_request_bytes` (one 8 MiB asset as base64,
//! ~10.7 MiB of JSON), and all pending media jobs together hold at most
//! `max_pending_media_bytes`; no other operation can use that room. Within
//! that, one person (stable user id, across all their connections and
//! Spaces) holds at most `max_media_bytes_per_caller`, and one connection
//! at most `max_media_bytes_per_connection`, so one person or one device
//! can't take the whole budget from everyone else. A job holds its bytes
//! from queueing until it ends in any way (answered, failed, timed out,
//! withdrawn because its caller went away, cancelled by a revoked share or
//! a token change). Sizes are counted while serializing into nothing, and
//! budgets are checked before the job is queued. Media
//! payloads, like every payload, are never logged.
//!
//! **Borrowed use.** The owner may share a connection (the providers lane
//! stores the share; [`ConnectionDirectory::shared_use_allowed`] reads it).
//! [`DeviceRelay::dispatch_as`] then queues work for another [`Caller`] on
//! the owner's one device, keeping the caller and Space with the job. The
//! grant is checked when the job is queued, again right before a device may
//! claim it, and again before its result is handed back; and
//! [`DeviceRelay::shares_changed`] fails borrowed jobs whose grant is gone.
//! A borrowed job never moves to another machine or credential, the
//! account-bound subscription sign-in (`device-openai-oauth`) is never
//! borrowed, and device tokens stay the owner's alone.
//!
//! Every job is bound to one connection, its owner and the token
//! *generation* it was queued under. Rotating or revoking the token, or
//! deleting the connection, fails every pending job of that connection. A
//! job is handed to a device once and never re-queued, so a retried result
//! never re-runs anything: a duplicate from the same device is acknowledged,
//! and anything else is refused. Nothing ever falls back to another user's
//! connection or another machine.
//!
//! Request and response payloads live in memory only, for as long as their
//! job, and are never logged. Neither are tokens.
//!
//! [`DeviceRelay::rig_model`] is the typed adapter for agent runs: Rig's own
//! OpenAI Chat wire on a [`DeviceTransport`], so Rig encodes the request and
//! parses the reply (tool calls included) exactly as it does over HTTP.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use rig_core::ProviderError;
use rig_core::driver::{Exchange, Opened, Opening, Transport};
use rig_core::providers::openai::OpenAIConfig;
use rig_core::providers::openai::wire::{Chat, Dialect};
use rig_core::wire::{Body, Encoded, Framing, Mode, WireFrame};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::AppState;
use crate::api::ApiError;
use crate::auth::Human;
use crate::db::Db;

pub use crate::auth::DEVICE_TOKEN_PREFIX;

/// The provider ids whose inference runs on the owner's device.
pub const DEVICE_PROVIDERS: [&str; 6] = [
    "device-openai-oauth",
    "device-claude-code",
    "device-openai-compatible",
    DEVICE_SYSTEMONE,
    DEVICE_EMBEDDINGS,
    DEVICE_EMBEDDINGGEMMA,
];

/// System One on the owner's device (`decide`).
pub const DEVICE_SYSTEMONE: &str = "device-systemone";
/// Embeddings on the owner's device (`embed`).
pub const DEVICE_EMBEDDINGS: &str = "device-embeddings";
/// EmbeddingGemma 2 (or a compatible media embedder) on the owner's device:
/// text `embed` and `media_embed`.
pub const DEVICE_EMBEDDINGGEMMA: &str = "device-embeddinggemma-2";
/// The account-bound subscription sign-in: never used by anyone but its owner.
const SUBSCRIPTION_PROVIDER: &str = "device-openai-oauth";

/// Whether `provider` is a device provider.
pub fn is_device_provider(provider: &str) -> bool {
    DEVICE_PROVIDERS.contains(&provider)
}

/// A person's *web device* (`crate::web_devices`): their desktop app or
/// `metro-connector` running web searches with their own providers and keys.
/// Not a model provider (never in [`DEVICE_PROVIDERS`], never shared or
/// borrowed); it serves `web_search` and `web_fetch` only.
pub const DEVICE_WEB: &str = "device-web";

/// Whether a device connection of `provider` serves `operation`.
pub fn serves(provider: &str, operation: Operation) -> bool {
    if provider == DEVICE_WEB {
        return matches!(operation, Operation::WebSearch | Operation::WebFetch);
    }
    if !is_device_provider(provider) {
        return false;
    }
    match operation {
        Operation::WebSearch | Operation::WebFetch => false,
        Operation::Models => true,
        Operation::Complete => !matches!(
            provider,
            DEVICE_SYSTEMONE | DEVICE_EMBEDDINGS | DEVICE_EMBEDDINGGEMMA
        ),
        Operation::Decide => provider == DEVICE_SYSTEMONE,
        Operation::Embed => provider == DEVICE_EMBEDDINGS || provider == DEVICE_EMBEDDINGGEMMA,
        Operation::MediaEmbed => provider == DEVICE_EMBEDDINGGEMMA,
    }
}

/// Longest device bearer token considered.
const MAX_TOKEN_LEN: usize = 512;
/// Longest error text a device may report, in bytes.
const MAX_DEVICE_ERROR: usize = 2048;
/// Most model ids a `models` answer may list, and the longest one.
const MAX_MODELS: usize = 1000;
const MAX_APPROVED_MODELS: usize = 64;
const MAX_MODEL_ID: usize = 256;
/// Finished job ids remembered to tell duplicates and late results apart
/// from unknown ids.
const FINISHED_MEMORY: usize = 4096;
/// Slack over the response bound for the result envelope.
const RESULT_ENVELOPE: usize = 4096;

/// The operations a device serves (each connection only its own, see
/// [`serves`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Models,
    Complete,
    /// One System One request.
    Decide,
    /// One embeddings request.
    Embed,
    /// One media embeddings request (a query or one asset), only on
    /// `device-embeddinggemma-2`.
    #[serde(rename = "media_embed")]
    MediaEmbed,
    /// One web search on a person's own web device (`device-web` only).
    #[serde(rename = "web_search")]
    WebSearch,
    /// Reading web pages on a person's own web device (`device-web` only).
    #[serde(rename = "web_fetch")]
    WebFetch,
}

/// Who asks for a relayed call, and where. Kept with the job so a borrowed
/// job can be re-checked for exactly this person and Space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    /// Stable identity-provider id.
    pub user: String,
    /// The Space the call is for; `None` for the personal Chat.
    pub space: Option<String>,
}

impl Caller {
    pub fn personal(user: &str) -> Self {
        Self {
            user: user.to_owned(),
            space: None,
        }
    }

    pub fn in_space(user: &str, space: &str) -> Self {
        Self {
            user: user.to_owned(),
            space: Some(space.to_owned()),
        }
    }
}

/// A personal provider connection as the providers lane stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceConnection {
    pub id: String,
    /// Stable identity-provider id of the connection's owner.
    pub owner_id: String,
    /// Provider id, such as `device-openai-oauth`.
    pub provider: String,
}

/// Reads provider connections. Implemented by `crate::providers`; called
/// inside [`Db::call`] with the connection already locked, so it must not
/// lock the database again.
pub trait ConnectionDirectory: Send + Sync + 'static {
    /// The connection `id`, or `None` when it doesn't exist.
    fn device_connection(
        &self,
        conn: &Connection,
        id: &str,
    ) -> anyhow::Result<Option<DeviceConnection>>;

    /// Whether `user`, who is not the owner, may use `connection` right now
    /// in `space` (`None` = personal Chat) under a share its owner made.
    /// Same locking rule as [`Self::device_connection`]. The default allows
    /// nothing, so borrowing stays off until the share store answers.
    fn shared_use_allowed(
        &self,
        _conn: &Connection,
        _connection: &DeviceConnection,
        _user: &str,
        _space: Option<&str>,
    ) -> anyhow::Result<bool> {
        Ok(false)
    }

    /// Whether `user` is a member of Space `space` right now. Same locking
    /// rule. Space-scoped work (the owner's own included) is released to a
    /// device, and its result back, only while this holds. The default
    /// answers no.
    fn member(&self, _conn: &Connection, _space: &str, _user: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
}

/// Bounds of the relay.
#[derive(Clone, Debug)]
pub struct DeviceLimits {
    /// Jobs waiting for one connection's device at once.
    pub queue_capacity: usize,
    /// How long a queued job may wait to be claimed.
    pub claim_timeout: Duration,
    /// How long a claimed job may take to be answered.
    pub run_timeout: Duration,
    /// A device that hasn't polled for this long is offline: jobs fail at
    /// once instead of waiting for a claim.
    pub offline_after: Duration,
    /// Largest serialized request handed to a device.
    pub max_request_bytes: usize,
    /// Largest serialized response a device may return.
    pub max_response_bytes: usize,
    /// Largest serialized `media_embed` request (only that operation).
    pub max_media_request_bytes: usize,
    /// Bytes all pending `media_embed` requests may hold together, across
    /// the relay, from queueing until the job ends.
    pub max_pending_media_bytes: usize,
    /// Of `max_pending_media_bytes`, what one person (stable user id) may
    /// hold at once, whoever's connection they use.
    pub max_media_bytes_per_caller: usize,
    /// Of `max_pending_media_bytes`, what one connection's jobs may hold at
    /// once, whoever sent them.
    pub max_media_bytes_per_connection: usize,
    /// Longest vector a device's `media_embed` answer may carry, so the
    /// most rows fit `max_response_bytes`.
    pub max_media_dimensions: usize,
}

impl Default for DeviceLimits {
    fn default() -> Self {
        Self {
            queue_capacity: 8,
            claim_timeout: Duration::from_secs(30),
            run_timeout: Duration::from_secs(300),
            offline_after: Duration::from_secs(45),
            max_request_bytes: 4 * 1024 * 1024,
            max_response_bytes: 2 * 1024 * 1024,
            max_media_request_bytes: metro_model_protocol::MEDIA_MAX_REQUEST_BYTES,
            max_pending_media_bytes: 3 * metro_model_protocol::MEDIA_MAX_REQUEST_BYTES,
            // One largest asset per person, two per device.
            max_media_bytes_per_caller: metro_model_protocol::MEDIA_MAX_REQUEST_BYTES,
            max_media_bytes_per_connection: 2 * metro_model_protocol::MEDIA_MAX_REQUEST_BYTES,
            max_media_dimensions: metro_model_protocol::MEDIA_STANDARD_DIMENSIONS,
        }
    }
}

/// Why a relayed call failed. The messages are for the person whose run
/// failed and never contain payloads or credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceError {
    /// No such connection for this owner, or it isn't a device connection.
    NotFound,
    /// The connection has no device token yet.
    NotPaired,
    /// The connector hasn't polled recently.
    Offline,
    QueueFull,
    /// Nobody claimed the job in time.
    NotClaimed,
    /// The device claimed the job but didn't answer in time.
    TimedOut,
    /// The device token was rotated or revoked, or the connection deleted,
    /// while the job was pending.
    CredentialChanged,
    /// The request or the response is larger than the relay allows.
    TooLarge,
    /// A typed request that doesn't fit its protocol (fixed text).
    InvalidRequest(String),
    /// The share a borrowed job relied on is gone (revoked, narrowed, or
    /// the caller left the Space); the job was cancelled or its result
    /// withheld.
    AccessRevoked,
    /// The device reported a failure (bounded, sanitized text).
    Device(String),
    /// The device answered with something that isn't the expected shape.
    BadResponse(String),
    Internal,
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceError::NotFound => f.write_str("no such device model connection"),
            DeviceError::NotPaired => f.write_str(
                "this device connection isn't paired yet; create a device token and start \
                 metro-connector with it",
            ),
            DeviceError::Offline => f.write_str(
                "your device connector is offline; start metro-connector on your computer \
                 and try again",
            ),
            DeviceError::QueueFull => {
                f.write_str("your device connector is busy; try again shortly")
            }
            DeviceError::NotClaimed => f.write_str(
                "your device connector didn't pick up the request in time; is it running?",
            ),
            DeviceError::TimedOut => {
                f.write_str("your device connector didn't return a result in time")
            }
            DeviceError::CredentialChanged => f.write_str(
                "the device token was rotated or revoked, or the connection was deleted; \
                 the request was cancelled",
            ),
            DeviceError::TooLarge => {
                f.write_str("the model request or response is too large for the device relay")
            }
            DeviceError::InvalidRequest(e) => write!(f, "{e}"),
            DeviceError::AccessRevoked => f.write_str(
                "access to this shared model connection was revoked; the request was cancelled",
            ),
            // The device's own text stays out of every message and log: it
            // can carry whatever the vendor or the connector echoed. The
            // connector keeps it in its own log.
            DeviceError::Device(_) => {
                f.write_str("your device connector reported an error; see its own log")
            }
            DeviceError::BadResponse(_) => {
                f.write_str("your device connector returned an invalid response")
            }
            DeviceError::Internal => f.write_str("internal error in the device relay"),
        }
    }
}

impl std::error::Error for DeviceError {}

impl DeviceError {
    /// A fixed name of the failure, for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            DeviceError::NotFound => "not_found",
            DeviceError::NotPaired => "not_paired",
            DeviceError::Offline => "offline",
            DeviceError::QueueFull => "queue_full",
            DeviceError::NotClaimed => "not_claimed",
            DeviceError::TimedOut => "timed_out",
            DeviceError::CredentialChanged => "credential_changed",
            DeviceError::TooLarge => "too_large",
            DeviceError::InvalidRequest(_) => "invalid_request",
            DeviceError::AccessRevoked => "access_revoked",
            DeviceError::Device(_) => "device_error",
            DeviceError::BadResponse(_) => "bad_response",
            DeviceError::Internal => "internal",
        }
    }
}

// ---------------------------------------------------------------------------
// Storage: one device credential per connection, as a hash.

/// Create the credentials table. Called from `Db::init`.
pub fn init(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS device_credentials (
             connection_id TEXT PRIMARY KEY,
             owner_id      TEXT NOT NULL,
             token_hash    BLOB NOT NULL UNIQUE,
             generation    INTEGER NOT NULL,
             created_at    INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS device_generations (
             connection_id TEXT PRIMARY KEY,
             generation    INTEGER NOT NULL
         );",
    )?;
    Ok(())
}

/// A fresh device token: the prefix and 256 random bits as hex.
pub fn new_device_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no system randomness: {e}"))?;
    let mut token = String::with_capacity(DEVICE_TOKEN_PREFIX.len() + 64);
    token.push_str(DEVICE_TOKEN_PREFIX);
    for b in bytes {
        token.push_str(&format!("{b:02x}"));
    }
    Ok(token)
}

/// What the database stores for a device token (256 bits of entropy, so no
/// salt or stretching is needed).
pub fn device_token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The next generation for `connection`. Generations only go up, across
/// revocation too, so a job queued under an old token can never match a
/// new one.
fn bump_generation(conn: &Connection, connection: &str) -> rusqlite::Result<u64> {
    conn.execute(
        "INSERT INTO device_generations (connection_id, generation) VALUES (?1, 1)
         ON CONFLICT (connection_id) DO UPDATE SET generation = generation + 1",
        params![connection],
    )?;
    let g: i64 = conn.query_row(
        "SELECT generation FROM device_generations WHERE connection_id = ?1",
        params![connection],
        |r| r.get(0),
    )?;
    Ok(g as u64)
}

/// Store `hash` as the connection's only token; returns its generation.
fn store_credential(
    conn: &Connection,
    connection: &str,
    owner: &str,
    hash: &[u8],
) -> anyhow::Result<u64> {
    let tx = conn.unchecked_transaction()?;
    let generation = bump_generation(&tx, connection)?;
    tx.execute(
        "INSERT INTO device_credentials (connection_id, owner_id, token_hash, generation, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (connection_id) DO UPDATE SET
             owner_id = excluded.owner_id, token_hash = excluded.token_hash,
             generation = excluded.generation, created_at = excluded.created_at",
        params![connection, owner, hash, generation as i64, now_secs()],
    )?;
    tx.commit()?;
    Ok(generation)
}

/// Drop the connection's token. Returns the new generation when there was
/// one to drop.
fn delete_credential(conn: &Connection, connection: &str) -> anyhow::Result<Option<u64>> {
    let tx = conn.unchecked_transaction()?;
    let generation = delete_credential_in(&tx, connection)?;
    tx.commit()?;
    Ok(generation)
}

/// [`delete_credential`] inside a transaction the caller holds.
fn delete_credential_in(conn: &Connection, connection: &str) -> anyhow::Result<Option<u64>> {
    let n = conn.execute(
        "DELETE FROM device_credentials WHERE connection_id = ?1",
        params![connection],
    )?;
    Ok(if n > 0 {
        Some(bump_generation(conn, connection)?)
    } else {
        None
    })
}

/// `(owner, generation)` of the connection's current token.
pub(crate) fn credential_of(
    conn: &Connection,
    connection: &str,
) -> rusqlite::Result<Option<(String, u64)>> {
    conn.query_row(
        "SELECT owner_id, generation FROM device_credentials WHERE connection_id = ?1",
        params![connection],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)),
    )
    .optional()
}

/// `(connection, owner, generation)` of a token hash.
fn credential_by_hash(
    conn: &Connection,
    hash: &[u8],
) -> rusqlite::Result<Option<(String, String, u64)>> {
    conn.query_row(
        "SELECT connection_id, owner_id, generation FROM device_credentials WHERE token_hash = ?1",
        params![hash],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as u64,
            ))
        },
    )
    .optional()
}

/// The connection `id`: a web device (`wd_…`, from `crate::web_devices`),
/// else whatever the providers' directory knows.
fn lookup(
    directory: &dyn ConnectionDirectory,
    conn: &Connection,
    id: &str,
) -> anyhow::Result<Option<DeviceConnection>> {
    if crate::web_devices::is_web_device_id(id) {
        return crate::web_devices::device_connection(conn, id);
    }
    Ok(directory
        .device_connection(conn, id)?
        .filter(|c| c.provider != DEVICE_WEB))
}

/// Whether `provider` may hold a device token.
fn takes_device_token(provider: &str) -> bool {
    is_device_provider(provider) || provider == DEVICE_WEB
}

/// The connection, when it exists, belongs to `owner` and is a device one.
fn owned_device_connection(
    directory: &dyn ConnectionDirectory,
    conn: &Connection,
    owner: &str,
    connection: &str,
) -> anyhow::Result<Option<DeviceConnection>> {
    Ok(lookup(directory, conn, connection)?.filter(|c| c.id == connection && c.owner_id == owner))
}

// ---------------------------------------------------------------------------
// The in-memory relay.

/// A device token resolved on this request: the connection it serves, that
/// connection's owner and the token's generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub connection: String,
    pub owner: String,
    pub generation: u64,
}

/// A job as handed to a device.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ClaimedDeviceJob {
    pub id: String,
    pub operation: Operation,
    pub request: Value,
}

/// Online state of a connection's device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeviceStatus {
    pub online: bool,
    /// Seconds since the device last polled, while this server has run.
    pub last_seen_secs: Option<u64>,
}

/// How a result submission went.
#[derive(Debug, PartialEq, Eq)]
pub enum ResultError {
    /// Unknown, or another connection's.
    Unknown,
    /// Not claimed by this device (yet).
    NotClaimed,
    /// Failed (timed out, abandoned or cancelled) before the result came.
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Finished {
    Completed,
    Expired,
}

enum JobState {
    Queued,
    /// Picked by a device poll; its grant is being re-checked first.
    Checking,
    Claimed {
        at: Instant,
    },
}

struct Job {
    connection: String,
    generation: u64,
    operation: Operation,
    /// An explicit turn validated against an upgraded connector's inventory.
    requires_inventory: bool,
    claim_grant: Option<Arc<crate::providers::guard::DeviceClaimGrant>>,
    /// The connection's owner when the job was queued.
    owner: String,
    /// Set for borrowed work: the caller that isn't the owner.
    borrowed: Option<Caller>,
    /// Taken when claimed, so the payload lives no longer than needed.
    request: Option<Value>,
    /// How long the device may take once it claimed the job.
    run_timeout: Duration,
    /// What this job holds of `max_pending_media_bytes` (0 for text work),
    /// charged to `caller` and `connection` too.
    media_bytes: usize,
    /// The stable user id the job is for (owner or borrower).
    caller: String,
    /// For work in a Space: who asked, and where. Rechecked for current
    /// membership right before the payload is released to the device.
    in_space: Option<Caller>,
    state: JobState,
    tx: oneshot::Sender<Result<Value, DeviceError>>,
}

impl Job {
    fn access(&self) -> JobAccess {
        JobAccess {
            borrowed: self.borrowed.clone(),
            grant: self.claim_grant.clone(),
            in_space: self.in_space.clone(),
            operation: self.operation,
        }
    }

    fn inventory_allows(&self, models: Option<&[String]>) -> bool {
        if self.operation != Operation::Complete {
            return true;
        }
        let Some(request) = &self.request else {
            return false;
        };
        match models {
            Some(ids) => request["model"]
                .as_str()
                .is_some_and(|m| ids.iter().any(|id| id == m)),
            None => !self.requires_inventory && request["reasoning_effort"].is_null(),
        }
    }
}

fn inventory_changed() -> DeviceError {
    DeviceError::InvalidRequest(
        "the device no longer approves that model or reasoning option".into(),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExpectedDevice {
    pub generation: u64,
    pub advertised: bool,
}

/// Pins prepared provider work to its pairing and connection snapshot.
pub(crate) struct DispatchBinding {
    pub generation: u64,
    pub grant: crate::providers::guard::DeviceClaimGrant,
    /// Only explicitly selected chat models rely on the advertised inventory.
    pub advertised: Option<bool>,
}

#[derive(Default)]
struct ConnQueue {
    queue: VecDeque<String>,
    /// Highest token generation seen for this connection.
    generation: u64,
    /// Last poll, and the generation it was made with.
    last_seen: Option<(Instant, u64)>,
    /// Chat model allowlist from the most recent authenticated HTTP poll.
    /// None means a legacy connector, not an empty allowlist.
    approved_models: Option<Vec<String>>,
}

#[derive(Default)]
struct Inner {
    conns: HashMap<String, ConnQueue>,
    jobs: HashMap<String, Job>,
    finished: HashMap<String, (String, u64, Finished)>,
    finished_order: VecDeque<String>,
    /// Sum of `Job::media_bytes` of the jobs in `jobs`, overall, by caller
    /// and by connection.
    media_bytes: usize,
    media_by_caller: HashMap<String, usize>,
    media_by_connection: HashMap<String, usize>,
}

impl Inner {
    /// Remove a job, releasing its media bytes.
    fn take(&mut self, id: &str) -> Option<Job> {
        let job = self.jobs.remove(id)?;
        if job.media_bytes > 0 {
            self.media_bytes = self.media_bytes.saturating_sub(job.media_bytes);
            release(&mut self.media_by_caller, &job.caller, job.media_bytes);
            release(
                &mut self.media_by_connection,
                &job.connection,
                job.media_bytes,
            );
        }
        Some(job)
    }

    /// Whether `bytes` more of media fit every budget for `caller` on
    /// `connection`; charges them when they do.
    fn charge_media(
        &mut self,
        limits: &DeviceLimits,
        caller: &str,
        connection: &str,
        bytes: usize,
    ) -> bool {
        let by_caller = self.media_by_caller.get(caller).copied().unwrap_or(0);
        let by_connection = self
            .media_by_connection
            .get(connection)
            .copied()
            .unwrap_or(0);
        if self.media_bytes + bytes > limits.max_pending_media_bytes
            || by_caller + bytes > limits.max_media_bytes_per_caller
            || by_connection + bytes > limits.max_media_bytes_per_connection
        {
            return false;
        }
        self.media_bytes += bytes;
        *self.media_by_caller.entry(caller.to_owned()).or_default() += bytes;
        *self
            .media_by_connection
            .entry(connection.to_owned())
            .or_default() += bytes;
        true
    }

    fn finish(&mut self, id: &str, connection: &str, generation: u64, how: Finished) {
        if self
            .finished
            .insert(id.to_owned(), (connection.to_owned(), generation, how))
            .is_none()
        {
            self.finished_order.push_back(id.to_owned());
            while self.finished_order.len() > FINISHED_MEMORY {
                if let Some(old) = self.finished_order.pop_front() {
                    self.finished.remove(&old);
                }
            }
        }
    }

    fn fail(&mut self, id: &str, why: Option<DeviceError>) {
        if let Some(job) = self.take(id) {
            if let Some(q) = self.conns.get_mut(&job.connection) {
                q.queue.retain(|q| q != id);
            }
            self.finish(id, &job.connection, job.generation, Finished::Expired);
            if let Some(why) = why {
                let _ = job.tx.send(Err(why));
            }
        }
    }

    /// Note that `connection` now has token `generation`. Returns false for
    /// an older generation than one already seen; a newer one fails every
    /// pending job of an older one.
    fn observe(&mut self, connection: &str, generation: u64) -> bool {
        let q = self.conns.entry(connection.to_owned()).or_default();
        if generation < q.generation {
            return false;
        }
        if generation > q.generation {
            q.generation = generation;
            q.last_seen = q.last_seen.filter(|(_, g)| *g == generation);
            q.approved_models = None;
            let doomed: Vec<String> = self
                .jobs
                .iter()
                .filter(|(_, j)| j.connection == connection && j.generation < generation)
                .map(|(id, _)| id.clone())
                .collect();
            for id in doomed {
                self.fail(&id, Some(DeviceError::CredentialChanged));
            }
        }
        true
    }

    fn unqueue(&mut self, connection: &str, id: &str) {
        if let Some(q) = self.conns.get_mut(connection) {
            q.queue.retain(|q| q != id);
        }
    }

    /// Fail every pending job of `connection` and forget its device.
    fn flush(&mut self, connection: &str) {
        let doomed: Vec<String> = self
            .jobs
            .iter()
            .filter(|(_, j)| j.connection == connection)
            .map(|(id, _)| id.clone())
            .collect();
        for id in doomed {
            self.fail(&id, Some(DeviceError::CredentialChanged));
        }
        if let Some(q) = self.conns.get_mut(connection) {
            q.last_seen = None;
        }
    }
}

fn release(map: &mut HashMap<String, usize>, key: &str, bytes: usize) {
    if let Some(n) = map.get_mut(key) {
        *n = n.saturating_sub(bytes);
        if *n == 0 {
            map.remove(key);
        }
    }
}

/// What a device poll found.
enum Next {
    Ready(ClaimedDeviceJob),
    /// A Space, borrowed, or bound job, to re-check first.
    Check {
        id: String,
        access: JobAccess,
    },
}

struct JobAccess {
    borrowed: Option<Caller>,
    grant: Option<Arc<crate::providers::guard::DeviceClaimGrant>>,
    in_space: Option<Caller>,
    operation: Operation,
}

impl JobAccess {
    fn needs_check(&self) -> bool {
        self.borrowed.is_some() || self.grant.is_some() || self.in_space.is_some()
    }

    /// Check the live grant and pairing with the database lock held.
    fn check(
        &self,
        conn: &Connection,
        directory: &dyn ConnectionDirectory,
        device: &Device,
    ) -> anyhow::Result<Result<(), DeviceError>> {
        if let Some(who) = &self.in_space
            && let Some(space) = &who.space
            && !directory.member(conn, space, &who.user)?
        {
            return Ok(Err(DeviceError::AccessRevoked));
        }
        if let Some(who) = &self.borrowed {
            let allowed = match lookup(directory, conn, &device.connection)? {
                Some(dc)
                    if dc.id == device.connection
                        && dc.owner_id == device.owner
                        && dc.owner_id != who.user
                        && dc.provider != DEVICE_WEB
                        && dc.provider != SUBSCRIPTION_PROVIDER
                        && serves(&dc.provider, self.operation) =>
                {
                    directory.shared_use_allowed(conn, &dc, &who.user, who.space.as_deref())?
                }
                _ => false,
            };
            if !allowed {
                return Ok(Err(DeviceError::AccessRevoked));
            }
        }
        if let Some(grant) = &self.grant
            && !grant.allowed(conn)?
        {
            return Ok(Err(DeviceError::AccessRevoked));
        }
        if !credential_of(conn, &device.connection)?.is_some_and(|(owner, generation)| {
            owner == device.owner && generation == device.generation
        }) {
            return Ok(Err(DeviceError::CredentialChanged));
        }
        Ok(Ok(()))
    }
}

fn access_check_error(error: anyhow::Error) -> DeviceError {
    tracing::error!(%error, "checking device job access");
    DeviceError::Internal
}

/// Model ids of a validated `models` answer.
fn model_ids(response: &Value) -> Vec<String> {
    response["models"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A chat request for the device wire, which has no streaming.
fn unary(mut request: Value) -> Result<Value, DeviceError> {
    let Some(body) = request.as_object_mut() else {
        return Err(DeviceError::BadResponse("request is not an object".into()));
    };
    body.remove("stream");
    body.remove("stream_options");
    Ok(request)
}

/// Withdraws its job when the waiting future is dropped.
struct Abandon<'a> {
    relay: &'a DeviceRelay,
    id: String,
}

impl Drop for Abandon<'_> {
    fn drop(&mut self) {
        self.relay.lock().fail(&self.id, None);
    }
}

/// Relays model jobs to personal devices. Cheap to clone.
#[derive(Clone)]
pub struct DeviceRelay {
    inner: Arc<Mutex<Inner>>,
    limits: Arc<DeviceLimits>,
    directory: Arc<dyn ConnectionDirectory>,
}

impl DeviceRelay {
    pub fn new(limits: DeviceLimits, directory: Arc<dyn ConnectionDirectory>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            limits: Arc::new(limits),
            directory,
        }
    }

    pub fn limits(&self) -> &DeviceLimits {
        &self.limits
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The models the owner's device offers on `connection`.
    pub async fn models(
        &self,
        db: &Db,
        owner: &str,
        connection: &str,
    ) -> Result<Vec<String>, DeviceError> {
        let response = self
            .dispatch(db, owner, connection, Operation::Models, json!({}))
            .await?;
        Ok(model_ids(&response))
    }

    /// [`Self::models`] with the catalog name the device reports for each
    /// model, if any (an optional `"display_names": {ID: NAME}` beside
    /// `"models"`; names that are not short printable strings are dropped).
    pub async fn models_named(
        &self,
        db: &Db,
        owner: &str,
        connection: &str,
    ) -> Result<Vec<(String, Option<String>)>, DeviceError> {
        let response = self
            .dispatch(db, owner, connection, Operation::Models, json!({}))
            .await?;
        let names = response.get("display_names").and_then(Value::as_object);
        Ok(model_ids(&response)
            .into_iter()
            .map(|id| {
                let name = names
                    .and_then(|n| n.get(&id))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|n| {
                        !n.is_empty()
                            && n.chars().count() <= 128
                            && !n.chars().any(char::is_control)
                    })
                    .map(str::to_owned);
                (id, name)
            })
            .collect())
    }

    /// [`Self::models`] for `caller`, who may be borrowing the connection.
    pub async fn models_as(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
    ) -> Result<Vec<String>, DeviceError> {
        let response = self
            .dispatch_as(db, caller, connection, Operation::Models, json!({}))
            .await?;
        Ok(model_ids(&response))
    }

    /// One Chat Completions call on the owner's device: `request` in, the
    /// `chat.completion` object out.
    pub async fn complete(
        &self,
        db: &Db,
        owner: &str,
        connection: &str,
        request: Value,
    ) -> Result<Value, DeviceError> {
        let request = unary(request)?;
        self.dispatch(db, owner, connection, Operation::Complete, request)
            .await
    }

    /// [`Self::complete`] for `caller`, who may be borrowing the connection.
    pub async fn complete_as(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        request: Value,
    ) -> Result<Value, DeviceError> {
        let request = unary(request)?;
        self.dispatch_as(db, caller, connection, Operation::Complete, request)
            .await
    }

    pub(crate) async fn complete_selected_as(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        request: Value,
        expected: ExpectedDevice,
        grant: crate::providers::guard::DeviceClaimGrant,
    ) -> Result<Value, DeviceError> {
        let request = unary(request)?;
        let size = metro_model_protocol::json_len(&request);
        self.dispatch_sized(
            db,
            caller,
            connection,
            Operation::Complete,
            request,
            size,
            true,
            Some(DispatchBinding {
                generation: expected.generation,
                grant,
                advertised: Some(expected.advertised),
            }),
            None,
        )
        .await
    }

    /// One System One call on a `device-systemone` connection. The request
    /// is checked before it is queued and the answers against the questions
    /// asked; the result is the normalized response
    /// (`metro_model_protocol::check_decide_response`).
    pub async fn decide(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        request: Value,
    ) -> Result<Value, DeviceError> {
        self.decide_bound(db, caller, connection, request, None)
            .await
    }

    pub(crate) async fn decide_bound(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        request: Value,
        binding: Option<DispatchBinding>,
    ) -> Result<Value, DeviceError> {
        let req = metro_model_protocol::check_decide_request(&request)
            .map_err(|e| DeviceError::InvalidRequest(e.to_string()))?;
        let body = serde_json::to_value(&req).map_err(|_| DeviceError::Internal)?;
        let response = self
            .dispatch_inner(
                db,
                caller,
                connection,
                Operation::Decide,
                body,
                true,
                binding,
            )
            .await?;
        metro_model_protocol::check_decide_response(&req, &response)
            .map_err(|e| DeviceError::BadResponse(e.to_string()))
    }

    /// One embeddings call on a `device-embeddings` connection, checked like
    /// [`Self::decide`]; vectors come back sorted by input index.
    pub async fn embed(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        request: Value,
    ) -> Result<Value, DeviceError> {
        self.embed_bound(db, caller, connection, request, None)
            .await
    }

    pub(crate) async fn embed_bound(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        request: Value,
        binding: Option<DispatchBinding>,
    ) -> Result<Value, DeviceError> {
        let req = metro_model_protocol::check_embed_request(&request)
            .map_err(|e| DeviceError::InvalidRequest(e.to_string()))?;
        let mut body = json!({"model": req.model, "input": req.input});
        if let Some(d) = req.dimensions {
            body["dimensions"] = json!(d);
        }
        let response = self
            .dispatch_inner(
                db,
                caller,
                connection,
                Operation::Embed,
                body,
                true,
                binding,
            )
            .await?;
        metro_model_protocol::check_embed_response(&req, &response)
            .map_err(|e| DeviceError::BadResponse(e.to_string()))
    }

    /// One media embeddings call on a `device-embeddinggemma-2` connection:
    /// `{model, query}` or `{model, asset}`, checked in full before it is
    /// queued; the answer is checked against the request (one query row, or
    /// typed document rows of at most `max_media_dimensions`) and returned
    /// normalized (`metro_model_protocol::check_media_response`).
    pub async fn media_embed(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        request: Value,
    ) -> Result<Value, DeviceError> {
        let req = metro_model_protocol::parse_media_request(request)
            .map_err(|e| DeviceError::InvalidRequest(e.to_string()))?;
        self.media_embed_request(db, caller, connection, req).await
    }

    /// [`Self::media_embed`] with a request already checked: the asset is
    /// moved into the job, not copied, and its size is already known.
    pub async fn media_embed_request(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        request: metro_model_protocol::MediaRequest,
    ) -> Result<Value, DeviceError> {
        self.media_embed_request_bound(db, caller, connection, request, None)
            .await
    }

    pub(crate) async fn media_embed_request_bound(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        request: metro_model_protocol::MediaRequest,
        binding: Option<DispatchBinding>,
    ) -> Result<Value, DeviceError> {
        let size = request.encoded_len();
        let shape = request.shape();
        let body = request.into_json();
        let response = self
            .dispatch_sized(
                db,
                caller,
                connection,
                Operation::MediaEmbed,
                body,
                size,
                true,
                binding,
                None,
            )
            .await?;
        metro_model_protocol::check_media_response(
            &shape,
            &response,
            self.limits.max_media_dimensions,
        )
        .map_err(|e| DeviceError::BadResponse(e.to_string()))
    }

    /// Queue `operation` for the device serving `connection`, which must be
    /// `owner`'s, and wait for its answer.
    pub async fn dispatch(
        &self,
        db: &Db,
        owner: &str,
        connection: &str,
        operation: Operation,
        request: Value,
    ) -> Result<Value, DeviceError> {
        self.dispatch_inner(
            db,
            &Caller::personal(owner),
            connection,
            operation,
            request,
            false,
            None,
        )
        .await
    }

    /// Queue `operation` on `connection` for `caller`: the owner, or someone
    /// the owner shared the connection with for `caller.space`. The job runs
    /// on the owner's device only; a borrowed job is re-checked before a
    /// claim and before its result is returned.
    pub async fn dispatch_as(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        operation: Operation,
        request: Value,
    ) -> Result<Value, DeviceError> {
        self.dispatch_inner(db, caller, connection, operation, request, true, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn dispatch_inner(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        operation: Operation,
        request: Value,
        allow_borrow: bool,
        binding: Option<DispatchBinding>,
    ) -> Result<Value, DeviceError> {
        // Counted while serializing into nothing: no copy of the payload.
        let size = metro_model_protocol::json_len(&request);
        self.dispatch_sized(
            db,
            caller,
            connection,
            operation,
            request,
            size,
            allow_borrow,
            binding,
            None,
        )
        .await
    }

    /// One web job on the initiator's own device, never borrowed, with the
    /// operation's own timeout. A Space caller is checked at claim and
    /// result delivery; a personal caller has no Space.
    pub async fn dispatch_web(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        operation: Operation,
        request: Value,
        run_timeout: Duration,
    ) -> Result<Value, DeviceError> {
        if !matches!(operation, Operation::WebSearch | Operation::WebFetch) {
            return Err(DeviceError::NotFound);
        }
        let size = metro_model_protocol::json_len(&request);
        self.dispatch_sized(
            db,
            caller,
            connection,
            operation,
            request,
            size,
            false,
            None,
            Some(run_timeout),
        )
        .await
    }

    /// [`Self::dispatch_inner`] with the request's encoded `size` known.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_sized(
        &self,
        db: &Db,
        caller: &Caller,
        connection: &str,
        operation: Operation,
        request: Value,
        size: usize,
        allow_borrow: bool,
        binding: Option<DispatchBinding>,
        run_timeout: Option<Duration>,
    ) -> Result<Value, DeviceError> {
        let media = operation == Operation::MediaEmbed;
        let cap = if media {
            self.limits.max_media_request_bytes
        } else {
            self.limits.max_request_bytes
        };
        if size > cap {
            return Err(DeviceError::TooLarge);
        }
        let directory = self.directory.clone();
        let (who, c) = (caller.clone(), connection.to_owned());
        let expected_generation = binding.as_ref().map(|b| b.generation);
        let (generation, owner, borrowed) = db
            .call(move |db| {
                let conn = db.lock();
                let Some(dc) = lookup(&*directory, &conn, &c)?
                    .filter(|d| d.id == c && serves(&d.provider, operation))
                else {
                    return Ok(Err(DeviceError::NotFound));
                };
                let borrowed = dc.owner_id != who.user;
                // A web device is never borrowed: searches run only on the
                // asking person's own machine.
                if borrowed
                    && (!allow_borrow
                        || dc.provider == DEVICE_WEB
                        || dc.provider == SUBSCRIPTION_PROVIDER
                        || !directory.shared_use_allowed(
                            &conn,
                            &dc,
                            &who.user,
                            who.space.as_deref(),
                        )?)
                {
                    return Ok(Err(DeviceError::NotFound));
                }
                Ok(match credential_of(&conn, &c)? {
                    // Only the owner's own token serves it; a token issued
                    // to someone else (the connection changed hands) never.
                    Some((cred_owner, g))
                        if cred_owner == dc.owner_id
                            && expected_generation.is_none_or(|e| e == g) =>
                    {
                        Ok((g, dc.owner_id, borrowed))
                    }
                    _ if expected_generation.is_some() => Err(DeviceError::CredentialChanged),
                    _ => Err(DeviceError::NotPaired),
                })
            })
            .await
            .map_err(|e| {
                tracing::error!(error = %format!("{e:#}"), "device relay lookup");
                DeviceError::Internal
            })??;

        let id = uuid::Uuid::new_v4().to_string();
        let (tx, mut rx) = oneshot::channel();
        let advertised = binding.as_ref().and_then(|b| b.advertised);
        let grant = binding.map(|b| Arc::new(b.grant));
        {
            let mut inner = self.lock();
            if !inner.observe(connection, generation) {
                return Err(DeviceError::CredentialChanged);
            }
            let q = inner.conns.entry(connection.to_owned()).or_default();
            if advertised.is_some_and(|expected| expected != q.approved_models.is_some()) {
                return Err(DeviceError::CredentialChanged);
            }
            let online = q.last_seen.is_some_and(|(at, g)| {
                g == generation && at.elapsed() <= self.limits.offline_after
            });
            if !online {
                return Err(DeviceError::Offline);
            }
            if q.queue.len() >= self.limits.queue_capacity {
                return Err(DeviceError::QueueFull);
            }
            let media_bytes = if media { size } else { 0 };
            if media && !inner.charge_media(&self.limits, &caller.user, connection, media_bytes) {
                return Err(DeviceError::QueueFull);
            }
            let q = inner.conns.entry(connection.to_owned()).or_default();
            q.queue.push_back(id.clone());
            inner.jobs.insert(
                id.clone(),
                Job {
                    connection: connection.to_owned(),
                    generation,
                    operation,
                    requires_inventory: advertised == Some(true),
                    claim_grant: grant.clone(),
                    owner: owner.clone(),
                    borrowed: borrowed.then(|| caller.clone()),
                    request: Some(request),
                    run_timeout: run_timeout.unwrap_or(self.limits.run_timeout),
                    media_bytes,
                    caller: caller.user.clone(),
                    in_space: caller.space.is_some().then(|| caller.clone()),
                    state: JobState::Queued,
                    tx,
                },
            );
        }
        tracing::debug!(job = %id, %connection, ?operation, borrowed, "device job queued");
        let _abandon = Abandon {
            relay: self,
            id: id.clone(),
        };
        let mut deadline = Instant::now() + self.limits.claim_timeout;
        let outcome = loop {
            tokio::select! {
                outcome = &mut rx => {
                    let outcome = outcome.unwrap_or(Err(DeviceError::Internal));
                    tracing::debug!(job = %id, ok = outcome.is_ok(), "device job finished");
                    break outcome;
                }
                () = tokio::time::sleep_until(deadline) => {
                    let mut inner = self.lock();
                    let next = match inner.jobs.get(&id).map(|j| &j.state) {
                        Some(JobState::Queued | JobState::Checking) => Err(DeviceError::NotClaimed),
                        Some(JobState::Claimed { at }) => {
                            let run = inner.jobs.get(&id).map_or(self.limits.run_timeout, |j| j.run_timeout);
                            let end = *at + run;
                            if end <= Instant::now() { Err(DeviceError::TimedOut) } else { Ok(end) }
                        }
                        // Finished meanwhile: the result is in the channel.
                        None => Ok(Instant::now() + Duration::from_secs(1)),
                    };
                    match next {
                        Ok(end) => deadline = end,
                        Err(why) => {
                            inner.fail(&id, None);
                            tracing::debug!(job = %id, kind = why.kind(), "device job failed");
                            return Err(why);
                        }
                    }
                }
            }
        };
        let response = outcome?;
        let access = JobAccess {
            borrowed: borrowed.then(|| caller.clone()),
            grant,
            in_space: caller.space.is_some().then(|| caller.clone()),
            operation,
        };
        if access.needs_check() {
            let directory = self.directory.clone();
            let device = Device {
                connection: connection.to_owned(),
                owner,
                generation,
            };
            db.call(move |db| access.check(&db.lock(), &*directory, &device))
                .await
                .map_err(access_check_error)??;
        }
        Ok(response)
    }

    /// Re-check every pending borrowed job of `connection` (`None`: of every
    /// connection) and fail those whose caller lost access. Call after a
    /// share is revoked or narrowed, or a member leaves a Space. The owner's
    /// own jobs are untouched.
    pub async fn shares_changed(&self, db: &Db, connection: Option<&str>) -> anyhow::Result<()> {
        let borrowed: Vec<_> = {
            let inner = self.lock();
            inner
                .jobs
                .iter()
                .filter(|(_, j)| connection.is_none_or(|c| j.connection == c))
                .filter(|(_, job)| job.borrowed.is_some())
                .map(|(id, job)| {
                    (
                        id.clone(),
                        job.access(),
                        Device {
                            connection: job.connection.clone(),
                            owner: job.owner.clone(),
                            generation: job.generation,
                        },
                    )
                })
                .collect()
        };
        for (id, access, device) in borrowed {
            let directory = self.directory.clone();
            let checked = db
                .call(move |db| access.check(&db.lock(), &*directory, &device))
                .await
                .map_err(access_check_error)
                .and_then(|r| r);
            if let Err(reason) = checked {
                tracing::debug!(job = %id, kind = reason.kind(), "borrowed device job cancelled");
                self.lock().fail(&id, Some(reason));
            }
        }
        Ok(())
    }

    /// Pick the next job for `device`: owner jobs are claimed at once, a
    /// borrowed one is marked [`JobState::Checking`] (when `borrowed` allows
    /// it) and returned for its grant check.
    fn next_job(&self, device: &Device, borrowed: bool) -> Option<Next> {
        let mut inner = self.lock();
        if !inner.observe(&device.connection, device.generation) {
            return None;
        }
        let q = inner.conns.entry(device.connection.clone()).or_default();
        q.last_seen = Some((Instant::now(), device.generation));
        let approved = q.approved_models.clone();
        let ids: Vec<String> = q.queue.iter().cloned().collect();
        for id in ids {
            let Some(job) = inner.jobs.get_mut(&id) else {
                inner.unqueue(&device.connection, &id);
                continue;
            };
            if job.generation != device.generation || !matches!(job.state, JobState::Queued) {
                inner.unqueue(&device.connection, &id);
                continue;
            }
            // Recheck at claim: a profile may have removed approval since
            // the message was accepted or since its model was built.
            if !job.inventory_allows(approved.as_deref()) {
                inner.fail(&id, Some(inventory_changed()));
                continue;
            }
            let access = job.access();
            if access.needs_check() {
                if !borrowed {
                    continue;
                }
                job.state = JobState::Checking;
                inner.unqueue(&device.connection, &id);
                return Some(Next::Check { id, access });
            }
            job.state = JobState::Claimed { at: Instant::now() };
            let request = job.request.take().unwrap_or(Value::Null);
            let operation = job.operation;
            inner.unqueue(&device.connection, &id);
            tracing::debug!(job = %id, ?operation, "device job claimed");
            return Some(Next::Ready(ClaimedDeviceJob {
                id,
                operation,
                request,
            }));
        }
        None
    }

    /// The oldest of the owner's own jobs for `device`'s connection, marking
    /// it claimed. Borrowed jobs need a grant check and are only handed out
    /// by [`Self::claim_checked`] (which the claim route uses).
    pub fn claim(&self, device: &Device) -> Option<ClaimedDeviceJob> {
        match self.next_job(device, false) {
            Some(Next::Ready(job)) => Some(job),
            _ => None,
        }
    }

    /// The oldest job for `device`'s connection, marking it claimed.
    /// Recheck access before handing out scoped, borrowed, or bound work.
    pub async fn claim_checked(&self, db: &Db, device: &Device) -> Option<ClaimedDeviceJob> {
        loop {
            let (id, access) = match self.next_job(device, true)? {
                Next::Ready(job) => return Some(job),
                Next::Check { id, access } => (id, access),
            };
            let (relay, device, job_id) = (self.clone(), device.clone(), id.clone());
            match db
                .call(move |db| {
                    let conn = db.lock();
                    let checked = access.check(&conn, &*relay.directory, &device)?;
                    // Keep the DB lock until the checked payload has been taken;
                    // a membership, connection or token write cannot interleave.
                    Ok(relay.release_checked(&device, &job_id, checked))
                })
                .await
            {
                Ok(Some(job)) => return Some(job),
                Ok(None) => {}
                Err(e) => self.lock().fail(&id, Some(access_check_error(e))),
            }
        }
    }

    fn release_checked(
        &self,
        device: &Device,
        id: &str,
        checked: Result<(), DeviceError>,
    ) -> Option<ClaimedDeviceJob> {
        let mut inner = self.lock();
        let approved = inner
            .conns
            .get(&device.connection)
            .and_then(|q| q.approved_models.clone());
        let job = inner.jobs.get_mut(id)?;
        if !matches!(job.state, JobState::Checking)
            || job.connection != device.connection
            || job.owner != device.owner
            || job.generation != device.generation
        {
            return None;
        }
        if let Err(reason) = checked {
            tracing::debug!(job = %id, kind = reason.kind(), "device job refused at claim");
            inner.fail(id, Some(reason));
            return None;
        }
        if !job.inventory_allows(approved.as_deref()) {
            inner.fail(id, Some(inventory_changed()));
            return None;
        }
        job.state = JobState::Claimed { at: Instant::now() };
        let request = job.request.take().unwrap_or(Value::Null);
        let operation = job.operation;
        tracing::debug!(job = %id, ?operation, "checked device job claimed");
        Some(ClaimedDeviceJob {
            id: id.to_owned(),
            operation,
            request,
        })
    }

    /// Accept `device`'s result for job `id`. `Ok(true)` when accepted,
    /// `Ok(false)` for a duplicate of a result this device already gave.
    pub fn complete_job(
        &self,
        device: &Device,
        id: &str,
        outcome: Result<Value, DeviceError>,
    ) -> Result<bool, ResultError> {
        let mut inner = self.lock();
        if let Some(job) = inner.jobs.get(id) {
            if job.connection != device.connection || job.generation != device.generation {
                return Err(ResultError::Unknown);
            }
            if !matches!(job.state, JobState::Claimed { .. }) {
                return Err(ResultError::NotClaimed);
            }
            let job = inner.take(id).expect("present");
            inner.finish(id, &job.connection, job.generation, Finished::Completed);
            let _ = job.tx.send(outcome);
            return Ok(true);
        }
        match inner.finished.get(id) {
            Some((c, g, how)) if *c == device.connection && *g == device.generation => match how {
                Finished::Completed => Ok(false),
                Finished::Expired => Err(ResultError::Expired),
            },
            _ => Err(ResultError::Unknown),
        }
    }

    /// Whether the device serving `connection` polled recently.
    pub fn status(&self, connection: &str) -> DeviceStatus {
        let inner = self.lock();
        let seen = inner
            .conns
            .get(connection)
            .and_then(|q| q.last_seen.filter(|(_, g)| *g == q.generation));
        DeviceStatus {
            online: seen.is_some_and(|(at, _)| at.elapsed() <= self.limits.offline_after),
            last_seen_secs: seen.map(|(at, _)| at.elapsed().as_secs()),
        }
    }

    /// A live current-generation heartbeat, and its optional local allowlist.
    pub(crate) fn chat_inventory(&self, connection: &str) -> Option<(u64, Option<Vec<String>>)> {
        let inner = self.lock();
        let q = inner.conns.get(connection)?;
        let (seen, generation) = q.last_seen?;
        (generation == q.generation && seen.elapsed() <= self.limits.offline_after)
            .then(|| (generation, q.approved_models.clone()))
    }

    fn report_inventory(&self, device: &Device, approved_models: Option<Vec<String>>) {
        let mut inner = self.lock();
        if inner.observe(&device.connection, device.generation)
            && let Some(q) = inner.conns.get_mut(&device.connection)
        {
            q.approved_models = approved_models;
            q.last_seen = Some((Instant::now(), device.generation));
        }
    }

    /// Issue (or rotate) the device token of `owner`'s `connection`. The
    /// token is returned once and only its hash is stored.
    pub async fn issue_token(
        &self,
        db: &Db,
        owner: &str,
        connection: &str,
    ) -> Result<String, IssueError> {
        let token = new_device_token().map_err(IssueError::Internal)?;
        let hash = device_token_hash(&token);
        let directory = self.directory.clone();
        let (o, c) = (owner.to_owned(), connection.to_owned());
        let generation = db
            .call(move |db| {
                let conn = db.lock();
                match lookup(&*directory, &conn, &c)? {
                    Some(dc) if dc.id == c && dc.owner_id == o => {
                        if !takes_device_token(&dc.provider) {
                            return Ok(Err(IssueError::NotDevice));
                        }
                    }
                    _ => return Ok(Err(IssueError::NotFound)),
                }
                Ok(Ok(store_credential(&conn, &c, &o, &hash)?))
            })
            .await
            .map_err(IssueError::Internal)??;
        self.retire(connection, generation);
        Ok(token)
    }

    /// Revoke the device token of `owner`'s `connection`.
    pub async fn revoke_token(
        &self,
        db: &Db,
        owner: &str,
        connection: &str,
    ) -> Result<(), IssueError> {
        let directory = self.directory.clone();
        let (o, c) = (owner.to_owned(), connection.to_owned());
        let generation = db
            .call(move |db| {
                let conn = db.lock();
                if owned_device_connection(&*directory, &conn, &o, &c)?.is_none() {
                    return Ok(Err(IssueError::NotFound));
                }
                Ok(Ok(delete_credential(&conn, &c)?))
            })
            .await
            .map_err(IssueError::Internal)??;
        // No credential, nothing was revoked: the relay keeps what it has.
        if let Some(g) = generation {
            self.retire(connection, g);
        }
        Ok(())
    }

    /// Publish a committed revocation to the relay: fail every pending job of
    /// `connection` and refuse polls and queueing under any generation
    /// before `generation`, before the caller returns.
    ///
    /// Conditional on the generation, under the relay lock: only jobs and
    /// polls of *older* generations are invalidated. A publication that
    /// arrives late (an older generation than the relay already knows)
    /// changes nothing, so a newer token's queued work survives it.
    pub(crate) fn retire(&self, connection: &str, generation: u64) {
        let mut inner = self.lock();
        let known = inner.conns.get(connection).map_or(0, |q| q.generation);
        if generation > known {
            inner.observe(connection, generation);
        }
    }

    /// The newest token generation the relay knows for `connection`.
    #[cfg(test)]
    pub(crate) fn known_generation(&self, connection: &str) -> u64 {
        self.lock()
            .conns
            .get(connection)
            .map_or(0, |q| q.generation)
    }

    /// Remove `owner`'s web device `id`: its token and its row go in one
    /// transaction, then its pending jobs fail (`CredentialChanged`).
    /// `Ok(false)` when it isn't theirs.
    pub async fn remove_web_device(&self, db: &Db, owner: &str, id: &str) -> anyhow::Result<bool> {
        let (o, i) = (owner.to_owned(), id.to_owned());
        let removed = db
            .call(move |db| {
                let conn = db.lock();
                let tx = conn.unchecked_transaction()?;
                let n = tx.execute(
                    "DELETE FROM web_devices WHERE id = ?1 AND owner_id = ?2",
                    params![i, o],
                )?;
                if n == 0 {
                    return Ok(None);
                }
                let generation = delete_credential_in(&tx, &i)?;
                tx.commit()?;
                Ok(Some(generation))
            })
            .await?;
        match removed {
            None => Ok(false),
            Some(generation) => {
                if let Some(g) = generation {
                    self.retire(id, g);
                }
                Ok(true)
            }
        }
    }

    /// Whether `owner`'s `connection` has a device token; `None` when the
    /// connection isn't theirs.
    async fn paired(&self, db: &Db, owner: &str, connection: &str) -> anyhow::Result<Option<bool>> {
        let directory = self.directory.clone();
        let (o, c) = (owner.to_owned(), connection.to_owned());
        db.call(move |db| {
            let conn = db.lock();
            if owned_device_connection(&*directory, &conn, &o, &c)?.is_none() {
                return Ok(None);
            }
            Ok(Some(
                credential_of(&conn, &c)?.is_some_and(|(cred_owner, _)| cred_owner == o),
            ))
        })
        .await
    }

    /// Clean up after the providers lane deleted `connection`: drop its
    /// token and fail its pending jobs. (Device requests already stop
    /// authenticating once the connection is gone.)
    pub async fn connection_removed(&self, db: &Db, connection: &str) -> anyhow::Result<()> {
        let c = connection.to_owned();
        db.call(move |db| {
            let conn = db.lock();
            delete_credential(&conn, &c)?;
            Ok(())
        })
        .await?;
        self.lock().flush(connection);
        Ok(())
    }

    /// Resolve a device bearer token, re-checking the connection.
    async fn authenticate(&self, db: &Db, token: &str) -> anyhow::Result<Option<Device>> {
        let hash = device_token_hash(token);
        let directory = self.directory.clone();
        db.call(move |db| {
            let conn = db.lock();
            let Some((connection, owner, generation)) = credential_by_hash(&conn, &hash)? else {
                return Ok(None);
            };
            match lookup(&*directory, &conn, &connection)? {
                Some(dc)
                    if dc.id == connection
                        && dc.owner_id == owner
                        && takes_device_token(&dc.provider) =>
                {
                    Ok(Some(Device {
                        connection,
                        owner,
                        generation,
                    }))
                }
                _ => Ok(None),
            }
        })
        .await
    }

    /// The typed Rig adapter for agent runs: Rig's OpenAI Chat wire, sent
    /// through `owner`'s device on `connection`, always for `model`.
    pub fn rig_model(
        &self,
        db: Db,
        owner: &str,
        connection: &str,
        model: &str,
    ) -> rig_core::Model<Chat, DeviceTransport> {
        self.rig_model_inner(db, Caller::personal(owner), false, connection, model)
    }

    /// [`Self::rig_model`] for `caller`, who may be borrowing `connection`:
    /// every request goes through [`Self::complete_as`], so each one is
    /// granted, claim-checked and result-checked on its own.
    pub fn rig_model_as(
        &self,
        db: Db,
        caller: Caller,
        connection: &str,
        model: &str,
    ) -> rig_core::Model<Chat, DeviceTransport> {
        self.rig_model_inner(db, caller, true, connection, model)
    }

    fn rig_model_inner(
        &self,
        db: Db,
        caller: Caller,
        allow_borrow: bool,
        connection: &str,
        model: &str,
    ) -> rig_core::Model<Chat, DeviceTransport> {
        // The base URL and key are never used: the transport ignores the
        // HTTP request's URI and headers and relays only its JSON body.
        let wire = OpenAIConfig::with_key(&DEVICE_DIALECT, "metro-device-relay")
            .with_base_url(DEVICE_BASE_URL)
            .chat(model.to_owned());
        rig_core::Model::new(
            wire,
            DeviceTransport {
                relay: self.clone(),
                db,
                caller,
                allow_borrow,
                connection: connection.into(),
                model: model.into(),
            },
        )
    }
}

/// Why a token couldn't be issued or revoked.
#[derive(Debug)]
pub enum IssueError {
    NotFound,
    NotDevice,
    Internal(anyhow::Error),
}

impl From<IssueError> for ApiError {
    fn from(e: IssueError) -> Self {
        match e {
            IssueError::NotFound => {
                ApiError::new(StatusCode::NOT_FOUND, "no such provider connection")
            }
            IssueError::NotDevice => ApiError::new(
                StatusCode::CONFLICT,
                "device tokens are only for device-* provider connections",
            ),
            IssueError::Internal(e) => ApiError::internal(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Rig transport.

const DEVICE_BASE_URL: &str = "http://metro-device.invalid/v1";
const DEVICE_DIALECT: Dialect =
    Dialect::gateway("metro-device", DEVICE_BASE_URL, "METRO_DEVICE_UNUSED_KEY");

/// Sends Rig's Chat Completions payloads through a device. Bound to one
/// owner, connection and model; anything else in the request is replaced.
#[derive(Clone)]
pub struct DeviceTransport {
    relay: DeviceRelay,
    db: Db,
    caller: Caller,
    /// `false`: owner only, as [`DeviceRelay::complete`].
    allow_borrow: bool,
    connection: String,
    model: String,
}

impl std::fmt::Debug for DeviceTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceTransport")
            .field("connection", &self.connection)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Transport<Chat> for DeviceTransport {
    fn send(&self, payload: Encoded, exchange: Exchange) -> Opening<WireFrame> {
        let mode = exchange.mode;
        let mut body: Value = match payload.request.body() {
            Body::Bytes(bytes) => match serde_json::from_slice(bytes) {
                Ok(v) => v,
                Err(_) => {
                    return Opening::failed(ProviderError::request(
                        "the device relay could not read the request body",
                    ));
                }
            },
            Body::Multipart(_) => {
                return Opening::failed(ProviderError::request(
                    "the device relay sends no multipart requests",
                ));
            }
        };
        if let Some(object) = body.as_object_mut() {
            object.insert("model".into(), Value::String(self.model.clone()));
        }
        let this = self.clone();
        Opening::new(async move {
            let response = if this.allow_borrow {
                this.relay
                    .complete_as(&this.db, &this.caller, &this.connection, body)
                    .await
            } else {
                this.relay
                    .complete(&this.db, &this.caller.user, &this.connection, body)
                    .await
            }
            .map_err(|e| ProviderError::Provider(e.to_string()))?;
            Ok(match mode {
                Mode::Unary => {
                    let bytes = serde_json::to_vec(&response).unwrap_or_default();
                    let frames = Framing::Whole.split(&bytes);
                    Opened::new(futures::stream::iter(frames.into_iter().map(Ok)))
                        .with_document(response)
                }
                Mode::Streaming => {
                    let frames = Framing::Sse.split(sse_from_completion(&response).as_bytes());
                    Opened::new(futures::stream::iter(frames.into_iter().map(Ok)))
                }
            })
        })
    }
}

/// A whole `chat.completion` as the event stream a streaming endpoint
/// would have sent: one delta per choice, a finish chunk, usage, `[DONE]`.
fn sse_from_completion(response: &Value) -> String {
    let id = response.get("id").cloned().unwrap_or(json!("device"));
    let model = response.get("model").cloned().unwrap_or(Value::Null);
    let created = response.get("created").cloned().unwrap_or(json!(0));
    let chunk = |choices: Value, usage: Option<&Value>| {
        let mut c = json!({
            "id": id, "object": "chat.completion.chunk", "created": created,
            "model": model, "choices": choices,
        });
        if let Some(u) = usage {
            c["usage"] = u.clone();
        }
        format!("data: {c}\n\n")
    };
    let mut out = String::new();
    let choices = response
        .get("choices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (n, choice) in choices.iter().enumerate() {
        let index = choice.get("index").cloned().unwrap_or(json!(n));
        let message = choice.get("message").cloned().unwrap_or(json!({}));
        let mut delta = serde_json::Map::new();
        delta.insert("role".into(), json!("assistant"));
        for key in ["content", "refusal", "reasoning_content", "reasoning"] {
            if let Some(v) = message.get(key).filter(|v| !v.is_null()) {
                delta.insert(key.into(), v.clone());
            }
        }
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            let calls: Vec<Value> = calls
                .iter()
                .enumerate()
                .map(|(i, call)| {
                    let mut call = call.clone();
                    if let Some(o) = call.as_object_mut() {
                        o.entry("index").or_insert(json!(i));
                    }
                    call
                })
                .collect();
            delta.insert("tool_calls".into(), Value::Array(calls));
        }
        out.push_str(&chunk(
            json!([{"index": index, "delta": delta, "finish_reason": null}]),
            None,
        ));
        let finish = choice
            .get("finish_reason")
            .cloned()
            .filter(|v| !v.is_null())
            .unwrap_or(json!("stop"));
        out.push_str(&chunk(
            json!([{"index": index, "delta": {}, "finish_reason": finish}]),
            None,
        ));
    }
    if let Some(usage) = response.get("usage").filter(|u| !u.is_null()) {
        out.push_str(&chunk(json!([]), Some(usage)));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

// ---------------------------------------------------------------------------
// HTTP.

/// The bearer token of a request, if it carries exactly one.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty() && token.len() <= MAX_TOKEN_LEN).then_some(token)
}

fn well_formed(token: &str) -> bool {
    token.strip_prefix(DEVICE_TOKEN_PREFIX).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    })
}

impl FromRequestParts<AppState> for Device {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = bearer(&parts.headers)
            .filter(|t| well_formed(t))
            .ok_or_else(ApiError::unauthorized)?;
        state
            .device
            .authenticate(&state.db, token)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(ApiError::unauthorized)
    }
}

/// An empty body or `{}`; anything else is refused.
fn empty_object(body: &Bytes) -> Result<(), ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(o)) if o.is_empty() => Ok(()),
        _ => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the body must be {}",
        )),
    }
}

/// Web devices have their own routes (`/v1/web-devices`); the provider
/// routes don't know them.
fn not_web_device(id: &str) -> Result<(), ApiError> {
    if crate::web_devices::is_web_device_id(id) {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "no such provider connection",
        ));
    }
    Ok(())
}

/// `POST /v1/providers/{id}/device-token`: issue or rotate. Owner only.
pub async fn issue_device_token(
    State(state): State<AppState>,
    human: Human,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    empty_object(&body)?;
    not_web_device(&id)?;
    let token = state.device.issue_token(&state.db, &human.id, &id).await?;
    tracing::info!(connection = %id, "device token issued");
    Ok((StatusCode::CREATED, Json(json!({ "token": token }))).into_response())
}

/// `DELETE /v1/providers/{id}/device-token`: revoke. Owner only.
pub async fn revoke_device_token(
    State(state): State<AppState>,
    human: Human,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    not_web_device(&id)?;
    state.device.revoke_token(&state.db, &human.id, &id).await?;
    tracing::info!(connection = %id, "device token revoked");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/providers/{id}/device-token`: pairing and online state.
pub async fn device_token_status(
    State(state): State<AppState>,
    human: Human,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    not_web_device(&id)?;
    let paired = state
        .device
        .paired(&state.db, &human.id, &id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "no such provider connection"))?;
    let status = state.device.status(&id);
    Ok(Json(json!({
        "paired": paired,
        "online": paired && status.online,
        "last_seen_secs": if paired { json!(status.last_seen_secs) } else { Value::Null },
    })))
}

/// `POST /v1/device/claim`.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DevicePoll {
    approved_models: Option<Vec<String>>,
}

pub async fn device_claim(
    State(state): State<AppState>,
    device: Device,
    body: Bytes,
) -> Result<Response, ApiError> {
    let invalid = || {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the body must be {} or contain a bounded approved_models array",
        )
    };
    let poll = match body.iter().copied().find(|b| !b.is_ascii_whitespace()) {
        None => DevicePoll::default(),
        Some(b'{') => serde_json::from_slice::<DevicePoll>(&body).map_err(|_| invalid())?,
        Some(_) => return Err(invalid()),
    };
    let approved_models = poll
        .approved_models
        .map(|mut ids| {
            if ids.len() > MAX_APPROVED_MODELS
                || ids.iter().any(|id| {
                    id.is_empty()
                        || id.len() > MAX_MODEL_ID
                        || !crate::providers::api::check_model(id)
                            .is_ok_and(|checked| checked == *id)
                })
            {
                return Err(invalid());
            }
            ids.sort();
            ids.dedup();
            Ok(ids)
        })
        .transpose()?;
    state.device.report_inventory(&device, approved_models);
    Ok(match state.device.claim_checked(&state.db, &device).await {
        Some(job) => (StatusCode::OK, Json(job)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceResult {
    ok: bool,
    #[serde(default)]
    response: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

/// A device's error text, bounded and stripped of control characters.
fn sanitize_error(e: &str) -> String {
    let mut out: String = e
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if out.len() > MAX_DEVICE_ERROR {
        let mut end = MAX_DEVICE_ERROR;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    let out = out.trim().to_owned();
    if out.is_empty() {
        "the request failed".into()
    } else {
        out
    }
}

/// Check a successful response's shape for `operation`.
fn validate_response(operation: Operation, response: &Value) -> Result<(), String> {
    match operation {
        Operation::Models => {
            let models = response
                .get("models")
                .and_then(Value::as_array)
                .ok_or("expected {\"models\":[string]}")?;
            if models.len() > MAX_MODELS {
                return Err("too many models".into());
            }
            if !models.iter().all(|m| {
                m.as_str().is_some_and(|s| {
                    !s.is_empty() && s.len() <= MAX_MODEL_ID && !s.chars().any(char::is_control)
                })
            }) {
                return Err("model ids must be short non-empty strings".into());
            }
            Ok(())
        }
        Operation::Complete => {
            let choices = response
                .get("choices")
                .and_then(Value::as_array)
                .ok_or("expected an OpenAI chat.completion object with choices")?;
            if choices.is_empty()
                || !choices
                    .iter()
                    .all(|c| c.get("message").is_some_and(Value::is_object))
            {
                return Err("every choice needs a message object".into());
            }
            Ok(())
        }
        // Checked in full against the request by the waiting caller
        // (`DeviceRelay::decide` / `embed`); here only the outer shape.
        Operation::Decide => match response.get("answers") {
            Some(Value::Object(_)) => Ok(()),
            _ => Err("expected a System One response with an answers object".into()),
        },
        Operation::Embed => match response.get("data") {
            Some(Value::Array(_)) => Ok(()),
            _ => Err("expected an embeddings response with a data array".into()),
        },
        Operation::MediaEmbed => match (response.get("data"), response.get("partial")) {
            (Some(Value::Array(_)), Some(Value::Bool(_))) => Ok(()),
            _ => Err("expected a media embeddings response with data and partial".into()),
        },
        // Normalized in full by `crate::web_tools`; here only the envelope.
        Operation::WebSearch | Operation::WebFetch => {
            match (response.get("v"), response.get("attempts")) {
                (Some(v), Some(Value::Array(_))) if v == 1 => Ok(()),
                _ => Err("expected a web response {\"v\":1,\"attempts\":[..]}".into()),
            }
        }
    }
}

/// `POST /v1/device/jobs/{id}/result`.
pub async fn device_result(
    State(state): State<AppState>,
    device: Device,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let relay = &state.device;
    let max = relay.limits.max_response_bytes;
    // The operation decides validation; look it up without holding the lock
    // across parsing.
    let operation = relay
        .lock()
        .jobs
        .get(&id)
        .filter(|j| j.connection == device.connection && j.generation == device.generation)
        .map(|j| j.operation);
    let parsed: DeviceResult = serde_json::from_slice(&body).map_err(|_| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "expected {\"ok\":true,\"response\":{..}} or {\"ok\":false,\"error\":\"..\"}",
        )
    })?;
    let mut reply_error = None;
    let outcome = match (parsed.ok, parsed.response, parsed.error) {
        (true, Some(response), None) if response.is_object() => {
            if body.len() > max + RESULT_ENVELOPE {
                reply_error = Some(ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "response too large",
                ));
                Err(DeviceError::TooLarge)
            } else {
                match operation.map(|op| validate_response(op, &response)) {
                    Some(Err(why)) => {
                        reply_error =
                            Some(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, why.clone()));
                        Err(DeviceError::BadResponse(why))
                    }
                    _ => Ok(response),
                }
            }
        }
        (false, None, Some(error)) => Err(DeviceError::Device(sanitize_error(&error))),
        _ => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "expected {\"ok\":true,\"response\":{..}} or {\"ok\":false,\"error\":\"..\"}",
            ));
        }
    };
    match relay.complete_job(&device, &id, outcome) {
        Ok(_) => match reply_error {
            // The job failed with the reason; tell the device why.
            Some(e) => Err(e),
            None => Ok(StatusCode::NO_CONTENT),
        },
        Err(ResultError::Unknown) => Err(ApiError::new(StatusCode::NOT_FOUND, "no such job")),
        Err(ResultError::NotClaimed) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "this job hasn't been claimed by this device",
        )),
        Err(ResultError::Expired) => Err(ApiError::new(
            StatusCode::GONE,
            "the job expired or was cancelled before the result arrived",
        )),
    }
}

/// Device relay routes, with full paths. Merge under the human body limit:
/// only the result route raises it, to the response bound.
pub fn router() -> Router<AppState> {
    let result_limit = DeviceLimits::default().max_response_bytes + RESULT_ENVELOPE;
    Router::new()
        .route(
            "/v1/providers/{id}/device-token",
            post(issue_device_token)
                .delete(revoke_device_token)
                .get(device_token_status),
        )
        .route("/v1/device/claim", post(device_claim))
        .route(
            "/v1/device/jobs/{id}/result",
            post(device_result).layer(DefaultBodyLimit::max(result_limit)),
        )
}

#[cfg(test)]
mod tests;
