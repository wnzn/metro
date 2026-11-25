//! Registered runners: reusable personal and organization runners, and
//! which Space each one serves.
//!
//! A registered runner has a stable opaque id, a name, a visibility and the
//! stable user id of its creator. Its token (`metro_rt_v2_…`) is shown once
//! and stored only as a SHA-256 hash.
//!
//! - A **personal** runner is visible to, managed by, and attachable by its
//!   creator only. To anyone else it does not exist (404).
//! - An **organization** runner can be created and managed (rotated,
//!   deleted) only by the organization admins named in `METRO_ORG_ADMINS`.
//!   Every signed-in user can see it, and any Space owner can attach it.
//!
//! Each Space has at most one runner: either a dedicated (legacy) Space
//! token, or one attached registered runner, never both. Only the Space
//! owner attaches or detaches. Members of the Space can use the runner
//! through the agent and Demo, but never see or manage its token. One
//! registered runner may be attached to many Spaces and keeps a separate
//! workspace for each (`spaces/<space_id>/` on the runner).
//!
//! Every change to who serves a Space bumps the Space's runner generation
//! (see [`crate::jobs`]), so jobs in flight fail rather than cross to the
//! new runner. Deleting a registered runner revokes its token and detaches
//! it from every Space it served.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::api::{self, ApiError, ApiResult, JsonBody, owner_of};
use crate::auth::{Human, new_registered_token, new_runner_token, runner_token_hash};
use crate::db::{Db, LimitError, now_ms, short_id};

pub(crate) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS runners (
             id          TEXT PRIMARY KEY NOT NULL,
             name        TEXT NOT NULL,
             visibility  TEXT NOT NULL CHECK (visibility IN ('personal', 'organization')),
             owner_id    TEXT NOT NULL,
             token_hash  BLOB NOT NULL UNIQUE,
             created_at  INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS runners_owner ON runners (owner_id);
         CREATE TABLE IF NOT EXISTS space_runners (
             space_id    TEXT PRIMARY KEY NOT NULL REFERENCES spaces (id),
             runner_id   TEXT NOT NULL REFERENCES runners (id),
             attached_by TEXT NOT NULL,
             attached_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS space_runners_runner ON space_runners (runner_id);",
    )
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Personal,
    Organization,
}

impl Visibility {
    fn as_str(self) -> &'static str {
        match self {
            Visibility::Personal => "personal",
            Visibility::Organization => "organization",
        }
    }
    fn parse(s: &str) -> rusqlite::Result<Self> {
        match s {
            "personal" => Ok(Visibility::Personal),
            "organization" => Ok(Visibility::Organization),
            other => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown runner visibility {other}").into(),
            )),
        }
    }
}

/// A registered runner as stored (never with its token hash).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredRunner {
    pub id: String,
    pub name: String,
    pub visibility: Visibility,
    /// Stable user id of the creator.
    pub owner_id: String,
}

impl RegisteredRunner {
    /// Whether `user_id` may see and attach it.
    pub fn visible_to(&self, user_id: &str) -> bool {
        self.visibility == Visibility::Organization || self.owner_id == user_id
    }

    /// Whether `user_id` may rotate its token or delete it.
    pub fn manageable_by(&self, user_id: &str, is_org_admin: bool) -> bool {
        match self.visibility {
            Visibility::Personal => self.owner_id == user_id,
            Visibility::Organization => is_org_admin,
        }
    }
}

/// Who serves a Space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpaceRunner {
    None,
    /// A dedicated (legacy) Space token.
    Dedicated,
    Registered(RegisteredRunner),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Attach {
    /// The Space is no longer owned by the caller.
    NoOwnedSpace,
    /// No runner with that id that the caller may attach.
    NoSuchRunner,
    /// The Space has a dedicated token; revoke it first.
    DedicatedPresent,
    /// The empty-only precondition failed: a registered runner is attached.
    RegisteredPresent,
    /// Already attached; nothing changed.
    Unchanged(RegisteredRunner),
    /// Attached (replacing any other registered runner); the Space's new generation.
    Attached(RegisteredRunner, u64),
}

#[derive(Debug, PartialEq, Eq)]
enum GuardedAssignment {
    NotMember,
    NotOwner,
    Changed,
    Applied(u64),
}

impl GuardedAssignment {
    fn generation(self) -> ApiResult<u64> {
        match self {
            Self::NotMember => Err(ApiError::not_found("no such Space")),
            Self::NotOwner => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "only the Space owner can do this",
            )),
            Self::Changed => Err(ApiError::new(
                StatusCode::CONFLICT,
                "this Space's runner assignment changed; nothing was changed",
            )),
            Self::Applied(generation) => Ok(generation),
        }
    }
}

