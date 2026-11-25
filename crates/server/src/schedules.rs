//! Scheduled agent prompts: a person's prompt that the server sends to the
//! agent once at a set instant, or on a cron recurrence in an IANA time zone.
//!
//! A task belongs to the person who created it (their stable user id, never
//! a display name) and is visible and manageable only by them. Another
//! person's task and a missing one both answer 404. It runs either in the
//! owner's personal Chat (`space_id` and `room_id` both null) or in a room
//! or thread of a Space they are a member of, through
//! [`crate::model_routing::submit_scheduled`]. That is the same routing,
//! model connection and runner as an interactive `@agent` message from the
//! owner. A scheduled prompt counts as the owner explicitly asking the agent.
//!
//! # Times
//!
//! Everything is stored as UTC milliseconds. A one-time `at` must carry an
//! offset (RFC 3339) and is returned in UTC. A cron expression is evaluated
//! in its `timezone`; see [`cron`] for the field syntax and for what happens
//! across daylight saving time changes. Responses give `next_run_at` in UTC
//! and, for cron tasks, `next_run_local` in the task's zone.
//!
//! # Firing
//!
//! [`spawn`] runs a loop that sleeps until the earliest `next_run_at`, at
//! most `METRO_SCHEDULE_TICK_SECS` (default 30), and wakes early when a task
//! is created or edited. A one-time task a few seconds ahead therefore fires
//! within about a second of its time. Each tick ([`tick_with`]) claims due occurrences in one immediate transaction. It
//! inserts a `schedule_runs` row unique on `(task_id, due_at)` and moves the
//! task's `next_run_at` past that occurrence in the same transaction, before
//! anything is submitted. A second tick, a second process on the same file
//! or a restart therefore never submits the same occurrence twice.
//!
//! - **Late or missed.** An occurrence found more than [`MISSED_GRACE`]
//!   after its due time (the server was down or stalled) is recorded as
//!   `missed` and is not run late. A recurring task records one `missed` row
//!   with the number of skipped occurrences, then resumes at its next future
//!   occurrence. Missed occurrences are never replayed in a burst.
//! - **Interrupted.** A claimed occurrence whose submission had not finished
//!   when the server stopped is marked `interrupted` at the next start and is
//!   not retried, because its trigger may or may not have been posted.
//! - **Revalidated before dispatch.** Just before submitting, the task is
//!   read again. If it was deleted, paused or edited after the claim, the
//!   occurrence is `cancelled`. If the owner is no longer a member of the
//!   Space, or the room is gone, the occurrence is `revoked`, the task is
//!   paused, and a notice goes to the owner's personal Chat. Nothing falls
//!   back to another room, Space, person or model.
//! - **Outcome.** `dispatched` means that `submit_scheduled` accepted the
//!   prompt: the trigger was posted and agent work started. The run's own
//!   result appears in the conversation, not here. `error` carries the
//!   routing error (for example, no usable model connection, or the owner's
//!   device is offline). `unknown` means the submission did not return
//!   within [`SUBMIT_TIMEOUT`], so it may or may not have been posted. That
//!   occurrence is not retried.
//!
//! The server stores no user token. Authority comes from the owner's stable
//! id, checked against current Space membership when the task is created,
//! edited and fired.

pub mod cron;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration as StdDuration;

use anyhow::Context;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use chrono::{DateTime, Duration, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody};
use crate::auth::Human;
use crate::db::{Addressed, Author, Db, actor, kind};
use cron::CronExpr;

/// Most tasks one person may have.
pub const MAX_TASKS_PER_USER: usize = 50;
/// Shortest allowed gap between two firings of a cron task.
pub const MIN_INTERVAL_MINUTES: u32 = 15;
/// Furthest ahead a one-time task may be set.
pub const MAX_ONCE_AHEAD_DAYS: i64 = 366;
/// An occurrence found later than this after its due time is `missed`.
pub const MISSED_GRACE: Duration = Duration::minutes(5);
/// How long one submission may take before it is recorded as `unknown`.
pub const SUBMIT_TIMEOUT: StdDuration = StdDuration::from_secs(60);
/// History rows kept per task.
pub const HISTORY_PER_TASK: usize = 100;
/// Longest task name, in characters.
pub const MAX_NAME_CHARS: usize = 100;
const MAX_EXPRESSION_CHARS: usize = 100;
const MAX_TIMEZONE_CHARS: usize = 64;
const MAX_DETAIL_CHARS: usize = 500;
/// Occurrences claimed per database round; a tick runs a few rounds.
const CLAIM_BATCH: usize = 32;
const CLAIM_ROUNDS: usize = 4;
/// Missed occurrences counted before the count is reported as a lower bound.
const MAX_COUNTED_MISSES: usize = 1_000;
pub const TICK_ENV: &str = "METRO_SCHEDULE_TICK_SECS";
const DEFAULT_TICK: StdDuration = StdDuration::from_secs(30);
/// Shortest sleep between ticks, so a backlog never spins.
const MIN_WAIT: StdDuration = StdDuration::from_millis(200);

/// Occurrence statuses in `schedule_runs` and `last_status`.
pub mod status {
    /// Claimed; submission in progress.
    pub const DISPATCHING: &str = "dispatching";
    pub const DISPATCHED: &str = "dispatched";
    pub const ERROR: &str = "error";
    pub const UNKNOWN: &str = "unknown";
    pub const MISSED: &str = "missed";
    pub const CANCELLED: &str = "cancelled";
    pub const REVOKED: &str = "revoked";
    pub const INTERRUPTED: &str = "interrupted";
}

// ---- Schema ----

/// Create the schedule tables. Idempotent.
pub fn init(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schedules (
             id          TEXT PRIMARY KEY NOT NULL,
             owner_id    TEXT NOT NULL,
             name        TEXT NOT NULL,
             prompt      TEXT NOT NULL,
             space_id    TEXT,
             room_id     TEXT,
             kind        TEXT NOT NULL CHECK (kind IN ('once', 'cron')),
             at_ms       INTEGER,
             expression  TEXT,
             timezone    TEXT,
             enabled     INTEGER NOT NULL,
             next_run_at INTEGER,
             revision    INTEGER NOT NULL DEFAULT 1,
             last_status TEXT,
             last_run_at INTEGER,
             last_detail TEXT,
             created_at  INTEGER NOT NULL,
             updated_at  INTEGER NOT NULL,
             CHECK ((space_id IS NULL) = (room_id IS NULL)),
             CHECK ((kind = 'once' AND at_ms IS NOT NULL)
                 OR (kind = 'cron' AND expression IS NOT NULL AND timezone IS NOT NULL))
         );
         CREATE INDEX IF NOT EXISTS schedules_owner ON schedules (owner_id, created_at);
         CREATE INDEX IF NOT EXISTS schedules_due ON schedules (next_run_at) WHERE enabled = 1;
         CREATE TABLE IF NOT EXISTS schedule_runs (
             id          INTEGER PRIMARY KEY AUTOINCREMENT,
             task_id     TEXT NOT NULL REFERENCES schedules (id) ON DELETE CASCADE,
             owner_id    TEXT NOT NULL,
             due_at      INTEGER NOT NULL,
             status      TEXT NOT NULL,
             detail      TEXT,
             claimed_at  INTEGER NOT NULL,
             finished_at INTEGER,
             UNIQUE (task_id, due_at)
         );
         CREATE INDEX IF NOT EXISTS schedule_runs_task ON schedule_runs (task_id, id);
         CREATE INDEX IF NOT EXISTS schedule_runs_status ON schedule_runs (status)
             WHERE status = 'dispatching';",
    )
    .context("creating schedule tables")
}

