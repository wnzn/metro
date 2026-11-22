//! Managed local user accounts, provisioned by organization admins.
//!
//! Only in the `dev` identity namespace. OIDC servers never read or issue
//! managed accounts: their admin routes answer 404 and managed bearer
//! tokens are never looked up.
//!
//! - **Stable ids are server-generated** and immutable: `mu:` followed by
//!   128 random bits as hex. `:` is not allowed in a `METRO_DEV_USERS` name,
//!   so a managed id can never equal an env user. A fresh id is also checked
//!   against every user id the database has ever stored (members, owners,
//!   authors, mentions, reactors, uploaders, runner and provider owners), so
//!   it never inherits history, membership or ownership.
//! - **Display names are separate and editable.** A name may not equal
//!   (ignoring case) a reserved actor, an env user, an org admin, another
//!   managed user, or any author or id already in the history, so nobody
//!   can pose as someone else in a room.
//! - **Tokens** are `metro_ut_` plus 256 random bits as hex, shown once in
//!   a `no-store` response and stored only as a SHA-256 hash. Reissue
//!   replaces the hash in one statement; revoke clears it. Every request
//!   reads the hash from the database, so both take effect immediately.
//! - Managed users are ordinary people: no admin rights (startup refuses a
//!   managed id in `METRO_ORG_ADMINS`), and they see a Space only once its
//!   owner adds them. An owner can't add an unknown `mu:` id ahead of time.
//! - Accounts are never deleted, so an id is never reused.
//!
//! The admin routes are `GET|POST /v1/admin/users`,
//! `PATCH /v1/admin/users/{id}` and `POST|DELETE /v1/admin/users/{id}/token`.
//! The static admin panel is served unauthenticated at `/admin`; it holds
//! no data and does nothing without an admin's bearer token.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, patch, post};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody};
use crate::auth::{Human, runner_token_hash};
use crate::db::{Db, now_ms};

/// Every managed user id starts with this.
pub const MANAGED_ID_PREFIX: &str = "mu:";
/// Every managed user's bearer token starts with this.
pub const MANAGED_TOKEN_PREFIX: &str = "metro_ut_";
/// Longest display name, in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// Most managed accounts on one server.
pub const MAX_MANAGED_USERS: usize = 1000;
/// The only namespace with managed accounts.
const DEV_NAMESPACE: &str = "dev";

const NO_USER: &str = "no such managed user";
const NOT_AVAILABLE: &str = "managed users are available only with dev token authentication";

pub(crate) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS managed_users (
             id              TEXT PRIMARY KEY NOT NULL,
             name            TEXT NOT NULL,
             token_hash      BLOB UNIQUE,
             created_by      TEXT NOT NULL,
             created_at      INTEGER NOT NULL,
             token_issued_at INTEGER
         );
         CREATE UNIQUE INDEX IF NOT EXISTS managed_users_name
             ON managed_users (name COLLATE NOCASE);",
    )
}

/// What the server knows about managed accounts at startup.
#[derive(Clone, Debug, Default)]
pub struct Managed {
    /// True only in the `dev` namespace.
    enabled: bool,
    /// The `METRO_DEV_USERS` names (never their tokens): ids no managed
    /// display name may take.
    env_ids: Arc<Vec<String>>,
}