/// Spaces and their new runner generations after a change.
pub type Bumped = Vec<(String, u64)>;

fn row_runner(r: &rusqlite::Row<'_>) -> rusqlite::Result<RegisteredRunner> {
    Ok(RegisteredRunner {
        id: r.get(0)?,
        name: r.get(1)?,
        visibility: Visibility::parse(&r.get::<_, String>(2)?)?,
        owner_id: r.get(3)?,
    })
}

const RUNNER_COLUMNS: &str = "id, name, visibility, owner_id";

/// Bump the runner generation of every Space `runner_id` is attached to.
fn bump_attached(conn: &Connection, runner_id: &str) -> rusqlite::Result<Bumped> {
    let mut stmt = conn.prepare_cached(
        "UPDATE spaces SET runner_generation = runner_generation + 1
         WHERE id IN (SELECT space_id FROM space_runners WHERE runner_id = ?1)
         RETURNING id, runner_generation",
    )?;
    let rows = stmt.query_map([runner_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
    })?;
    rows.collect()
}

impl Db {
    pub fn create_runner(
        &self,
        owner_id: &str,
        name: &str,
        visibility: Visibility,
        token_hash: &[u8],
        max: usize,
    ) -> Result<RegisteredRunner, LimitError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let count: i64 = match visibility {
            Visibility::Personal => tx.query_row(
                "SELECT COUNT(*) FROM runners WHERE visibility = 'personal' AND owner_id = ?1",
                [owner_id],
                |r| r.get(0),
            )?,
            Visibility::Organization => tx.query_row(
                "SELECT COUNT(*) FROM runners WHERE visibility = 'organization'",
                [],
                |r| r.get(0),
            )?,
        };
        if usize::try_from(count).unwrap_or(usize::MAX) >= max {
            return Err(LimitError::TooMany);
        }
        let runner = RegisteredRunner {
            id: format!("rn-{}", short_id()),
            name: name.to_owned(),
            visibility,
            owner_id: owner_id.to_owned(),
        };
        tx.execute(
            "INSERT INTO runners (id, name, visibility, owner_id, token_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                runner.id,
                runner.name,
                visibility.as_str(),
                runner.owner_id,
                token_hash,
                now_ms()
            ],
        )?;
        tx.commit()?;
        Ok(runner)
    }

    /// Organization runners and `user_id`'s personal runners.
    pub fn list_runners(&self, user_id: &str) -> anyhow::Result<Vec<RegisteredRunner>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {RUNNER_COLUMNS} FROM runners
             WHERE visibility = 'organization' OR owner_id = ?1
             ORDER BY visibility = 'organization', created_at, rowid"
        ))?;
        let rows = stmt.query_map([user_id], row_runner)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_runner(&self, id: &str) -> anyhow::Result<Option<RegisteredRunner>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                &format!("SELECT {RUNNER_COLUMNS} FROM runners WHERE id = ?1"),
                [id],
                row_runner,
            )
            .optional()?)
    }

    /// Replace a registered runner's token. Returns the Spaces it serves
    /// with their new generations, or `None` when it does not exist.
    pub fn rotate_runner_token(
        &self,
        id: &str,
        token_hash: &[u8],
    ) -> anyhow::Result<Option<Bumped>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        if tx.execute(
            "UPDATE runners SET token_hash = ?2 WHERE id = ?1",
            params![id, token_hash],
        )? == 0
        {
            return Ok(None);
        }
        let bumped = bump_attached(&tx, id)?;
        tx.commit()?;
        Ok(Some(bumped))
    }

    /// Delete a registered runner: its token stops working and it is
    /// detached from every Space. Returns those Spaces with their new
    /// generations, or `None` when it does not exist.
    pub fn delete_runner(&self, id: &str) -> anyhow::Result<Option<Bumped>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM runners WHERE id = ?1)",
            [id],
            |r| r.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        let bumped = bump_attached(&tx, id)?;
        tx.execute("DELETE FROM space_runners WHERE runner_id = ?1", [id])?;
        tx.execute("DELETE FROM runners WHERE id = ?1", [id])?;
        tx.commit()?;
        Ok(Some(bumped))
    }

    /// The registered runner a token hash belongs to, with the Spaces it is
    /// attached to and their current generations, read together.
    pub fn registered_runner_by_token_hash(
        &self,
        hash: &[u8],
    ) -> anyhow::Result<Option<(String, Bumped)>> {
        let conn = self.lock();
        let Some(id) = conn
            .query_row(
                "SELECT id FROM runners WHERE token_hash = ?1",
                [hash],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut stmt = conn.prepare_cached(
            "SELECT s.id, s.runner_generation FROM space_runners a JOIN spaces s ON s.id = a.space_id
             WHERE a.runner_id = ?1 ORDER BY s.id",
        )?;
        let spaces = stmt
            .query_map([&id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })?
            .collect::<Result<_, _>>()?;
        Ok(Some((id, spaces)))
    }

    pub fn space_runner(&self, space_id: &str) -> anyhow::Result<SpaceRunner> {
        let conn = self.lock();
        let registered = conn
            .query_row(
                "SELECT r.id, r.name, r.visibility, r.owner_id
                 FROM space_runners a JOIN runners r ON r.id = a.runner_id
                 WHERE a.space_id = ?1",
                [space_id],
                row_runner,
            )
            .optional()?;
        if let Some(runner) = registered {
            return Ok(SpaceRunner::Registered(runner));
        }
        let dedicated: bool = conn
            .query_row(
                "SELECT runner_token_hash IS NOT NULL FROM spaces WHERE id = ?1",
                [space_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        Ok(if dedicated {
            SpaceRunner::Dedicated
        } else {
            SpaceRunner::None
        })
    }

    /// Read membership, assignment and its revision in one SQLite snapshot.
    /// Only the owner gets the capability and, if assigned, a revision.
    fn space_runner_for(
        &self,
        space_id: &str,
        actor: &str,
    ) -> anyhow::Result<Option<(SpaceRunner, bool, i64)>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT r.id, r.name, r.visibility, r.owner_id, s.runner_token_hash IS NOT NULL,
                        m.role = 'owner', s.runner_generation
                 FROM spaces s JOIN space_members m ON m.space_id = s.id
                 LEFT JOIN space_runners a ON a.space_id = s.id
                 LEFT JOIN runners r ON r.id = a.runner_id
                 WHERE s.id = ?1 AND m.user_id = ?2",
                params![space_id, actor],
                |row| {
                    let runner = if row.get::<_, Option<String>>(0)?.is_some() {
                        SpaceRunner::Registered(row_runner(row)?)
                    } else if row.get::<_, bool>(4)? {
                        SpaceRunner::Dedicated
                    } else {
                        SpaceRunner::None
                    };
                    Ok((runner, row.get(5)?, row.get(6)?))
                },
            )
            .optional()?)
    }

    /// Attach `runner_id` to `space_id` on behalf of `actor` (the Space
    /// owner), who must be allowed to see the runner. Replaces any other
    /// registered runner; refuses while a dedicated token is set.
    pub fn attach_runner(
        &self,
        space_id: &str,
        runner_id: &str,
        actor: &str,
    ) -> anyhow::Result<Attach> {
        self.attach_runner_inner(space_id, runner_id, actor, false)
    }

    /// Attach only while the Space is still owned and has neither kind of runner.
    pub fn attach_runner_if_empty(
        &self,
        space_id: &str,
        runner_id: &str,
        actor: &str,
    ) -> anyhow::Result<Attach> {
        self.attach_runner_inner(space_id, runner_id, actor, true)
    }

    fn attach_runner_inner(
        &self,
        space_id: &str,
        runner_id: &str,
        actor: &str,
        empty_only: bool,
    ) -> anyhow::Result<Attach> {
        let mut conn = self.lock();
        // Reserve the write before observing either runner assignment, even
        // if a different SQLite connection is pairing the same Space.
        let tx = if empty_only {
            conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?
        } else {
            conn.transaction()?
        };
        if empty_only {
            let owned: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM space_members
                 WHERE space_id = ?1 AND user_id = ?2 AND role = 'owner')",
                params![space_id, actor],
                |r| r.get(0),
            )?;
            if !owned {
                return Ok(Attach::NoOwnedSpace);
            }
        }
        let runner = tx
            .query_row(
                &format!("SELECT {RUNNER_COLUMNS} FROM runners WHERE id = ?1"),
                [runner_id],
                row_runner,
            )
            .optional()?;
        let Some(runner) = runner.filter(|r| r.visible_to(actor)) else {
            return Ok(Attach::NoSuchRunner);
        };
        let dedicated: bool = tx.query_row(
            "SELECT runner_token_hash IS NOT NULL FROM spaces WHERE id = ?1",
            [space_id],
            |r| r.get(0),
        )?;
        if dedicated {
            return Ok(Attach::DedicatedPresent);
        }
        let current: Option<String> = tx
            .query_row(
                "SELECT runner_id FROM space_runners WHERE space_id = ?1",
                [space_id],
                |r| r.get(0),
            )
            .optional()?;
        if empty_only && current.is_some() {
            return Ok(Attach::RegisteredPresent);
        }
        if current.as_deref() == Some(runner_id) {
            return Ok(Attach::Unchanged(runner));
        }
        tx.execute(
            "INSERT INTO space_runners (space_id, runner_id, attached_by, attached_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (space_id) DO UPDATE SET
                 runner_id = excluded.runner_id,
                 attached_by = excluded.attached_by,
                 attached_at = excluded.attached_at",
            params![space_id, runner_id, actor, now_ms()],
        )?;
        let generation: i64 = tx.query_row(
            "UPDATE spaces SET runner_generation = runner_generation + 1
             WHERE id = ?1 RETURNING runner_generation",
            [space_id],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(Attach::Attached(runner, generation as u64))
    }

    /// Leave `space_id` without a runner: detach its registered runner, or
    /// revoke its dedicated token. Returns the new generation, or `None`
    /// when it had no runner.
    pub fn detach_runner(&self, space_id: &str) -> anyhow::Result<Option<u64>> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let generation = Self::remove_assignment(&tx, space_id)?;
        tx.commit()?;
        Ok(generation)
    }

    fn detach_runner_if_current(
        &self,
        space_id: &str,
        actor: &str,
        revision: i64,
    ) -> anyhow::Result<GuardedAssignment> {
        let mut conn = self.lock();
        // Acquire the write reservation before checking live ownership or
        // generation, including when another process uses this database.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        match Self::assignment_owner(&tx, space_id, actor)? {
            None => return Ok(GuardedAssignment::NotMember),
            Some((false, _)) => return Ok(GuardedAssignment::NotOwner),
            Some((true, generation)) if generation != revision => {
                return Ok(GuardedAssignment::Changed);
            }
            Some((true, _)) => {}
        }
        let Some(generation) = Self::remove_assignment(&tx, space_id)? else {
            return Ok(GuardedAssignment::Changed);
        };
        tx.commit()?;
        Ok(GuardedAssignment::Applied(generation))
    }

    /// Issue only when empty, or rotate the exact dedicated revision. Live
    /// ownership and the assignment predicate are checked under the same write
    /// reservation. Returns only the committed generation, never a secret.
    fn set_dedicated_token_guarded(
        &self,
        space_id: &str,
        actor: &str,
        revision: Option<i64>,
        hash: &[u8],
    ) -> anyhow::Result<GuardedAssignment> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        match Self::assignment_owner(&tx, space_id, actor)? {
            None => return Ok(GuardedAssignment::NotMember),
            Some((false, _)) => return Ok(GuardedAssignment::NotOwner),
            Some((true, _)) => {}
        }
        let generation: Option<i64> = tx
            .query_row(
                "UPDATE spaces SET runner_token_hash = ?2, runner_generation = runner_generation + 1
                 WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM space_runners WHERE space_id = ?1)
                   AND ((?3 IS NULL AND runner_token_hash IS NULL)
                     OR (runner_generation = ?3 AND runner_token_hash IS NOT NULL))
                 RETURNING runner_generation",
                params![space_id, hash, revision],
                |row| row.get(0),
            )
            .optional()?;
        let Some(generation) = generation else {
            return Ok(GuardedAssignment::Changed);
        };
        tx.commit()?;
        Ok(GuardedAssignment::Applied(generation as u64))
    }

    fn assignment_owner(
        tx: &Connection,
        space_id: &str,
        actor: &str,
    ) -> rusqlite::Result<Option<(bool, i64)>> {
        tx.query_row(
            "SELECT m.role = 'owner', s.runner_generation
             FROM spaces s JOIN space_members m ON m.space_id = s.id
             WHERE s.id = ?1 AND m.user_id = ?2",
            params![space_id, actor],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
    }

    fn remove_assignment(tx: &Connection, space_id: &str) -> anyhow::Result<Option<u64>> {
        let detached = tx.execute("DELETE FROM space_runners WHERE space_id = ?1", [space_id])?;
        let revoked = tx.execute(
            "UPDATE spaces SET runner_token_hash = NULL
             WHERE id = ?1 AND runner_token_hash IS NOT NULL",
            [space_id],
        )?;
        if detached == 0 && revoked == 0 {
            return Ok(None);
        }
        let generation: i64 = tx.query_row(
            "UPDATE spaces SET runner_generation = runner_generation + 1
             WHERE id = ?1 RETURNING runner_generation",
            [space_id],
            |r| r.get(0),
        )?;
        Ok(Some(generation as u64))
    }
}

