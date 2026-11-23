//! Remote monitoring: a bounded, persistent log of allowlisted error events.
//!
//! # What is stored
//!
//! A row holds only a scope, a fixed [`Code`] (component and code) and the
//! server time it happened. There is no free-text column: no error message
//! from a model service, device, runner or person, no prompt, path, URL,
//! query, token or user id is ever written. The summary a reader sees is the
//! fixed sentence of the code, looked up when the log is read, and a row
//! whose code this build does not know is not returned.
//!
//! # Scope
//!
//! Every row is scoped when it is written:
//!
//! - **Space** events (agent runs, their model connections, the Space's
//!   runner) carry the Space id. Only the Space's *current* owner reads them
//!   (`GET /v1/monitoring/events?space_id=SID`); outsiders and unknown ids
//!   get 404 and members 403, checked against the database on every read.
//! - **Instance** events (the server starting, the scheduler failing) carry
//!   no Space. Only organization admins read them (no `space_id`). They are
//!   operational only: nothing about a person, their personal Chat or any
//!   Space is written there, and no Space event is ever folded into them.
//!
//! Personal Chat turns are never recorded at all: they are private to one
//! person, and an admin log is not the place for them.
//!
//! # Retention
//!
//! Both by age and by count, and it survives restarts because it is the
//! database itself: rows older than [`Limits::monitoring_max_age`] are not
//! returned and are deleted on the next write and at startup; each scope
//! keeps its newest [`Limits::monitoring_events_per_scope`] rows, and the
//! whole table at most [`MAX_TOTAL_EVENTS`].
//!
//! [`Limits::monitoring_max_age`]: crate::config::Limits::monitoring_max_age
//! [`Limits::monitoring_events_per_scope`]: crate::config::Limits::monitoring_events_per_scope

use std::time::Duration;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::{self, ApiError, ApiResult, owner_of};
use crate::auth::Human;
use crate::db::{Db, now_ms, rfc3339_utc};
use crate::jobs::JobError;

/// Most events one read returns.
pub const PAGE: usize = 100;
/// Most rows kept across every scope together.
pub const MAX_TOTAL_EVENTS: usize = 100_000;

pub(crate) fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS monitoring_events (
             id         INTEGER PRIMARY KEY AUTOINCREMENT,
             space_id   TEXT,
             component  TEXT NOT NULL,
             code       TEXT NOT NULL,
             created_at INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS monitoring_events_scope ON monitoring_events (space_id, id);
         CREATE INDEX IF NOT EXISTS monitoring_events_age ON monitoring_events (created_at);",
    )
}

/// Who may read an event: decided when it is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The Space's current owner.
    Space(String),
    /// Organization admins: operational events of this server only.
    Instance,
}

impl Scope {
    fn space_id(&self) -> Option<&str> {
        match self {
            Scope::Space(id) => Some(id),
            Scope::Instance => None,
        }
    }
}

macro_rules! codes {
    ($( $variant:ident => ($component:literal, $code:literal, $summary:literal), )*) => {
        /// Every event Metro records. The allowlist: nothing else is written
        /// or returned.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Code { $( $variant, )* }

        impl Code {
            pub const ALL: &'static [Code] = &[ $( Code::$variant, )* ];

            pub fn component(self) -> &'static str {
                match self { $( Code::$variant => $component, )* }
            }
            pub fn code(self) -> &'static str {
                match self { $( Code::$variant => $code, )* }
            }
            pub fn summary(self) -> &'static str {
                match self { $( Code::$variant => $summary, )* }
            }
            pub fn parse(component: &str, code: &str) -> Option<Code> {
                match (component, code) {
                    $( ($component, $code) => Some(Code::$variant), )*
                    _ => None,
                }
            }
        }
    };
}

codes! {
    // Space: agent runs.
    AgentRunFailed => ("agent", "run_failed", "An agent run failed."),
    AgentTimedOut => ("agent", "timed_out", "An agent run took too long and was stopped."),
    AgentTurnLimit => ("agent", "turn_limit", "An agent run reached its limit of model calls."),
    AgentInvalidToolCall => ("agent", "invalid_tool_call", "The model asked for a tool it may not use."),
    AgentHistoryUnavailable => ("agent", "history_unavailable", "An agent run could not read the room history."),
    AgentBusy => ("agent", "busy", "An agent run was refused because the agent was busy."),
    // Space: the model connection a run used.
    ProviderUnavailable => ("provider", "unavailable", "The model connection for an agent run could not be used."),
    ProviderUnreachable => ("provider", "unreachable", "The model service could not be reached or did not answer in time."),
    ProviderAuthRejected => ("provider", "auth_rejected", "The model service rejected the connection's credentials."),
    ProviderRateLimited => ("provider", "rate_limited", "The model service is limiting requests."),
    ProviderServerError => ("provider", "server_error", "The model service reported a server error."),
    ProviderRejected => ("provider", "rejected", "The model service refused a request."),
    ProviderBadResponse => ("provider", "bad_response", "The model service sent a reply Metro could not use."),
    ProviderFailed => ("provider", "failed", "A model call failed."),
    // Space: its runner.
    RunnerQueueFull => ("runner", "queue_full", "The Space's runner queue was full; a tool call was refused."),
    RunnerNotClaimed => ("runner", "not_claimed", "No runner picked up a tool call in time."),
    RunnerTimedOut => ("runner", "timed_out", "The runner did not return a tool result in time."),
    RunnerChanged => ("runner", "changed", "A tool call was cancelled because the Space's runner changed."),
    // Instance.
    ServerStarted => ("server", "started", "The server started."),
    SchedulerTickFailed => ("scheduler", "tick_failed", "The scheduler could not check scheduled prompts."),
    SchedulerRecoveryFailed => ("scheduler", "recovery_failed", "The scheduler could not recover occurrences interrupted by a restart."),
}