impl Managed {
    /// For `namespace`, with env user names from `METRO_DEV_USERS`.
    pub fn from_env(namespace: &str) -> Self {
        let env_ids = if namespace == DEV_NAMESPACE {
            std::env::var(metro_auth::env::DEV_USERS)
                .ok()
                .map(|raw| env_user_names(&raw))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Self::new(namespace, env_ids)
    }

    pub fn new(namespace: &str, env_ids: Vec<String>) -> Self {
        Self {
            enabled: namespace == DEV_NAMESPACE,
            env_ids: Arc::new(env_ids),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether `name` equals (ignoring case) a `METRO_DEV_USERS` name.
    pub fn env_ids_contain(&self, name: &str) -> bool {
        self.env_ids.iter().any(|e| e.eq_ignore_ascii_case(name))
    }
}

/// The keys of a `METRO_DEV_USERS` JSON object. Values are skipped
/// unread, so no token is ever held here.
fn env_user_names(raw: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Whether `id` has the managed form. Only meaningful in the dev namespace.
pub fn is_managed_id(id: &str) -> bool {
    id.strip_prefix(MANAGED_ID_PREFIX).is_some_and(|rest| {
        rest.len() == 32 && rest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn new_id() -> anyhow::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no system randomness: {e}"))?;
    let mut id = String::from(MANAGED_ID_PREFIX);
    for b in bytes {
        id.push_str(&format!("{b:02x}"));
    }
    Ok(id)
}

/// A fresh managed user token.
pub fn new_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no system randomness: {e}"))?;
    let mut token = String::from(MANAGED_TOKEN_PREFIX);
    for b in bytes {
        token.push_str(&format!("{b:02x}"));
    }
    Ok(token)
}

/// Invisible characters that let one name pose as another.
pub(crate) fn deceptive(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}' | '\u{00AD}')
}

/// A usable display name, trimmed, or why not.
pub(crate) fn check_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(|c| c.is_control() || deceptive(c))
    {
        return Err(ApiError::bad_request(format!(
            "name must be 1..={MAX_NAME_CHARS} printable characters"
        )));
    }
    Ok(name.to_owned())
}

/// Columns, in any table, that hold a person's stable id.
const ID_COLUMNS: &[&str] = &[
    "user_id",
    "owner_id",
    "actor_id",
    "reactor_id",
    "uploader_id",
    "attached_by",
    "created_by",
    "creator_id",
    "author_id",
];
/// Columns that hold a person's display name: authored history and
/// profiles (`managed_users.name` is checked on its own).
const NAME_COLUMNS: &[&str] = &["actor", "display_name", "default_name"];

/// `(table, column)` for every listed column present in the schema.
fn columns(tx: &Connection, wanted: &[&str]) -> rusqlite::Result<Vec<(String, String)>> {
    let mut tables = tx.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let tables: Vec<String> = tables
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for table in tables {
        let mut info = tx.prepare(&format!(
            "PRAGMA table_info(\"{}\")",
            table.replace('"', "\"\"")
        ))?;
        let cols: Vec<String> = info
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        for col in cols {
            if wanted.contains(&col.as_str()) {
                out.push((table.clone(), col));
            }
        }
    }
    Ok(out)
}

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// The column naming whose row this is, for rows a person may keep their
/// own name or id in: authored events, profiles.
fn row_owner(tx: &Connection, table: &str) -> rusqlite::Result<Option<&'static str>> {
    let mut info = tx.prepare(&format!("PRAGMA table_info({})", quote(table)))?;
    let cols: Vec<String> = info
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<_, _>>()?;
    Ok(["actor_id", "user_id"]
        .into_iter()
        .find(|c| cols.iter().any(|x| x == c)))
}

/// Whether `value` appears (ignoring case) in any of `cols`. With
/// `except_id`, values in rows that person owns, and a column value equal
/// to their own id, don't count.
fn appears(
    tx: &Connection,
    cols: &[(String, String)],
    value: &str,
    except_id: Option<&str>,
) -> rusqlite::Result<bool> {
    for (table, col) in cols {
        let c = quote(col);
        let mut sql = format!(
            "SELECT 1 FROM {t} WHERE CAST({c} AS TEXT) = ?1 COLLATE NOCASE",
            t = quote(table),
        );
        if except_id.is_some() {
            sql.push_str(&format!(" AND CAST({c} AS TEXT) != ?2"));
            if let Some(owner) = row_owner(tx, table)? {
                let o = quote(owner);
                sql.push_str(&format!(" AND ({o} IS NULL OR {o} != ?2)"));
            }
        }
        sql.push_str(" LIMIT 1");
        let found = match except_id {
            Some(id) => tx.query_row(&sql, params![value, id], |_| Ok(())),
            None => tx.query_row(&sql, [value], |_| Ok(())),
        }
        .optional()?;
        if found.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `id` was ever recorded anywhere as a person: as a column value,
/// a mentioned recipient, or a managed account.
fn id_ever_used(tx: &Connection, id: &str) -> rusqlite::Result<bool> {
    if appears(tx, &columns(tx, ID_COLUMNS)?, id, None)? {
        return Ok(true);
    }
    for (table, col) in columns(tx, &["mentions"])? {
        let sql = format!(
            "SELECT 1 FROM {t} WHERE instr(lower({c}), lower(?1)) > 0 LIMIT 1",
            t = quote(&table),
            c = quote(&col),
        );
        if tx.query_row(&sql, [id], |_| Ok(())).optional()?.is_some() {
            return Ok(true);
        }
    }
    Ok(tx
        .query_row(
            "SELECT 1 FROM managed_users WHERE id = ?1 COLLATE NOCASE",
            [id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Whether a display name is reserved or already someone else's: a
/// reserved actor, an id-shaped name, an env user, an org admin, a managed
/// account's name, anyone's stable id, a profile name, or a name already
/// shown on someone else's messages. `self_id` is the person who would
/// take the name; their own id and names stay available to them.
pub(crate) fn name_clash(
    tx: &Connection,
    managed: &Managed,
    org_admins: &[String],
    name: &str,
    self_id: Option<&str>,
) -> rusqlite::Result<bool> {
    use crate::db::actor;
    let lower = name.to_lowercase();
    let mine = |other: &str| self_id == Some(other);
    let reserved = [actor::AGENT, actor::RUNNER, actor::SYSTEM, "admin", "metro"];
    if reserved.contains(&lower.as_str())
        || lower.starts_with(MANAGED_ID_PREFIX)
        || lower.starts_with("user:")
        || managed
            .env_ids
            .iter()
            .any(|e| !mine(e) && e.to_lowercase() == lower)
        || org_admins
            .iter()
            .any(|a| !mine(a) && a.to_lowercase() == lower)
    {
        return Ok(true);
    }
    let other_managed = tx
        .query_row(
            "SELECT 1 FROM managed_users WHERE name = ?1 COLLATE NOCASE AND id != ?2",
            params![name, self_id.unwrap_or("")],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if other_managed {
        return Ok(true);
    }
    Ok(appears(tx, &columns(tx, ID_COLUMNS)?, name, self_id)?
        || appears(tx, &columns(tx, NAME_COLUMNS)?, name, self_id)?)
}

// ---- Database ----

/// One managed account as admins see it.
#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct ManagedUser {
    pub id: String,
    pub name: String,
    pub has_token: bool,
}

pub enum CreateError {
    NameTaken,
    TooMany,
    Other(anyhow::Error),
}

impl From<rusqlite::Error> for CreateError {
    fn from(e: rusqlite::Error) -> Self {
        CreateError::Other(e.into())
    }
}

impl From<anyhow::Error> for CreateError {
    fn from(e: anyhow::Error) -> Self {
        CreateError::Other(e)
    }
}

impl Db {
    /// The managed account a token belongs to: `(id, name)`.
    pub fn managed_user_by_token_hash(
        &self,
        hash: &[u8],
    ) -> anyhow::Result<Option<(String, String)>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT id, name FROM managed_users WHERE token_hash = ?1",
                [hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
    }

    pub fn managed_user_exists(&self, id: &str) -> anyhow::Result<bool> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT 1 FROM managed_users WHERE id = ?1",
                [id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn list_managed_users(&self) -> anyhow::Result<Vec<ManagedUser>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, token_hash IS NOT NULL FROM managed_users ORDER BY created_at, id",
        )?;
        let users = stmt
            .query_map([], |r| {
                Ok(ManagedUser {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    has_token: r.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(users)
    }

    /// Create an account with a fresh id and token; returns `(id, token)`.
    pub fn create_managed_user(
        &self,
        managed: &Managed,
        org_admins: &[String],
        name: &str,
        created_by: &str,
    ) -> Result<(String, String), CreateError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM managed_users", [], |r| r.get(0))?;
        if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_MANAGED_USERS {
            return Err(CreateError::TooMany);
        }
        if name_clash(&tx, managed, org_admins, name, None)? {
            return Err(CreateError::NameTaken);
        }
        let id = fresh_id(&tx, org_admins)?;
        let token = new_token()?;
        let now = now_ms();
        tx.execute(
            "INSERT INTO managed_users (id, name, token_hash, created_by, created_at, token_issued_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, name, runner_token_hash(&token), created_by, now],
        )?;
        tx.commit()?;
        Ok((id, token))
    }

    /// Rename an account. `Ok(None)` when there is no such account.
    pub fn rename_managed_user(
        &self,
        managed: &Managed,
        org_admins: &[String],
        id: &str,
        name: &str,
    ) -> Result<Option<ManagedUser>, CreateError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let has_token: Option<bool> = tx
            .query_row(
                "SELECT token_hash IS NOT NULL FROM managed_users WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(has_token) = has_token else {
            return Ok(None);
        };
        if name_clash(&tx, managed, org_admins, name, Some(id))? {
            return Err(CreateError::NameTaken);
        }
        tx.execute(
            "UPDATE managed_users SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        tx.commit()?;
        Ok(Some(ManagedUser {
            id: id.to_owned(),
            name: name.to_owned(),
            has_token,
        }))
    }

    /// Replace an account's token in one statement. `Ok(None)` when there
    /// is no such account.
    pub fn reissue_managed_token(&self, id: &str) -> anyhow::Result<Option<String>> {
        let token = new_token()?;
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE managed_users SET token_hash = ?2, token_issued_at = ?3 WHERE id = ?1",
            params![id, runner_token_hash(&token), now_ms()],
        )?;
        Ok((n == 1).then_some(token))
    }

    /// Clear an account's token. False when there is no such account.
    pub fn revoke_managed_token(&self, id: &str) -> anyhow::Result<bool> {
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE managed_users SET token_hash = NULL, token_issued_at = NULL WHERE id = ?1",
            [id],
        )?;
        Ok(n == 1)
    }
}

/// A new id that no one has ever had.
fn fresh_id(tx: &Transaction<'_>, org_admins: &[String]) -> Result<String, CreateError> {
    for _ in 0..8 {
        let id = new_id()?;
        if !org_admins.iter().any(|a| a.eq_ignore_ascii_case(&id)) && !id_ever_used(tx, &id)? {
            return Ok(id);
        }
    }
    Err(CreateError::Other(anyhow::anyhow!(
        "could not generate an unused managed user id"
    )))
}

// ---- HTTP ----

/// 403 unless `who` is an env org admin; 404 outside the dev namespace.
fn require_admin(state: &AppState, who: &Human) -> ApiResult<()> {
    if is_managed_id(&who.id) || !state.config.is_org_admin(&who.id) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "only an organization admin can manage users",
        ));
    }
    if !state.managed.enabled() {
        return Err(ApiError::not_found(NOT_AVAILABLE));
    }
    Ok(())
}

/// The managed id in a path, or 404.
fn path_id(id: &str) -> ApiResult<&str> {
    if is_managed_id(id) {
        Ok(id)
    } else {
        Err(ApiError::not_found(NO_USER))
    }
}

/// Admin responses are never cached; some carry a token.
fn private(status: StatusCode, body: serde_json::Value) -> Response {
    let mut response = crate::api::no_store(status, body);
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn create_error(e: CreateError) -> ApiError {
    match e {
        CreateError::NameTaken => ApiError::new(
            StatusCode::CONFLICT,
            "that name is already used or reserved; choose another",
        ),
        CreateError::TooMany => ApiError::new(
            StatusCode::CONFLICT,
            format!("this server already has {MAX_MANAGED_USERS} managed users"),
        ),
        CreateError::Other(e) => ApiError::internal(e),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NameBody {
    name: String,
}

pub async fn list_users(State(state): State<AppState>, who: Human) -> ApiResult<Response> {
    require_admin(&state, &who)?;
    let users = crate::api::db(&state, |db| db.list_managed_users()).await?;
    Ok(private(StatusCode::OK, json!({"users": users})))
}

pub async fn create_user(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<NameBody>,
) -> ApiResult<Response> {
    require_admin(&state, &who)?;
    let Json(body) = body?;
    let name = check_name(&body.name)?;
    let (managed, admins, by, n) = (
        state.managed.clone(),
        state.config.org_admins.clone(),
        who.id.clone(),
        name.clone(),
    );
    let created = state
        .db
        .call(move |db| Ok(db.create_managed_user(&managed, &admins, &n, &by)))
        .await
        .map_err(ApiError::internal)?;
    let (id, token) = created.map_err(create_error)?;
    tracing::info!(user = %id, "managed user created");
    Ok(private(
        StatusCode::CREATED,
        json!({"id": id, "name": name, "token": token}),
    ))
}

pub async fn rename_user(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: JsonBody<NameBody>,
) -> ApiResult<Response> {
    require_admin(&state, &who)?;
    let id = path_id(&id)?.to_owned();
    let Json(body) = body?;
    let name = check_name(&body.name)?;
    let (managed, admins) = (state.managed.clone(), state.config.org_admins.clone());
    let renamed = state
        .db
        .call(move |db| Ok(db.rename_managed_user(&managed, &admins, &id, &name)))
        .await
        .map_err(ApiError::internal)?
        .map_err(create_error)?;
    match renamed {
        Some(user) => Ok(private(StatusCode::OK, json!(user))),
        None => Err(ApiError::not_found(NO_USER)),
    }
}

pub async fn reissue_token(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    require_admin(&state, &who)?;
    let id = path_id(&id)?.to_owned();
    let uid = id.clone();
    match crate::api::db(&state, move |db| db.reissue_managed_token(&uid)).await? {
        Some(token) => {
            tracing::info!(user = %id, "managed user token reissued");
            Ok(private(StatusCode::OK, json!({"token": token})))
        }
        None => Err(ApiError::not_found(NO_USER)),
    }
}

pub async fn revoke_token(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    require_admin(&state, &who)?;
    let id = path_id(&id)?.to_owned();
    let uid = id.clone();
    if crate::api::db(&state, move |db| db.revoke_managed_token(&uid)).await? {
        tracing::info!(user = %id, "managed user token revoked");
        let mut response = StatusCode::NO_CONTENT.into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        Ok(response)
    } else {
        Err(ApiError::not_found(NO_USER))
    }
}

// ---- Static admin panel ----

const INDEX_HTML: &str = include_str!("admin_web/index.html");
const ADMIN_CSS: &str = include_str!("admin_web/admin.css");
const ADMIN_JS: &str = include_str!("admin_web/admin.js");

/// Same-origin scripts and styles only, no framing, no form posts.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn asset(content_type: &'static str, body: &'static str) -> Response {
    let mut response = body.into_response();
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    response
}

async fn index() -> Response {
    asset("text/html; charset=utf-8", INDEX_HTML)
}

async fn css() -> Response {
    asset("text/css; charset=utf-8", ADMIN_CSS)
}

async fn js() -> Response {
    asset("text/javascript; charset=utf-8", ADMIN_JS)
}

async fn to_admin() -> Redirect {
    Redirect::to("/admin")
}

/// The admin API (human body limit applied by the caller).
pub fn api_router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/users", get(list_users).post(create_user))
        .route("/v1/admin/users/{id}", patch(rename_user))
        .route(
            "/v1/admin/users/{id}/token",
            post(reissue_token).delete(revoke_token),
        )
}

/// The unauthenticated, data-free admin shell.
pub fn web_router() -> Router<AppState> {
    Router::new()
        .route("/", get(to_admin))
        .route("/admin", get(index))
        .route("/admin/", get(to_admin))
        .route("/admin/assets/admin.css", get(css))
        .route("/admin/assets/admin.js", get(js))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_tokens_have_distinct_forms() {
        let a = new_id().unwrap();
        let b = new_id().unwrap();
        assert_ne!(a, b);
        assert!(is_managed_id(&a));
        assert!(crate::auth::valid_id(&a));
        // `:` is not a valid METRO_DEV_USERS name character.
        assert!(a.contains(':'));
        assert!(!is_managed_id("mu:abc"));
        assert!(!is_managed_id("jm"));
        assert!(!is_managed_id(&a.to_uppercase()));
        let t = new_token().unwrap();
        assert!(t.starts_with(MANAGED_TOKEN_PREFIX));
        assert_eq!(t.len(), MANAGED_TOKEN_PREFIX.len() + 64);
        assert!(!t.starts_with(crate::auth::RUNNER_TOKEN_PREFIX));
        assert!(!t.starts_with(crate::auth::DEVICE_TOKEN_PREFIX));
    }

    #[test]
    fn env_names_only() {
        assert_eq!(
            env_user_names(r#"{"jm":"dev-token-aaaaaaaaaa","bob":"x"}"#),
            vec!["bob".to_owned(), "jm".to_owned()]
        );
        assert!(env_user_names("nope").is_empty());
    }

    #[test]
    fn history_blocks_ids_and_names() {
        let db = Db::open_in_memory().unwrap();
        let dormant = format!("mu:{}", "d".repeat(32));
        let mentioned = format!("mu:{}", "e".repeat(32));
        let space = db.create_space("olduser", "Old", 10).unwrap();
        db.add_member(&space.id, &dormant, 10).unwrap();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO personal_events (user_id, kind, actor, actor_id, text, mentions, created_at)
                 VALUES ('olduser', 'message', 'Old Name', 'olduser', 'hi', ?1, 0)",
                [format!(r#"[{{"kind":"user","id":"{mentioned}"}}]"#)],
            )
            .unwrap();
        }
        let conn = db.lock();
        assert!(id_ever_used(&conn, &dormant).unwrap());
        assert!(id_ever_used(&conn, &dormant.to_uppercase()).unwrap());
        assert!(id_ever_used(&conn, &mentioned).unwrap());
        assert!(id_ever_used(&conn, "olduser").unwrap());
        assert!(!id_ever_used(&conn, &format!("mu:{}", "f".repeat(32))).unwrap());
        let m = Managed::new("dev", vec!["jm".into()]);
        assert!(name_clash(&conn, &m, &[], "old name", None).unwrap());
        assert!(name_clash(&conn, &m, &[], "OLDUSER", None).unwrap());
        assert!(name_clash(&conn, &m, &[], "Jm", None).unwrap());
        assert!(name_clash(&conn, &m, &["root".into()], "ROOT", None).unwrap());
        assert!(!name_clash(&conn, &m, &[], "Fresh", None).unwrap());
        // A name someone wrote under their own id stays available to them.
        assert!(!name_clash(&conn, &m, &[], "Old Name", Some("olduser")).unwrap());
    }

    #[test]
    fn names_are_checked() {
        assert_eq!(check_name("  Ana  ").ok().as_deref(), Some("Ana"));
        assert!(check_name("").is_err());
        assert!(check_name("a\nb").is_err());
        assert!(check_name("a\u{202E}b").is_err());
        assert!(check_name(&"x".repeat(MAX_NAME_CHARS + 1)).is_err());
    }
}