// ---- HTTP ----

fn view(runner: &RegisteredRunner, who: &Human, state: &AppState) -> serde_json::Value {
    let (presence, update) = crate::updates::runner_fields(state, &format!("runner:{}", runner.id));
    json!({
        "id": runner.id,
        "name": runner.name,
        "visibility": runner.visibility,
        "can_manage": runner.manageable_by(&who.id, state.config.is_org_admin(&who.id)),
        "presence": presence,
        "update": update,
    })
}

/// How a Space's runner is shown to its members. A dedicated token has no
/// registered runner behind it, so it has no id. `presence` and `update`
/// are display data (see [`crate::updates`]).
fn space_view(state: &AppState, space_id: &str, runner: &SpaceRunner) -> serde_json::Value {
    match runner {
        SpaceRunner::None => serde_json::Value::Null,
        SpaceRunner::Dedicated => {
            let (presence, update) =
                crate::updates::runner_fields(state, &format!("space:{space_id}"));
            json!({
                "id": null,
                "name": "Dedicated Space runner",
                "visibility": "dedicated",
                "kind": "dedicated",
                "presence": presence,
                "update": update,
            })
        }
        SpaceRunner::Registered(r) => {
            let (presence, update) =
                crate::updates::runner_fields(state, &format!("runner:{}", r.id));
            json!({
                "id": r.id,
                "name": r.name,
                "visibility": r.visibility,
                "kind": "registered",
                "presence": presence,
                "update": update,
            })
        }
    }
}