impl From<&JobError> for Code {
    fn from(e: &JobError) -> Self {
        match e {
            JobError::QueueFull => Code::RunnerQueueFull,
            JobError::NotClaimed => Code::RunnerNotClaimed,
            JobError::TimedOut => Code::RunnerTimedOut,
            JobError::CredentialChanged => Code::RunnerChanged,
        }
    }
}

/// The code of a model call's failure, from its classification only (its
/// kind and HTTP status), never from its text.
pub fn provider_code(report: &rig_core::ErrorReport) -> Code {
    use rig_core::ErrorKind as K;
    match report.kind {
        K::Http | K::Timeout => Code::ProviderUnreachable,
        K::Json | K::Response => Code::ProviderBadResponse,
        K::ProviderResponse => match report.http_status {
            Some(401 | 403) => Code::ProviderAuthRejected,
            Some(429) => Code::ProviderRateLimited,
            Some(500..=599) => Code::ProviderServerError,
            _ => Code::ProviderRejected,
        },
        _ => Code::ProviderFailed,
    }
}

/// How long and how many events are kept.
#[derive(Clone, Copy, Debug)]
pub struct Retention {
    pub per_scope: usize,
    pub max_age: Duration,
}

impl Retention {
    pub fn of(limits: &crate::config::Limits) -> Self {
        Self {
            per_scope: limits.monitoring_events_per_scope.max(1),
            max_age: limits.monitoring_max_age,
        }
    }

    fn cutoff(self, now: i64) -> i64 {
        now.saturating_sub(i64::try_from(self.max_age.as_millis()).unwrap_or(i64::MAX))
    }
}

/// One event as read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MonitoringEvent {
    /// RFC 3339 UTC.
    pub timestamp: String,
    pub component: &'static str,
    pub code: &'static str,
    pub summary: &'static str,
}

/// Write one event at `at` (epoch ms) and apply retention to its scope.
pub fn record_at(
    db: &Db,
    scope: &Scope,
    code: Code,
    at: i64,
    retention: Retention,
) -> anyhow::Result<()> {
    let mut conn = db.lock();
    let tx = conn.transaction()?;
    let space = scope.space_id();
    tx.execute(
        "INSERT INTO monitoring_events (space_id, component, code, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![space, code.component(), code.code(), at],
    )?;
    tx.execute(
        "DELETE FROM monitoring_events WHERE created_at < ?1",
        [retention.cutoff(now_ms())],
    )?;
    tx.execute(
        "DELETE FROM monitoring_events WHERE space_id IS ?1 AND id <= (
             SELECT id FROM monitoring_events WHERE space_id IS ?1
             ORDER BY id DESC LIMIT 1 OFFSET ?2)",
        params![space, retention.per_scope as i64],
    )?;
    tx.execute(
        "DELETE FROM monitoring_events WHERE id <= (
             SELECT id FROM monitoring_events ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [MAX_TOTAL_EVENTS as i64],
    )?;
    tx.commit()?;
    Ok(())
}

/// Apply retention to every scope (at startup).
pub fn prune(db: &Db, retention: Retention) -> anyhow::Result<usize> {
    let conn = db.lock();
    let mut n = conn.execute(
        "DELETE FROM monitoring_events WHERE created_at < ?1",
        [retention.cutoff(now_ms())],
    )?;
    n += conn.execute(
        "DELETE FROM monitoring_events WHERE id IN (
             SELECT id FROM (
                 SELECT id, ROW_NUMBER() OVER (PARTITION BY space_id ORDER BY id DESC) AS rn
                 FROM monitoring_events)
             WHERE rn > ?1)",
        [retention.per_scope as i64],
    )?;
    n += conn.execute(
        "DELETE FROM monitoring_events WHERE id <= (
             SELECT id FROM monitoring_events ORDER BY id DESC LIMIT 1 OFFSET ?1)",
        [MAX_TOTAL_EVENTS as i64],
    )?;
    Ok(n)
}