/// `GET|POST /v1/schedules`, `PATCH|DELETE /v1/schedules/{id}`,
/// `GET /v1/schedules/{id}/history`, `POST /v1/schedules/{id}/run`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/schedules", get(list).post(create))
        .route(
            "/v1/schedules/{id}",
            axum::routing::patch(update).delete(remove),
        )
        .route("/v1/schedules/{id}/history", get(history))
        .route("/v1/schedules/{id}/run", axum::routing::post(run_now))
}

// ---- Model ----

#[derive(Clone, Debug, PartialEq)]
pub enum Schedule {
    Once { at: DateTime<Utc> },
    Cron { expression: String, timezone: Tz },
}

impl Schedule {
    /// The first firing strictly after `after`.
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Schedule::Once { at } => (*at > after).then_some(*at),
            Schedule::Cron {
                expression,
                timezone,
            } => CronExpr::parse(expression)
                .ok()?
                .next_after(*timezone, after),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    pub id: String,
    pub owner_id: String,
    pub name: String,
    pub prompt: String,
    pub target: Option<(String, String)>,
    pub schedule: Schedule,
    pub enabled: bool,
    pub next_run_at: Option<DateTime<Utc>>,
    pub revision: i64,
    pub last_status: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_detail: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

impl Task {
    pub fn view(&self) -> serde_json::Value {
        let (space_id, room_id) = match &self.target {
            Some((s, r)) => (Some(s.as_str()), Some(r.as_str())),
            None => (None, None),
        };
        let (schedule, next_local) = match &self.schedule {
            Schedule::Once { at } => (json!({"kind": "once", "at": rfc3339(*at)}), None),
            Schedule::Cron {
                expression,
                timezone,
            } => (
                json!({"kind": "cron", "expression": expression, "timezone": timezone.name()}),
                self.next_run_at.map(|t| {
                    t.with_timezone(timezone)
                        .to_rfc3339_opts(SecondsFormat::Secs, true)
                }),
            ),
        };
        json!({
            "id": self.id,
            "owner_id": self.owner_id,
            "name": self.name,
            "prompt": self.prompt,
            "space_id": space_id,
            "room_id": room_id,
            "schedule": schedule,
            "enabled": self.enabled,
            "next_run_at": self.next_run_at.map(rfc3339),
            "next_run_local": next_local,
            "last_status": self.last_status,
            "last_run_at": self.last_run_at.map(rfc3339),
            "last_detail": self.last_detail,
            "created_at": rfc3339(self.created_at),
            "updated_at": rfc3339(self.updated_at),
        })
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RunView {
    pub id: i64,
    /// When it was due; for a run now, when it was asked for.
    pub due_at: String,
    /// Started with "Run now" rather than by the schedule.
    pub run_now: bool,
    pub status: String,
    pub detail: Option<String>,
    pub claimed_at: String,
    pub finished_at: Option<String>,
}

// ---- Input ----

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleIn {
    pub kind: String,
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub expression: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub space_id: Option<String>,
    #[serde(default)]
    pub room_id: Option<String>,
    pub schedule: ScheduleIn,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// A field that may be absent (keep), `null` (clear) or a value.
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub space_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub room_id: Option<Option<String>>,
    #[serde(default)]
    pub schedule: Option<ScheduleIn>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug)]
pub enum ScheduleError {
    BadRequest(String),
    NotFound(&'static str),
    Conflict(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for ScheduleError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

impl From<rusqlite::Error> for ScheduleError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Internal(e.into())
    }
}

impl From<ScheduleError> for ApiError {
    fn from(e: ScheduleError) -> Self {
        match e {
            ScheduleError::BadRequest(m) => ApiError::new(StatusCode::BAD_REQUEST, m),
            ScheduleError::NotFound(m) => ApiError::new(StatusCode::NOT_FOUND, m),
            ScheduleError::Conflict(m) => ApiError::new(StatusCode::CONFLICT, m),
            ScheduleError::Internal(e) => ApiError::internal(e),
        }
    }
}

fn bad(m: impl Into<String>) -> ScheduleError {
    ScheduleError::BadRequest(m.into())
}

const NO_TASK: &str = "no such scheduled task";

fn check_name(name: &str) -> Result<String, ScheduleError> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || name.chars().any(char::is_control)
    {
        return Err(bad(format!(
            "name must be 1..={MAX_NAME_CHARS} printable characters"
        )));
    }
    Ok(name.to_owned())
}

fn check_prompt(prompt: &str, max: usize) -> Result<String, ScheduleError> {
    let prompt = prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > max {
        return Err(bad(format!("prompt must be 1..={max} characters")));
    }
    Ok(prompt.to_owned())
}

/// Parse and check a schedule as of `now`: a one-time instant in the
/// future (at most a year ahead), or a cron expression that fires, no more
/// often than every [`MIN_INTERVAL_MINUTES`], in a known IANA zone.
pub fn parse_schedule(input: &ScheduleIn, now: DateTime<Utc>) -> Result<Schedule, ScheduleError> {
    match input.kind.as_str() {
        "once" => {
            if input.expression.is_some() || input.timezone.is_some() {
                return Err(bad(
                    "a once schedule takes only `at`, an RFC 3339 timestamp with an offset",
                ));
            }
            let at = input
                .at
                .as_deref()
                .ok_or_else(|| bad("schedule.at is required for kind `once`"))?;
            let at = DateTime::parse_from_rfc3339(at.trim())
                .map_err(|_| {
                    bad(
                        "schedule.at must be an RFC 3339 timestamp with an offset, such as \
                         2026-11-23T09:00:00+08:00",
                    )
                })?
                .with_timezone(&Utc);
            let at = Utc
                .timestamp_millis_opt(at.timestamp_millis())
                .single()
                .ok_or_else(|| bad("schedule.at is out of range"))?;
            if at <= now {
                return Err(bad("schedule.at must be in the future"));
            }
            if at > now + Duration::days(MAX_ONCE_AHEAD_DAYS) {
                return Err(bad(format!(
                    "schedule.at must be within {MAX_ONCE_AHEAD_DAYS} days"
                )));
            }
            Ok(Schedule::Once { at })
        }
        "cron" => {
            if input.at.is_some() {
                return Err(bad(
                    "a cron schedule takes `expression` and `timezone`, not `at`",
                ));
            }
            let expression = input
                .expression
                .as_deref()
                .map(str::trim)
                .ok_or_else(|| bad("schedule.expression is required for kind `cron`"))?;
            if expression.chars().count() > MAX_EXPRESSION_CHARS {
                return Err(bad("schedule.expression is too long"));
            }
            let zone = input.timezone.as_deref().map(str::trim).ok_or_else(|| {
                bad(
                    "schedule.timezone is required for kind `cron`, an IANA zone such as \
                         Asia/Manila",
                )
            })?;
            if zone.len() > MAX_TIMEZONE_CHARS {
                return Err(bad("schedule.timezone is too long"));
            }
            let timezone: Tz = zone.parse().map_err(|_| {
                bad(format!(
                    "unknown time zone `{zone}`; use an IANA zone such as Asia/Manila"
                ))
            })?;
            let cron = CronExpr::parse(expression).map_err(bad)?;
            if cron.min_spacing_minutes() < MIN_INTERVAL_MINUTES {
                return Err(bad(format!(
                    "a recurring task may fire at most every {MIN_INTERVAL_MINUTES} minutes"
                )));
            }
            if cron.next_after(timezone, now).is_none() {
                return Err(bad("this cron expression never fires"));
            }
            // Normalize whitespace so the stored expression is canonical.
            let expression = expression.split_whitespace().collect::<Vec<_>>().join(" ");
            Ok(Schedule::Cron {
                expression,
                timezone,
            })
        }
        other => Err(bad(format!(
            "schedule.kind must be `once` or `cron`, not `{other}`"
        ))),
    }
}

/// Both null for personal Chat, or both set for a Space room or thread.
fn check_target(
    space_id: Option<String>,
    room_id: Option<String>,
) -> Result<Option<(String, String)>, ScheduleError> {
    match (space_id, room_id) {
        (None, None) => Ok(None),
        (Some(s), Some(r)) => {
            if crate::config::validate_id(&s).is_err() {
                return Err(ScheduleError::NotFound("no such Space"));
            }
            if crate::config::validate_id(&r).is_err() {
                return Err(ScheduleError::NotFound("no such room"));
            }
            Ok(Some((s, r)))
        }
        (Some(_), None) => Err(bad("room_id is required with space_id")),
        (None, Some(_)) => Err(bad(
            "room_id needs space_id; use both null for personal Chat",
        )),
    }
}

/// Whether `owner` may send the agent work to `target` right now: personal
/// Chat always; a Space room only while they are a member of that Space and
/// the room (or thread) is in it.
fn target_access(
    db: &Db,
    owner: &str,
    target: &Option<(String, String)>,
) -> anyhow::Result<Access> {
    let Some((sid, rid)) = target else {
        return Ok(Access::Ok);
    };
    if db.membership(sid, owner)?.is_none() {
        return Ok(Access::NoSpace);
    }
    if db.get_room(sid, rid)?.is_none() {
        return Ok(Access::NoRoom);
    }
    Ok(Access::Ok)
}

#[derive(Debug, PartialEq, Eq)]
enum Access {
    Ok,
    NoSpace,
    NoRoom,
}

fn require_access(
    db: &Db,
    owner: &str,
    target: &Option<(String, String)>,
) -> Result<(), ScheduleError> {
    match target_access(db, owner, target)? {
        Access::Ok => Ok(()),
        Access::NoSpace => Err(ScheduleError::NotFound("no such Space")),
        Access::NoRoom => Err(ScheduleError::NotFound("no such room")),
    }
}

fn valid_task_id(id: &str) -> bool {
    id.len() == 4 + 32
        && id.starts_with("sch_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ---- Store ----

fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

fn from_ms(v: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(v).single().unwrap_or_default()
}

const TASK_COLUMNS: &str = "id, owner_id, name, prompt, space_id, room_id, kind, at_ms, expression, \
     timezone, enabled, next_run_at, revision, last_status, last_run_at, last_detail, created_at, \
     updated_at";

fn row_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let kind: String = r.get(6)?;
    let schedule = if kind == "once" {
        Schedule::Once {
            at: from_ms(r.get(7)?),
        }
    } else {
        let zone: String = r.get(9)?;
        Schedule::Cron {
            expression: r.get(8)?,
            timezone: zone.parse().map_err(|_| {
                rusqlite::Error::FromSqlConversionFailure(
                    9,
                    rusqlite::types::Type::Text,
                    format!("unknown time zone {zone}").into(),
                )
            })?,
        }
    };
    let space: Option<String> = r.get(4)?;
    let room: Option<String> = r.get(5)?;
    Ok(Task {
        id: r.get(0)?,
        owner_id: r.get(1)?,
        name: r.get(2)?,
        prompt: r.get(3)?,
        target: space.zip(room),
        schedule,
        enabled: r.get::<_, i64>(10)? != 0,
        next_run_at: r.get::<_, Option<i64>>(11)?.map(from_ms),
        revision: r.get(12)?,
        last_status: r.get(13)?,
        last_run_at: r.get::<_, Option<i64>>(14)?.map(from_ms),
        last_detail: r.get(15)?,
        created_at: from_ms(r.get(16)?),
        updated_at: from_ms(r.get(17)?),
    })
}

/// Task `id`, read on the caller's connection (or transaction).
pub(crate) fn get_task_in(conn: &Connection, id: &str) -> rusqlite::Result<Option<Task>> {
    get_task(conn, id)
}

fn get_task(conn: &Connection, id: &str) -> rusqlite::Result<Option<Task>> {
    conn.query_row(
        &format!("SELECT {TASK_COLUMNS} FROM schedules WHERE id = ?1"),
        [id],
        row_task,
    )
    .optional()
}

fn get_owned(conn: &Connection, owner: &str, id: &str) -> Result<Task, ScheduleError> {
    if !valid_task_id(id) {
        return Err(ScheduleError::NotFound(NO_TASK));
    }
    match get_task(conn, id)? {
        Some(t) if t.owner_id == owner => Ok(t),
        // Another person's task and a missing one look the same.
        _ => Err(ScheduleError::NotFound(NO_TASK)),
    }
}

fn schedule_columns(s: &Schedule) -> (&'static str, Option<i64>, Option<String>, Option<String>) {
    match s {
        Schedule::Once { at } => ("once", Some(ms(*at)), None, None),
        Schedule::Cron {
            expression,
            timezone,
        } => (
            "cron",
            None,
            Some(expression.clone()),
            Some(timezone.name().to_owned()),
        ),
    }
}

/// Create a task for `owner` as of `now`.
pub fn create_task(
    db: &Db,
    owner: &str,
    body: CreateBody,
    now: DateTime<Utc>,
    max_prompt_chars: usize,
) -> Result<Task, ScheduleError> {
    let name = check_name(&body.name)?;
    let prompt = check_prompt(&body.prompt, max_prompt_chars)?;
    let schedule = parse_schedule(&body.schedule, now)?;
    let target = check_target(body.space_id, body.room_id)?;
    require_access(db, owner, &target)?;
    let next = if body.enabled {
        schedule.next_after(now)
    } else {
        None
    };
    let id = format!("sch_{}", uuid::Uuid::new_v4().simple());
    let (kind, at, expression, timezone) = schedule_columns(&schedule);
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM schedules WHERE owner_id = ?1",
        [owner],
        |r| r.get(0),
    )?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= MAX_TASKS_PER_USER {
        return Err(ScheduleError::Conflict(format!(
            "scheduled task limit reached ({MAX_TASKS_PER_USER})"
        )));
    }
    let (space, room) = match &target {
        Some((s, r)) => (Some(s.as_str()), Some(r.as_str())),
        None => (None, None),
    };
    tx.execute(
        "INSERT INTO schedules (id, owner_id, name, prompt, space_id, room_id, kind, at_ms, \
         expression, timezone, enabled, next_run_at, revision, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13, ?13)",
        params![
            id,
            owner,
            name,
            prompt,
            space,
            room,
            kind,
            at,
            expression,
            timezone,
            body.enabled as i64,
            next.map(ms),
            ms(now),
        ],
    )?;
    let task = get_task(&tx, &id)?.context("reading the new task")?;
    tx.commit()?;
    Ok(task)
}

pub fn list_tasks(db: &Db, owner: &str) -> anyhow::Result<Vec<Task>> {
    let conn = db.lock();
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {TASK_COLUMNS} FROM schedules WHERE owner_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([owner], row_task)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Apply a partial edit as of `now`. Every edit bumps the revision, so an
/// occurrence claimed before it is cancelled rather than dispatched with
/// stale content. Pausing never needs Space access; anything that leaves
/// the task enabled re-checks it.
pub fn update_task(
    db: &Db,
    owner: &str,
    id: &str,
    body: PatchBody,
    now: DateTime<Utc>,
    max_prompt_chars: usize,
) -> Result<Task, ScheduleError> {
    let current = get_owned(&db.lock(), owner, id)?;
    let name = match &body.name {
        Some(n) => check_name(n)?,
        None => current.name.clone(),
    };
    let prompt = match &body.prompt {
        Some(p) => check_prompt(p, max_prompt_chars)?,
        None => current.prompt.clone(),
    };
    let (cur_space, cur_room) = match current.target.clone() {
        Some((s, r)) => (Some(s), Some(r)),
        None => (None, None),
    };
    let space = body.space_id.clone().unwrap_or(cur_space);
    let room = match (&body.room_id, &body.space_id) {
        (Some(r), _) => r.clone(),
        // Moving to personal Chat clears the room too.
        (None, Some(None)) => None,
        (None, _) => cur_room,
    };
    let target = check_target(space, room)?;
    let schedule_changed = body.schedule.is_some();
    let schedule = match &body.schedule {
        Some(s) => parse_schedule(s, now)?,
        None => current.schedule.clone(),
    };
    let enabled = body.enabled.unwrap_or(current.enabled);
    if enabled {
        require_access(db, owner, &target)?;
    }
    let next = if !enabled {
        None
    } else if schedule_changed || !current.enabled {
        let next = schedule.next_after(now);
        if next.is_none() && matches!(schedule, Schedule::Once { .. }) {
            return Err(bad(
                "this one-time task's time has passed; set a new schedule.at to enable it",
            ));
        }
        next
    } else {
        current.next_run_at
    };
    let (kind, at, expression, timezone) = schedule_columns(&schedule);
    let (space, room) = match &target {
        Some((s, r)) => (Some(s.as_str()), Some(r.as_str())),
        None => (None, None),
    };
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE schedules SET name = ?3, prompt = ?4, space_id = ?5, room_id = ?6, kind = ?7, \
         at_ms = ?8, expression = ?9, timezone = ?10, enabled = ?11, next_run_at = ?12, \
         revision = revision + 1, updated_at = ?13 \
         WHERE id = ?1 AND owner_id = ?2 AND revision = ?14",
        params![
            id,
            owner,
            name,
            prompt,
            space,
            room,
            kind,
            at,
            expression,
            timezone,
            enabled as i64,
            next.map(ms),
            ms(now),
            current.revision,
        ],
    )?;
    if changed == 0 {
        return Err(ScheduleError::Conflict(
            "the task changed while it was being edited; try again".to_owned(),
        ));
    }
    let task = get_task(&tx, id)?.context("reading the edited task")?;
    tx.commit()?;
    Ok(task)
}

/// Delete a task and its history. A claimed occurrence not yet submitted is
/// then cancelled at revalidation.
pub fn delete_task(db: &Db, owner: &str, id: &str) -> Result<(), ScheduleError> {
    let mut conn = db.lock();
    let tx = conn.transaction()?;
    get_owned(&tx, owner, id)?;
    tx.execute("DELETE FROM schedule_runs WHERE task_id = ?1", [id])?;
    tx.execute(
        "DELETE FROM schedules WHERE id = ?1 AND owner_id = ?2",
        params![id, owner],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn task_history(db: &Db, owner: &str, id: &str) -> Result<Vec<RunView>, ScheduleError> {
    let conn = db.lock();
    get_owned(&conn, owner, id)?;
    let mut stmt = conn.prepare_cached(
        "SELECT id, due_at, status, detail, claimed_at, finished_at FROM schedule_runs \
         WHERE task_id = ?1 AND owner_id = ?2 ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![id, owner, HISTORY_PER_TASK as i64], |r| {
        Ok(RunView {
            id: r.get(0)?,
            due_at: rfc3339(from_ms(r.get::<_, i64>(1)?.abs())),
            run_now: r.get::<_, i64>(1)? < 0,
            status: r.get(2)?,
            detail: r.get(3)?,
            claimed_at: rfc3339(from_ms(r.get(4)?)),
            finished_at: r.get::<_, Option<i64>>(5)?.map(|v| rfc3339(from_ms(v))),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn prune(conn: &Connection, task_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM schedule_runs WHERE task_id = ?1 AND id NOT IN \
         (SELECT id FROM schedule_runs WHERE task_id = ?1 ORDER BY id DESC LIMIT ?2)",
        params![task_id, HISTORY_PER_TASK as i64],
    )?;
    Ok(())
}

// ---- Firing ----

/// One claimed occurrence, with the task revision it was claimed at.
#[derive(Clone, Debug)]
pub struct Claim {
    pub run_id: i64,
    pub task_id: String,
    pub revision: i64,
    pub due_at: DateTime<Utc>,
}

/// What a tick did, for tests and logs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    pub dispatched: usize,
    pub errors: usize,
    pub unknown: usize,
    pub missed: usize,
    pub cancelled: usize,
    pub revoked: usize,
}

/// Claim every due occurrence as of `now`, atomically: record the claim
/// (or the miss) and advance `next_run_at` in one immediate transaction.
pub fn claim_due(db: &Db, now: DateTime<Utc>, limit: usize) -> anyhow::Result<(Vec<Claim>, usize)> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let due: Vec<Task> = {
        let mut stmt = tx.prepare_cached(&format!(
            "SELECT {TASK_COLUMNS} FROM schedules \
             WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1 \
             ORDER BY next_run_at, id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![ms(now), limit as i64], row_task)?;
        rows.collect::<Result<_, _>>()?
    };
    let mut claims = Vec::new();
    let mut missed = 0;
    for task in due {
        let Some(due_at) = task.next_run_at else {
            continue;
        };
        if now - due_at > MISSED_GRACE {
            let next = task.schedule.next_after(now);
            let detail = miss_detail(&task.schedule, due_at, now, next);
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO schedule_runs \
                 (task_id, owner_id, due_at, status, detail, claimed_at, finished_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![
                    task.id,
                    task.owner_id,
                    ms(due_at),
                    status::MISSED,
                    detail,
                    ms(now)
                ],
            )?;
            tx.execute(
                "UPDATE schedules SET next_run_at = ?2, last_status = ?3, last_run_at = ?4, \
                 last_detail = ?5 WHERE id = ?1 AND next_run_at = ?4",
                params![task.id, next.map(ms), status::MISSED, ms(due_at), detail],
            )?;
            if inserted == 1 {
                missed += 1;
            }
        } else {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO schedule_runs \
                 (task_id, owner_id, due_at, status, claimed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    task.id,
                    task.owner_id,
                    ms(due_at),
                    status::DISPATCHING,
                    ms(now)
                ],
            )?;
            let next = task.schedule.next_after(due_at.max(now));
            tx.execute(
                "UPDATE schedules SET next_run_at = ?2, last_status = ?3, last_run_at = ?4, \
                 last_detail = NULL WHERE id = ?1 AND next_run_at = ?4",
                params![task.id, next.map(ms), status::DISPATCHING, ms(due_at)],
            )?;
            // Zero rows: this occurrence was already claimed (another tick or
            // process); it has now been advanced past and is not submitted again.
            if inserted == 1 {
                claims.push(Claim {
                    run_id: tx.last_insert_rowid(),
                    task_id: task.id.clone(),
                    revision: task.revision,
                    due_at,
                });
            }
        }
        prune(&tx, &task.id)?;
    }
    tx.commit()?;
    Ok((claims, missed))
}

fn miss_detail(
    schedule: &Schedule,
    due_at: DateTime<Utc>,
    now: DateTime<Utc>,
    next: Option<DateTime<Utc>>,
) -> String {
    let mut count = 1usize;
    let mut cursor = due_at;
    while count <= MAX_COUNTED_MISSES {
        match schedule.next_after(cursor) {
            Some(t) if t <= now => {
                count += 1;
                cursor = t;
            }
            _ => break,
        }
    }
    let count = if count > MAX_COUNTED_MISSES {
        format!("more than {MAX_COUNTED_MISSES}")
    } else {
        count.to_string()
    };
    let next = match next {
        Some(t) => format!("next at {}", rfc3339(t)),
        None => "no further occurrence".to_owned(),
    };
    format!(
        "the server was not running at the scheduled time; {count} occurrence(s) since {} \
         were skipped and not run late; {next}",
        rfc3339(due_at)
    )
}

/// Mark occurrences still `dispatching` (their server stopped mid-submit)
/// as `interrupted`. They are never retried. Call once at startup.
pub fn recover_interrupted(db: &Db, now: DateTime<Utc>) -> anyhow::Result<usize> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let detail = "the server stopped while submitting this occurrence; the trigger may or may \
                  not have been posted, and it is not retried";
    tx.execute(
        "UPDATE schedules SET last_status = ?1, last_detail = ?2 WHERE last_status = ?3 AND id IN \
         (SELECT task_id FROM schedule_runs WHERE status = ?3 \
         AND ABS(due_at) = schedules.last_run_at)",
        params![status::INTERRUPTED, detail, status::DISPATCHING],
    )?;
    let n = tx.execute(
        "UPDATE schedule_runs SET status = ?1, detail = ?2, finished_at = ?3 WHERE status = ?4",
        params![status::INTERRUPTED, detail, ms(now), status::DISPATCHING],
    )?;
    tx.commit()?;
    Ok(n)
}

fn finish(
    db: &Db,
    claim: &Claim,
    from: &str,
    state: &str,
    detail: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let detail: String = detail.chars().take(MAX_DETAIL_CHARS).collect();
    let mut conn = db.lock();
    let tx = conn.transaction()?;
    let changed = tx.execute(
        "UPDATE schedule_runs SET status = ?2, detail = ?3, finished_at = ?4 \
         WHERE id = ?1 AND status = ?5",
        params![claim.run_id, state, detail, ms(now), from],
    )?;
    // Only a transition that happened, of the task's latest occurrence (by
    // run id: two occurrences can share a time), sets the task's summary.
    if changed == 1 {
        tx.execute(
            "UPDATE schedules SET last_status = ?2, last_detail = ?3 \
             WHERE id = ?1 AND (SELECT MAX(id) FROM schedule_runs WHERE task_id = ?1) = ?4",
            params![claim.task_id, state, detail, claim.run_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// The prompt submitter: [`crate::model_routing::submit_scheduled`] in the
/// server, a fake in tests. The future runs in its own task, so a slow
/// submission is never cancelled by the scheduler's timeout.
pub trait Submitter: Send + Sync {
    fn submit(
        &self,
        state: AppState,
        owner_id: String,
        space: Option<(String, String)>,
        prompt: String,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;
}

/// Submits through the interactive model routing.
pub struct Routing;

impl Submitter for Routing {
    fn submit(
        &self,
        state: AppState,
        owner_id: String,
        space: Option<(String, String)>,
        prompt: String,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>> {
        Box::pin(async move {
            let space = space.as_ref().map(|(s, r)| (s.as_str(), r.as_str()));
            crate::model_routing::submit_scheduled(&state, &owner_id, space, &prompt).await
        })
    }
}

enum Outcome {
    Dispatched,
    Error(String),
    Unknown,
    Cancelled(&'static str),
    Revoked(&'static str),
}

/// Revalidate one claimed occurrence and submit it.
async fn fire(
    state: &AppState,
    claim: &Claim,
    submit: &dyn Submitter,
    timeout: StdDuration,
) -> anyhow::Result<Outcome> {
    let task_id = claim.task_id.clone();
    let task = state
        .db
        .call(move |db| Ok(get_task(&db.lock(), &task_id)?))
        .await?;
    let Some(task) = task else {
        return Ok(Outcome::Cancelled("the task was deleted before dispatch"));
    };
    if !task.enabled {
        return Ok(Outcome::Cancelled("the task was paused before dispatch"));
    }
    if task.revision != claim.revision {
        return Ok(Outcome::Cancelled(
            "the task was edited after this occurrence was claimed; it was not sent",
        ));
    }
    let (owner, target) = (task.owner_id.clone(), task.target.clone());
    match state
        .db
        .call(move |db| target_access(db, &owner, &target))
        .await?
    {
        Access::Ok => {}
        Access::NoSpace => {
            return Ok(Outcome::Revoked(
                "the owner is no longer a member of the task's Space; the task was paused",
            ));
        }
        Access::NoRoom => {
            return Ok(Outcome::Revoked(
                "the task's room no longer exists in its Space; the task was paused",
            ));
        }
    }
    let mut handle = tokio::spawn(submit.submit(
        state.clone(),
        task.owner_id.clone(),
        task.target.clone(),
        task.prompt.clone(),
    ));
    match tokio::time::timeout(timeout, &mut handle).await {
        Ok(joined) => Ok(submitted(joined)),
        Err(_) => {
            // `unknown` is committed before the late completion is handed
            // off, so a late result always lands on top of it.
            let (db, c) = (state.db.clone(), claim.clone());
            let (st, detail) = describe(&Outcome::Unknown, timeout);
            db.call(move |db| finish(db, &c, status::DISPATCHING, st, &detail, Utc::now()))
                .await?;
            let (db, claim) = (state.db.clone(), claim.clone());
            tokio::spawn(async move {
                let (st, detail) = describe(&submitted(handle.await), timeout);
                let result = db
                    .call(move |db| finish(db, &claim, status::UNKNOWN, st, &detail, Utc::now()))
                    .await;
                if let Err(e) = result {
                    tracing::error!(error = %format!("{e:#}"), "recording a late scheduled result");
                }
            });
            Ok(Outcome::Unknown)
        }
    }
}

fn submitted(joined: Result<anyhow::Result<()>, tokio::task::JoinError>) -> Outcome {
    match joined {
        Ok(Ok(())) => Outcome::Dispatched,
        Ok(Err(e)) => Outcome::Error(format!("{e:#}")),
        Err(e) => Outcome::Error(format!("the submission failed: {e}")),
    }
}

fn describe(outcome: &Outcome, timeout: StdDuration) -> (&'static str, String) {
    match outcome {
        Outcome::Dispatched => (
            status::DISPATCHED,
            "the prompt was posted and agent work was accepted; the agent's result, or its \
             error, is in the conversation"
                .to_owned(),
        ),
        Outcome::Error(e) => (status::ERROR, e.clone()),
        Outcome::Unknown => (
            status::UNKNOWN,
            format!(
                "the submission had not returned after {}s; it was left running and is not \
                 retried, and this entry is updated when it returns",
                timeout.as_secs()
            ),
        ),
        Outcome::Cancelled(d) => (status::CANCELLED, (*d).to_owned()),
        Outcome::Revoked(d) => (status::REVOKED, (*d).to_owned()),
    }
}

/// Pause a task whose access was revoked and tell its owner in their
/// personal Chat, so the failure is visible where they read.
fn revoke(db: &Db, claim: &Claim, detail: &str) -> anyhow::Result<()> {
    let task = {
        let conn = db.lock();
        conn.execute(
            "UPDATE schedules SET enabled = 0, next_run_at = NULL, revision = revision + 1 \
             WHERE id = ?1 AND revision = ?2",
            params![claim.task_id, claim.revision],
        )?;
        get_task(&conn, &claim.task_id)?
    };
    if let Some(task) = task {
        let text = format!(
            "Scheduled task \"{}\" did not run at {}: {detail}.",
            task.name,
            rfc3339(claim.due_at)
        );
        db.append_personal(
            &task.owner_id,
            kind::ERROR,
            &Author::server(actor::SYSTEM),
            &text,
            &Addressed::room(),
        )?;
    }
    Ok(())
}

/// One scheduler pass at wall-clock `now`, submitting through `submit`.
pub async fn tick_with(
    state: &AppState,
    now: DateTime<Utc>,
    submit: &dyn Submitter,
) -> anyhow::Result<TickReport> {
    tick_inner(state, now, submit, SUBMIT_TIMEOUT).await
}

pub(crate) async fn tick_inner(
    state: &AppState,
    now: DateTime<Utc>,
    submit: &dyn Submitter,
    timeout: StdDuration,
) -> anyhow::Result<TickReport> {
    let mut report = TickReport::default();
    for _ in 0..CLAIM_ROUNDS {
        let (claims, missed) = state
            .db
            .call(move |db| claim_due(db, now, CLAIM_BATCH))
            .await?;
        report.missed += missed;
        let full = claims.len() == CLAIM_BATCH;
        process_claims(state, now, &claims, submit, timeout, &mut report).await;
        if !full {
            break;
        }
    }
    Ok(report)
}

/// Revalidate, submit and record already-claimed occurrences.
pub(crate) async fn process_claims(
    state: &AppState,
    now: DateTime<Utc>,
    claims: &[Claim],
    submit: &dyn Submitter,
    timeout: StdDuration,
    report: &mut TickReport,
) {
    {
        let outcomes = futures::future::join_all(claims.iter().map(|claim| async move {
            let outcome = match fire(state, claim, submit, timeout).await {
                Ok(o) => o,
                Err(e) => Outcome::Error(format!("checking the task failed: {e:#}")),
            };
            (claim.clone(), outcome)
        }))
        .await;
        for (claim, outcome) in outcomes {
            match &outcome {
                Outcome::Dispatched => report.dispatched += 1,
                Outcome::Error(_) => report.errors += 1,
                Outcome::Unknown => report.unknown += 1,
                Outcome::Cancelled(_) => report.cancelled += 1,
                Outcome::Revoked(_) => report.revoked += 1,
            }
            let (st, detail) = describe(&outcome, timeout);
            let revoked = matches!(outcome, Outcome::Revoked(_));
            let finished = Utc::now().max(now);
            let result = state
                .db
                .call(move |db| {
                    finish(db, &claim, status::DISPATCHING, st, &detail, finished)?;
                    if revoked {
                        revoke(db, &claim, &detail)?;
                    }
                    Ok(())
                })
                .await;
            if let Err(e) = result {
                tracing::error!(error = %format!("{e:#}"), "recording a scheduled occurrence");
            }
        }
    }
}

// ---- Run now ----

/// Claim a "run now" occurrence of `owner`'s task `id` at `now`: a
/// `schedule_runs` row like any occurrence (unique on its due time), and
/// the task's last-run summary. Its schedule (`next_run_at`) is unchanged,
/// and a paused task stays paused.
/// Whether `owner` may send work to `target` now, read on `conn`.
fn access_in(
    conn: &Connection,
    owner: &str,
    target: &Option<(String, String)>,
) -> rusqlite::Result<Access> {
    let Some((sid, rid)) = target else {
        return Ok(Access::Ok);
    };
    let member: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)",
        params![sid, owner],
        |r| r.get(0),
    )?;
    if !member {
        return Ok(Access::NoSpace);
    }
    let room: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM space_rooms WHERE space_id = ?1 AND id = ?2)",
        params![sid, rid],
        |r| r.get(0),
    )?;
    Ok(if room { Access::Ok } else { Access::NoRoom })
}

/// Claim a "run now" occurrence of `owner`'s task `id` at `now`, on the
/// caller's transaction, and return it with the exact task snapshot to
/// dispatch. Refused (with nothing written) unless the task is theirs,
/// still at `expected_revision` when given, targets `required_space` when
/// given, and its Space and room are still theirs. The occurrence is a
/// `schedule_runs` row like any other (in its own namespace, see below);
/// the task's schedule is unchanged.
pub(crate) fn claim_now_in(
    conn: &Connection,
    owner: &str,
    id: &str,
    expected_revision: Option<i64>,
    required_space: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(Claim, Task), ScheduleError> {
    let task = get_owned(conn, owner, id)?;
    if expected_revision.is_some_and(|r| r != task.revision) {
        return Err(ScheduleError::Conflict(
            "the task changed since it was shown; review it and try again".to_owned(),
        ));
    }
    if let Some(sid) = required_space
        && task.target.as_ref().is_none_or(|(s, _)| s != sid)
    {
        return Err(ScheduleError::Conflict(
            "this task doesn't run in this Space".to_owned(),
        ));
    }
    match access_in(conn, owner, &task.target)? {
        Access::Ok => {}
        Access::NoSpace => {
            return Err(ScheduleError::Conflict(
                "you're no longer a member of this task's Space; it can't run".to_owned(),
            ));
        }
        Access::NoRoom => {
            return Err(ScheduleError::Conflict(
                "this task's room no longer exists; it can't run".to_owned(),
            ));
        }
    }
    // Run-now rows live in their own namespace: `due_at` is the negated
    // time, and a scheduled occurrence's `due_at` is never negative, so a
    // run now can never take the unique (task, due_at) slot of a real
    // occurrence (which `claim_due` inserts with INSERT OR IGNORE).
    let mut key = -ms(now).max(1);
    let run_id = loop {
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO schedule_runs \
             (task_id, owner_id, due_at, status, claimed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![task.id, task.owner_id, key, status::DISPATCHING, ms(now)],
        )?;
        if inserted == 1 {
            break conn.last_insert_rowid();
        }
        key -= 1;
    };
    let due = ms(now);
    conn.execute(
        "UPDATE schedules SET last_status = ?2, last_run_at = ?3, last_detail = NULL WHERE id = ?1",
        params![task.id, status::DISPATCHING, due],
    )?;
    prune(conn, &task.id)?;
    Ok((
        Claim {
            run_id,
            task_id: task.id.clone(),
            revision: task.revision,
            due_at: from_ms(due),
        },
        task,
    ))
}

/// [`claim_now_in`] in its own transaction.
fn claim_now(
    db: &Db,
    owner: &str,
    id: &str,
    expected_revision: Option<i64>,
    now: DateTime<Utc>,
) -> Result<(Claim, Task), ScheduleError> {
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let out = claim_now_in(&tx, owner, id, expected_revision, None, now)?;
    tx.commit()?;
    Ok(out)
}

fn run_view(db: &Db, run_id: i64) -> anyhow::Result<Option<RunView>> {
    let conn = db.lock();
    Ok(conn
        .query_row(
            "SELECT id, due_at, status, detail, claimed_at, finished_at FROM schedule_runs \
             WHERE id = ?1",
            [run_id],
            |r| {
                Ok(RunView {
                    id: r.get(0)?,
                    due_at: rfc3339(from_ms(r.get::<_, i64>(1)?.abs())),
                    run_now: r.get::<_, i64>(1)? < 0,
                    status: r.get(2)?,
                    detail: r.get(3)?,
                    claimed_at: rfc3339(from_ms(r.get(4)?)),
                    finished_at: r.get::<_, Option<i64>>(5)?.map(|v| rfc3339(from_ms(v))),
                })
            },
        )
        .optional()?)
}

/// "Run now": submit `owner`'s task `id` once, immediately, through the
/// same submission path as a scheduled occurrence, and record it in its
/// history. Only the task's owner can; another person's task and a missing
/// one both answer 404. Access to the task's Space and room is rechecked
/// first: without it nothing is recorded or sent (409).
pub async fn run_now_with(
    state: &AppState,
    owner: &str,
    id: &str,
    expected_revision: Option<i64>,
    submit: &dyn Submitter,
    timeout: StdDuration,
) -> Result<RunView, ScheduleError> {
    let (o, i) = (owner.to_owned(), id.to_owned());
    let (claim, task) = state
        .db
        .call(move |db| Ok(claim_now(db, &o, &i, expected_revision, Utc::now())))
        .await??;
    dispatch_claimed(state, claim, task, submit, timeout).await
}

/// Submit exactly the claimed snapshot `task` for `claim` and record the
/// outcome. A submission still running at `timeout` is committed as
/// `unknown` first; its late result then replaces that.
pub(crate) async fn dispatch_claimed(
    state: &AppState,
    claim: Claim,
    task: Task,
    submit: &dyn Submitter,
    timeout: StdDuration,
) -> Result<RunView, ScheduleError> {
    let mut handle = tokio::spawn(submit.submit(
        state.clone(),
        task.owner_id.clone(),
        task.target.clone(),
        task.prompt.clone(),
    ));
    let outcome = match tokio::time::timeout(timeout, &mut handle).await {
        Ok(joined) => submitted(joined),
        Err(_) => {
            let (st, detail) = describe(&Outcome::Unknown, timeout);
            let detail = format!("Run now: {detail}");
            let c = claim.clone();
            state
                .db
                .call(move |db| finish(db, &c, status::DISPATCHING, st, &detail, Utc::now()))
                .await?;
            let (db, late) = (state.db.clone(), claim.clone());
            tokio::spawn(async move {
                let (st, detail) = describe(&submitted(handle.await), timeout);
                let detail = format!("Run now: {detail}");
                let result = db
                    .call(move |db| finish(db, &late, status::UNKNOWN, st, &detail, Utc::now()))
                    .await;
                if let Err(e) = result {
                    tracing::error!(error = %format!("{e:#}"), "recording a late run-now result");
                }
            });
            Outcome::Unknown
        }
    };
    let (st, detail) = describe(&outcome, timeout);
    let detail = format!("Run now: {detail}");
    let claim_id = claim.run_id;
    state
        .db
        .call(move |db| {
            // A no-op when `unknown` was already committed above.
            finish(db, &claim, status::DISPATCHING, st, &detail, Utc::now())?;
            run_view(db, claim_id)
        })
        .await?
        .ok_or_else(|| ScheduleError::NotFound(NO_TASK))
}

/// [`run_now_with`] through the interactive routing.
pub async fn run_now_for(
    state: &AppState,
    owner: &str,
    id: &str,
    expected_revision: Option<i64>,
) -> Result<RunView, ScheduleError> {
    run_now_with(
        state,
        owner,
        id,
        expected_revision,
        &Routing,
        SUBMIT_TIMEOUT,
    )
    .await
}

/// [`dispatch_claimed`] through the interactive routing.
pub(crate) async fn dispatch_claimed_for(
    state: &AppState,
    claim: Claim,
    task: Task,
) -> Result<RunView, ScheduleError> {
    dispatch_claimed(state, claim, task, &Routing, SUBMIT_TIMEOUT).await
}

/// The earliest pending firing of any enabled task.
pub fn earliest_due(db: &Db) -> anyhow::Result<Option<DateTime<Utc>>> {
    let conn = db.lock();
    let v: Option<i64> = conn.query_row(
        "SELECT MIN(next_run_at) FROM schedules WHERE enabled = 1",
        [],
        |r| r.get(0),
    )?;
    Ok(v.map(from_ms))
}

/// Wakes the scheduler when a task is created or edited, so a near firing
/// is not delayed by a full tick interval.
static WAKE: std::sync::OnceLock<std::sync::Arc<tokio::sync::Notify>> = std::sync::OnceLock::new();

fn wake() {
    if let Some(n) = WAKE.get() {
        n.notify_one();
    }
}

/// The tick interval: `METRO_SCHEDULE_TICK_SECS` (1..=300), default 30 s.
pub fn tick_interval() -> StdDuration {
    std::env::var(TICK_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|s| StdDuration::from_secs(s.clamp(1, 300)))
        .unwrap_or(DEFAULT_TICK)
}

/// Start the scheduler. Call once, after the state is built.
pub fn spawn(state: AppState) {
    let every = tick_interval();
    tokio::spawn(async move {
        match state
            .db
            .call(|db| recover_interrupted(db, Utc::now()))
            .await
        {
            Ok(0) => {}
            Ok(n) => tracing::warn!(
                occurrences = n,
                "scheduled occurrences were interrupted by the last shutdown; not retried"
            ),
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "recovering scheduled occurrences");
                state
                    .monitor(
                        crate::monitoring::Scope::Instance,
                        crate::monitoring::Code::SchedulerRecoveryFailed,
                    )
                    .await;
            }
        }
        let notify = WAKE.get_or_init(Default::default).clone();
        loop {
            match tick_with(&state, Utc::now(), &Routing).await {
                Ok(r) if r != TickReport::default() => {
                    tracing::info!(?r, "scheduled prompts");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), "scheduler tick");
                    state
                        .monitor(
                            crate::monitoring::Scope::Instance,
                            crate::monitoring::Code::SchedulerTickFailed,
                        )
                        .await;
                }
            }
            // Sleep until the earliest firing, at most one tick interval,
            // and wake early when a task changes.
            let wait = match state.db.call(earliest_due).await {
                Ok(Some(due)) => (due - Utc::now())
                    .to_std()
                    .map(|d| d + StdDuration::from_millis(20))
                    .unwrap_or(MIN_WAIT)
                    .clamp(MIN_WAIT, every),
                Ok(None) => every,
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), "reading the next firing");
                    every
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = notify.notified() => {}
            }
        }
    });
}

// ---- Handlers ----

fn limit(state: &AppState) -> usize {
    state.config.limits.max_message_chars
}

async fn list(State(state): State<AppState>, who: Human) -> ApiResult<Json<serde_json::Value>> {
    let owner = who.id.clone();
    let tasks = crate::api::db(&state, move |db| list_tasks(db, &owner)).await?;
    let tasks: Vec<_> = tasks.iter().map(Task::view).collect();
    Ok(Json(json!({"tasks": tasks})))
}

async fn create(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<CreateBody>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let Json(body) = body?;
    let (owner, max) = (who.id.clone(), limit(&state));
    let task = state
        .db
        .call(move |db| Ok(create_task(db, &owner, body, Utc::now(), max)))
        .await
        .map_err(ApiError::internal)??;
    wake();
    Ok((StatusCode::CREATED, Json(task.view())))
}

async fn update(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: JsonBody<PatchBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let Json(body) = body?;
    let (owner, max) = (who.id.clone(), limit(&state));
    let task = state
        .db
        .call(move |db| Ok(update_task(db, &owner, &id, body, Utc::now(), max)))
        .await
        .map_err(ApiError::internal)??;
    wake();
    Ok(Json(task.view()))
}

async fn remove(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let owner = who.id.clone();
    state
        .db
        .call(move |db| Ok(delete_task(db, &owner, &id)))
        .await
        .map_err(ApiError::internal)??;
    Ok(StatusCode::NO_CONTENT)
}

async fn history(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let owner = who.id.clone();
    let runs = state
        .db
        .call(move |db| Ok(task_history(db, &owner, &id)))
        .await
        .map_err(ApiError::internal)??;
    Ok(Json(json!({"runs": runs})))
}

async fn run_now(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    /// Optionally, the revision the person saw: a task edited since then
    /// isn't run (409).
    #[derive(Deserialize, Default)]
    #[serde(deny_unknown_fields)]
    struct RunBody {
        #[serde(default)]
        revision: Option<i64>,
    }
    let body: RunBody = if body.iter().all(u8::is_ascii_whitespace) {
        RunBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|_| ApiError::bad_request("body must be {} or {\"revision\": n}"))?
    };
    let run = run_now_for(&state, &who.id, &id, body.revision).await?;
    Ok(Json(json!({"run": run})))
}