const NO_RUNNER: &str = "no such runner";

/// A runner `who` may see; 404 otherwise.
async fn visible_runner(state: &AppState, id: &str, who: &Human) -> ApiResult<RegisteredRunner> {
    if crate::config::validate_id(id).is_err() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, NO_RUNNER));
    }
    let id = id.to_owned();
    api::db(state, move |db| db.get_runner(&id))
        .await?
        .filter(|r| r.visible_to(&who.id))
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, NO_RUNNER))
}

/// Like [`visible_runner`], and 403 unless `who` may manage it.
pub(crate) async fn managed_runner(
    state: &AppState,
    id: &str,
    who: &Human,
) -> ApiResult<RegisteredRunner> {
    let runner = visible_runner(state, id, who).await?;
    if !runner.manageable_by(&who.id, state.config.is_org_admin(&who.id)) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "only an organization admin can manage an organization runner",
        ));
    }
    Ok(runner)
}

pub async fn list_runners(
    State(state): State<AppState>,
    who: Human,
) -> ApiResult<Json<serde_json::Value>> {
    let uid = who.id.clone();
    let runners = api::db(&state, move |db| db.list_runners(&uid)).await?;
    let runners: Vec<_> = runners.iter().map(|r| view(r, &who, &state)).collect();
    Ok(Json(json!({"runners": runners, "attach_if_empty": true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRunner {
    name: String,
    visibility: Visibility,
}

pub async fn create_runner(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<CreateRunner>,
) -> ApiResult<Response> {
    let Json(body) = body?;
    let name = api::check_name(&state, &body.name)?;
    let limits = &state.config.limits;
    let max = match body.visibility {
        Visibility::Organization if !state.config.is_org_admin(&who.id) => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "only an organization admin can create an organization runner",
            ));
        }
        Visibility::Organization => limits.max_org_runners,
        Visibility::Personal => limits.max_personal_runners,
    };
    let token = new_registered_token().map_err(ApiError::internal)?;
    let hash = runner_token_hash(&token);
    let (owner, visibility) = (who.id.clone(), body.visibility);
    match api::db(&state, move |db| {
        Ok(db.create_runner(&owner, &name, visibility, &hash, max))
    })
    .await?
    {
        Ok(runner) => {
            tracing::info!(runner = %runner.id, visibility = runner.visibility.as_str(), "runner registered");
            Ok(api::no_store(
                StatusCode::CREATED,
                json!({"runner": view(&runner, &who, &state), "token": token}),
            ))
        }
        Err(LimitError::TooMany) => Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("runner limit reached ({max})"),
        )),
        Err(LimitError::Duplicate | LimitError::Nested) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "the runner could not be created",
        )),
        Err(LimitError::Other(e)) => Err(ApiError::internal(e)),
    }
}