/// The newest events of `scope` within retention, newest first, at most
/// [`PAGE`]. The caller must have authorized the reader for `scope`.
pub fn list(db: &Db, scope: &Scope, retention: Retention) -> anyhow::Result<Vec<MonitoringEvent>> {
    let conn = db.lock();
    let mut stmt = conn.prepare(
        "SELECT component, code, created_at FROM monitoring_events
         WHERE space_id IS ?1 AND created_at >= ?2
         ORDER BY id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(
        params![
            scope.space_id(),
            retention.cutoff(now_ms()),
            (PAGE * 2) as i64
        ],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (component, code, at) = row?;
        // The allowlist again: a row this build does not know is not shown.
        let (Some(code), Some(timestamp)) = (Code::parse(&component, &code), rfc3339_utc(at))
        else {
            continue;
        };
        out.push(MonitoringEvent {
            timestamp,
            component: code.component(),
            code: code.code(),
            summary: code.summary(),
        });
        if out.len() == PAGE {
            break;
        }
    }
    Ok(out)
}

impl AppState {
    /// Record `code` in `scope` now. Never fails the caller: a monitoring
    /// write that fails is logged (without any event text) and dropped.
    pub async fn monitor(&self, scope: Scope, code: Code) {
        let retention = Retention::of(&self.config.limits);
        let r = self
            .db
            .call(move |db| record_at(db, &scope, code, now_ms(), retention))
            .await;
        if let Err(e) = r {
            tracing::error!(error = %format!("{e:#}"), "recording a monitoring event");
        }
    }

    /// [`AppState::monitor`] for a Space.
    pub async fn monitor_space(&self, space_id: &str, code: Code) {
        self.monitor(Scope::Space(space_id.to_owned()), code).await;
    }
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/v1/monitoring/events", get(events))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    space_id: Option<String>,
}

/// `GET /v1/monitoring/events[?space_id=SID]`.
async fn events(
    State(state): State<AppState>,
    who: Human,
    query: Result<Query<EventsQuery>, QueryRejection>,
) -> ApiResult<Response> {
    let Query(query) = query?;
    let scope = match query.space_id {
        // 404 to outsiders and unknown ids, 403 to members; read from the
        // database on this request.
        Some(sid) => Scope::Space(owner_of(&state, &sid, &who).await?.id),
        None => {
            if crate::admin_users::is_managed_id(&who.id) || !state.config.is_org_admin(&who.id) {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "only organization admins can read this server's events",
                ));
            }
            Scope::Instance
        }
    };
    let retention = Retention::of(&state.config.limits);
    let events = api::db(&state, move |db| list(db, &scope, retention)).await?;
    let mut response = Json(serde_json::json!({ "events": events })).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for &c in Code::ALL {
            assert_eq!(Code::parse(c.component(), c.code()), Some(c));
            assert!(seen.insert((c.component(), c.code())), "{c:?}");
            assert!(c.summary().ends_with('.'));
        }
        assert_eq!(Code::parse("agent", "anything else"), None);
    }

    fn retention(per_scope: usize) -> Retention {
        Retention {
            per_scope,
            max_age: Duration::from_secs(3600),
        }
    }

    #[test]
    fn scopes_are_kept_apart_and_bounded() {
        let db = Db::open_in_memory().unwrap();
        let r = retention(3);
        let a = Scope::Space("a".into());
        let b = Scope::Space("b".into());
        for _ in 0..5 {
            record_at(&db, &a, Code::RunnerNotClaimed, now_ms(), r).unwrap();
        }
        record_at(&db, &b, Code::AgentRunFailed, now_ms(), r).unwrap();
        record_at(&db, &Scope::Instance, Code::ServerStarted, now_ms(), r).unwrap();
        assert_eq!(list(&db, &a, r).unwrap().len(), 3);
        let b_events = list(&db, &b, r).unwrap();
        assert_eq!(b_events.len(), 1);
        assert_eq!(b_events[0].code, "run_failed");
        let inst = list(&db, &Scope::Instance, r).unwrap();
        assert_eq!(inst.len(), 1);
        assert_eq!(inst[0].component, "server");
    }

    #[test]
    fn old_and_unknown_rows_are_not_returned() {
        let db = Db::open_in_memory().unwrap();
        let r = retention(10);
        let a = Scope::Space("a".into());
        record_at(&db, &a, Code::AgentTimedOut, now_ms(), r).unwrap();
        db.lock()
            .execute(
                "INSERT INTO monitoring_events (space_id, component, code, created_at)
                 VALUES ('a', 'agent', 'raw text from somewhere', ?1),
                        ('a', 'agent', 'run_failed', ?2)",
                params![now_ms(), now_ms() - 2 * 3600 * 1000],
            )
            .unwrap();
        let events = list(&db, &a, r).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].code, "timed_out");
        assert_eq!(prune(&db, r).unwrap(), 1);
    }
}