pub async fn rotate_runner_token(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    api::empty_object(&body)?;
    let runner = managed_runner(&state, &id, &who).await?;
    let token = new_registered_token().map_err(ApiError::internal)?;
    let hash = runner_token_hash(&token);
    let rid = runner.id.clone();
    let bumped = api::db(&state, move |db| db.rotate_runner_token(&rid, &hash))
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, NO_RUNNER))?;
    // Same runner, new token: claimed jobs fail, queued ones wait for it.
    for (space, generation) in &bumped {
        state.jobs.rotate(space, *generation);
    }
    tracing::info!(runner = %runner.id, spaces = bumped.len(), "runner token rotated");
    Ok(api::no_store(StatusCode::OK, json!({"token": token})))
}

/// Revoke the runner's token and detach it from every Space it served.
pub async fn delete_runner(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let runner = managed_runner(&state, &id, &who).await?;
    let rid = runner.id.clone();
    let bumped = api::db(&state, move |db| db.delete_runner(&rid))
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, NO_RUNNER))?;
    for (space, generation) in &bumped {
        state.jobs.revoke(space, *generation);
    }
    tracing::info!(runner = %runner.id, detached = bumped.len(), "runner deleted");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_space_runner(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
) -> ApiResult<Response> {
    if crate::config::validate_id(&sid).is_err() {
        return Err(ApiError::not_found("no such Space"));
    }
    let space_id = sid.clone();
    let (runner, owner, revision) =
        api::db(&state, move |db| db.space_runner_for(&space_id, &who.id))
            .await?
            .ok_or_else(|| ApiError::not_found("no such Space"))?;
    let mut body = json!({"runner": space_view(&state, &sid, &runner)});
    if owner {
        body["guarded_token_changes"] = json!(true);
        if runner != SpaceRunner::None {
            body["assignment_revision"] = json!(revision.to_string());
        }
    }
    // Confirmations and token receipts rely on a fresh assignment, not a
    // cached response from before a pairing change.
    Ok(api::no_store(StatusCode::OK, body))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachBody {
    runner_id: String,
}

pub async fn put_space_runner(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<AttachBody>,
) -> ApiResult<Json<serde_json::Value>> {
    attach_space_runner(state, who, sid, body, false).await
}

/// A separate route: a client must never accidentally invoke the replacement
/// API when talking to a server that doesn't support empty-only attachment.
pub async fn post_space_runner_if_empty(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<AttachBody>,
) -> ApiResult<Json<serde_json::Value>> {
    attach_space_runner(state, who, sid, body, true).await
}

async fn attach_space_runner(
    state: AppState,
    who: Human,
    sid: String,
    body: JsonBody<AttachBody>,
    empty_only: bool,
) -> ApiResult<Json<serde_json::Value>> {
    let space = owner_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    if crate::config::validate_id(&body.runner_id).is_err() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, NO_RUNNER));
    }
    let (space_id, rid, uid) = (space.id.clone(), body.runner_id, who.id.clone());
    match api::db(&state, move |db| {
        if empty_only {
            db.attach_runner_if_empty(&space_id, &rid, &uid)
        } else {
            db.attach_runner(&space_id, &rid, &uid)
        }
    })
    .await?
    {
        Attach::NoOwnedSpace => Err(ApiError::not_found("no such Space")),
        Attach::NoSuchRunner => Err(ApiError::new(StatusCode::NOT_FOUND, NO_RUNNER)),
        Attach::RegisteredPresent => Err(ApiError::new(
            StatusCode::CONFLICT,
            "this Space already has a registered runner; nothing was changed",
        )),
        Attach::DedicatedPresent => Err(ApiError::new(
            StatusCode::CONFLICT,
            "this Space has a dedicated runner token; revoke it first \
             (DELETE /v1/spaces/{sid}/runner-token or DELETE /v1/spaces/{sid}/runner)",
        )),
        Attach::Unchanged(runner) => Ok(Json(
            json!({"runner": space_view(&state, &space.id, &SpaceRunner::Registered(runner))}),
        )),
        Attach::Attached(runner, generation) => {
            // Whatever was queued or claimed for the previous runner fails.
            state.jobs.revoke(&space.id, generation);
            tracing::info!(space = %space.id, runner = %runner.id, generation, "runner attached");
            Ok(Json(
                json!({"runner": space_view(&state, &space.id, &SpaceRunner::Registered(runner))}),
            ))
        }
    }
}

/// Detach the Space's registered runner, or revoke its dedicated token.
pub async fn delete_space_runner(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<StatusCode> {
    api::empty_object(&body)?;
    let space = owner_of(&state, &sid, &who).await?;
    let space_id = space.id.clone();
    if let Some(generation) = api::db(&state, move |db| db.detach_runner(&space_id)).await? {
        state.jobs.revoke(&space.id, generation);
        tracing::info!(space = %space.id, generation, "runner detached");
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentRevisionBody {
    /// Decimal string, not a JSON number: SQLite's integer can exceed JS precision.
    assignment_revision: String,
}

fn assignment_revision(raw: &str) -> ApiResult<i64> {
    raw.parse::<i64>()
        .ok()
        .filter(|r| *r >= 0 && r.to_string() == raw)
        .ok_or_else(|| ApiError::bad_request("invalid runner assignment revision"))
}

/// A separate action route: a client must never fall back to unconditional DELETE.
pub async fn post_detach_space_runner_if_current(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<AssignmentRevisionBody>,
) -> ApiResult<StatusCode> {
    let space = owner_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    let revision = assignment_revision(&body.assignment_revision)?;
    let space_id = space.id.clone();
    let generation = api::db(&state, move |db| {
        db.detach_runner_if_current(&space_id, &who.id, revision)
    })
    .await?
    .generation()?;
    state.jobs.revoke(&space.id, generation);
    tracing::info!(space = %space.id, generation, "runner removed (guarded)");
    Ok(StatusCode::NO_CONTENT)
}

/// Separate from the legacy issue-or-rotate route so old servers fail closed.
pub async fn post_issue_dedicated_token_if_empty(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    let space = owner_of(&state, &sid, &who).await?;
    api::empty_object(&body)?;
    change_dedicated_token(&state, who, space.id, None).await
}

pub async fn post_rotate_dedicated_token_if_current(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<AssignmentRevisionBody>,
) -> ApiResult<Response> {
    let space = owner_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    let revision = assignment_revision(&body.assignment_revision)?;
    change_dedicated_token(&state, who, space.id, Some(revision)).await
}

async fn change_dedicated_token(
    state: &AppState,
    who: Human,
    sid: String,
    revision: Option<i64>,
) -> ApiResult<Response> {
    let token = new_runner_token().map_err(ApiError::internal)?;
    let hash = runner_token_hash(&token);
    let space_id = sid.clone();
    let generation = api::db(state, move |db| {
        db.set_dedicated_token_guarded(&space_id, &who.id, revision, &hash)
    })
    .await?
    .generation()?;
    state.jobs.rotate(&sid, generation);
    tracing::info!(space = %sid, generation, "dedicated runner token changed (guarded)");
    Ok(api::no_store(
        StatusCode::OK,
        json!({"token": token, "assignment_revision": generation.to_string()}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_space(owner: &str) -> (Db, String) {
        let db = Db::open_in_memory().unwrap();
        let s = db.create_space(owner, "s", 10).unwrap();
        (db, s.id)
    }

    #[test]
    fn guarded_runner_writes_recheck_live_owner_even_with_current_revision() {
        let (db, sid) = db_with_space("alice");
        let generation = db.set_runner_token(&sid, b"fixture-hash").unwrap().unwrap();
        let revision = i64::try_from(generation).unwrap();
        // Simulate a permission change after HTTP's owner preflight. Call
        // the write directly, so access can't pass based on that old check.
        db.lock()
            .execute(
                "UPDATE space_members SET role = 'member' WHERE space_id = ?1 AND user_id = 'alice'",
                [&sid],
            )
            .unwrap();
        assert_eq!(
            db.detach_runner_if_current(&sid, "alice", revision)
                .unwrap(),
            GuardedAssignment::NotOwner
        );
        assert_eq!(
            db.set_dedicated_token_guarded(&sid, "alice", Some(revision), b"fixture-new-hash")
                .unwrap(),
            GuardedAssignment::NotOwner
        );
        db.lock()
            .execute(
                "DELETE FROM space_members WHERE space_id = ?1 AND user_id = 'alice'",
                [&sid],
            )
            .unwrap();
        assert_eq!(
            db.detach_runner_if_current(&sid, "alice", revision)
                .unwrap(),
            GuardedAssignment::NotMember
        );
        assert_eq!(
            db.set_dedicated_token_guarded(&sid, "alice", None, b"fixture-new-hash")
                .unwrap(),
            GuardedAssignment::NotMember
        );
        assert_eq!(db.space_runner(&sid).unwrap(), SpaceRunner::Dedicated);
        assert_eq!(db.space_runner_generation(&sid).unwrap(), Some(generation));
    }

    #[test]
    fn attach_rules_and_generations() {
        let (db, s) = db_with_space("alice");
        let mine = db
            .create_runner("alice", "laptop", Visibility::Personal, b"h1", 5)
            .unwrap();
        let bobs = db
            .create_runner("bob", "bob", Visibility::Personal, b"h2", 5)
            .unwrap();
        let org = db
            .create_runner("admin", "ci", Visibility::Organization, b"h3", 5)
            .unwrap();
        assert_eq!(
            db.attach_runner(&s, &bobs.id, "alice").unwrap(),
            Attach::NoSuchRunner
        );
        assert_eq!(
            db.attach_runner(&s, "rn-nope", "alice").unwrap(),
            Attach::NoSuchRunner
        );
        let g0 = db.space_runner_generation(&s).unwrap().unwrap();
        let Attach::Attached(_, g1) = db.attach_runner(&s, &mine.id, "alice").unwrap() else {
            panic!()
        };
        assert!(g1 > g0);
        assert!(matches!(
            db.attach_runner(&s, &mine.id, "alice").unwrap(),
            Attach::Unchanged(_)
        ));
        assert!(db.runner_paired(&s).unwrap());
        // A dedicated token cannot replace an attached runner.
        assert_eq!(db.set_runner_token(&s, b"legacy").unwrap(), None);
        assert_eq!(db.clear_runner_token(&s).unwrap(), None);
        let Attach::Attached(_, g2) = db.attach_runner(&s, &org.id, "alice").unwrap() else {
            panic!()
        };
        assert!(g2 > g1);
        let (id, spaces) = db.registered_runner_by_token_hash(b"h3").unwrap().unwrap();
        assert_eq!(id, org.id);
        assert_eq!(spaces, vec![(s.clone(), g2)]);
        assert_eq!(
            db.registered_runner_by_token_hash(b"h1")
                .unwrap()
                .unwrap()
                .1,
            vec![]
        );
        // Rotation bumps attached Spaces only.
        let bumped = db.rotate_runner_token(&org.id, b"h4").unwrap().unwrap();
        assert_eq!(bumped, vec![(s.clone(), g2 + 1)]);
        assert!(db.registered_runner_by_token_hash(b"h3").unwrap().is_none());
        // Delete detaches.
        let bumped = db.delete_runner(&org.id).unwrap().unwrap();
        assert_eq!(bumped, vec![(s.clone(), g2 + 2)]);
        assert_eq!(db.space_runner(&s).unwrap(), SpaceRunner::None);
        assert!(!db.runner_paired(&s).unwrap());
        assert_eq!(db.delete_runner(&org.id).unwrap(), None);
        // Dedicated token blocks attaching; detach revokes it.
        db.set_runner_token(&s, b"legacy").unwrap().unwrap();
        assert_eq!(db.space_runner(&s).unwrap(), SpaceRunner::Dedicated);
        assert_eq!(
            db.attach_runner(&s, &mine.id, "alice").unwrap(),
            Attach::DedicatedPresent
        );
        assert!(db.detach_runner(&s).unwrap().is_some());
        assert!(db.runner_by_token_hash(b"legacy").unwrap().is_none());
        assert_eq!(db.detach_runner(&s).unwrap(), None);
    }

    #[test]
    fn limits_per_owner_and_org() {
        let db = Db::open_in_memory().unwrap();
        db.create_runner("a", "1", Visibility::Personal, b"1", 1)
            .unwrap();
        assert!(matches!(
            db.create_runner("a", "2", Visibility::Personal, b"2", 1),
            Err(LimitError::TooMany)
        ));
        db.create_runner("b", "3", Visibility::Personal, b"3", 1)
            .unwrap();
        db.create_runner("a", "4", Visibility::Organization, b"4", 1)
            .unwrap();
        assert!(matches!(
            db.create_runner("b", "5", Visibility::Organization, b"5", 1),
            Err(LimitError::TooMany)
        ));
        let seen: Vec<_> = db
            .list_runners("a")
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(seen, vec!["1", "4"]);
        let seen: Vec<_> = db
            .list_runners("b")
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(seen, vec!["3", "4"]);
    }

    #[test]
    fn upgrades_a_version_2_database_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v2.sqlite3");
        {
            let db = Db::open(&path).unwrap();
            let s = db.create_space("alice", "kept", 10).unwrap();
            db.set_runner_token(&s.id, b"legacy").unwrap().unwrap();
        }
        {
            // Make it look like a baseline (version 2) database.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "DROP TABLE space_runners; DROP TABLE runners;
                 UPDATE metro_meta SET value = '2' WHERE key = 'schema_version';",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let spaces = db.list_spaces("alice").unwrap();
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].name, "kept");
        assert!(spaces[0].runner_paired);
        assert!(db.runner_by_token_hash(b"legacy").unwrap().is_some());
        assert_eq!(
            db.space_runner(&spaces[0].id).unwrap(),
            SpaceRunner::Dedicated
        );
        let conn = db.lock();
        let v: String = conn
            .query_row(
                "SELECT value FROM metro_meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "3");
    }
}
