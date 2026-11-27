//! `workspace.shell`: who may run what, and approvals.
//!
//! **Policy.** A registered runner has one default (`shell_mode`,
//! `approver`) and optional per-Space overrides (`NULL` inherits), read and
//! changed only by whoever manages the runner. A dedicated runner has one
//! policy per Space, managed by the Space owner. Defaults: `approve`,
//! `owner`. The desktop's built-in runner (`runners.built_in`) gets `allow`
//! only while it is attached to exactly one Space; otherwise the effective
//! mode is `approve`. `classify` needs one owner's System One connection, so
//! organization runners can't save it.
//!
//! **Approvals.** An approval waits in memory (with its `shell_approval`
//! event as the record) for 10 minutes, until someone eligible decides,
//! the run stops (its tool call is dropped) or the Space's runner
//! assignment changes. Only an approved command is queued. Pending
//! approvals don't survive a restart: startup marks them `expired`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use metro_model_protocol::{self as protocol, Question};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody, db, member_of, owner_of};
use crate::auth::Human;
use crate::db::{Activity, Addressed, Author, Db, actor, kind, now_ms, short_id};
use crate::model_routing::{Decision, RoutingSettings};

/// How long an approval waits for a decision.
pub const APPROVAL_TTL: Duration = Duration::from_secs(600);
/// A `safe` answer below this probability counts as unsure.
pub const SAFE_FLOOR: f64 = 0.8;
const MAX_REQUEST_CHARS: usize = 4_000;
const Q_SHELL: &str = "shell_risk";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Approve,
    Classify,
    Allow,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Approve => "approve",
            Mode::Classify => "classify",
            Mode::Allow => "allow",
        }
    }
    /// Unknown stored text reads as the default.
    fn parse(s: &str) -> Self {
        match s {
            "off" => Mode::Off,
            "classify" => Mode::Classify,
            "allow" => Mode::Allow,
            _ => Mode::Approve,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Approver {
    Owner,
    Members,
}

impl Approver {
    pub fn as_str(self) -> &'static str {
        match self {
            Approver::Owner => "owner",
            Approver::Members => "members",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "members" => Approver::Members,
            _ => Approver::Owner,
        }
    }
}

pub const BUILT_IN_MULTIPLE_SPACES: &str = "built_in_multiple_spaces";
pub const CLASSIFY_UNAVAILABLE: &str = "classify_unavailable";

// ---- Storage ----

/// Additive: runner columns and the two policy tables. After `runners`.
pub(crate) fn init(conn: &Connection) -> rusqlite::Result<()> {
    for (col, def) in [
        ("built_in", "INTEGER NOT NULL DEFAULT 0"),
        ("shell_mode", "TEXT NOT NULL DEFAULT 'approve'"),
        ("shell_approver", "TEXT NOT NULL DEFAULT 'owner'"),
    ] {
        let has: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM pragma_table_info('runners') WHERE name = ?1)",
            [col],
            |r| r.get(0),
        )?;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE runners ADD COLUMN {col} {def}"))?;
        }
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS space_shell_policy (
             space_id       TEXT PRIMARY KEY NOT NULL REFERENCES spaces (id),
             shell_mode     TEXT NOT NULL,
             shell_approver TEXT NOT NULL,
             updated_at     INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS runner_space_shell (
             runner_id      TEXT NOT NULL REFERENCES runners (id),
             space_id       TEXT NOT NULL REFERENCES spaces (id),
             shell_mode     TEXT,
             shell_approver TEXT,
             updated_at     INTEGER NOT NULL,
             PRIMARY KEY (runner_id, space_id)
         );
         CREATE INDEX IF NOT EXISTS runner_space_shell_space ON runner_space_shell (space_id);
         CREATE INDEX IF NOT EXISTS space_events_shell ON space_events (space_id, id)
             WHERE kind IN ('shell_approval', 'shell_approval_resolved');",
    )
}

/// The Space is being deleted, in its transaction.
pub(crate) fn space_deleted(tx: &Connection, space_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM runner_space_shell WHERE space_id = ?1",
        [space_id],
    )?;
    tx.execute(
        "DELETE FROM space_shell_policy WHERE space_id = ?1",
        [space_id],
    )?;
    Ok(())
}

/// A registered runner left `space_id` (detached or replaced): its
/// override for that Space goes with it.
pub(crate) fn assignment_removed(tx: &Connection, space_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM runner_space_shell WHERE space_id = ?1",
        [space_id],
    )?;
    Ok(())
}

pub(crate) fn runner_deleted(tx: &Connection, runner_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM runner_space_shell WHERE runner_id = ?1",
        [runner_id],
    )?;
    Ok(())
}

/// Who manages the runner serving a Space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Manager {
    /// A dedicated token: the Space owner.
    Dedicated,
    /// A personal registered runner: its owner.
    Personal(String),
    /// An organization runner: the org admins.
    Organization,
}

/// The shell policy that applies in one Space now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpacePolicy {
    pub runner_name: String,
    pub manager: Manager,
    /// The configured mode, after inheritance.
    pub mode: Mode,
    pub approver: Approver,
    /// `dedicated`, `runner_default` or `space_override`.
    pub source: &'static str,
    /// What a call does now.
    pub effective: Mode,
    pub downgraded: Option<&'static str>,
    /// The Space's assignment fence (`spaces.runner_drop_before`): it moves
    /// whenever who serves the Space changes, never on a token rotation.
    pub fence: i64,
}

impl SpacePolicy {
    pub fn view(&self) -> Value {
        json!({
            "shell_mode": self.mode,
            "approver": self.approver,
            "effective_mode": self.effective,
            "downgraded_reason": self.downgraded,
            "runner_name": self.runner_name,
            "source": self.source,
        })
    }
}

fn effective(
    manager: &Manager,
    built_in: bool,
    attached: i64,
    mode: Mode,
) -> (Mode, Option<&'static str>) {
    match (manager, mode) {
        (Manager::Personal(_), Mode::Allow) if built_in && attached != 1 => {
            (Mode::Approve, Some(BUILT_IN_MULTIPLE_SPACES))
        }
        (Manager::Organization, Mode::Classify) => (Mode::Approve, Some(CLASSIFY_UNAVAILABLE)),
        _ => (mode, None),
    }
}

/// The policy in `space_id`, or `None` when it has no runner.
pub fn policy_in(conn: &Connection, space_id: &str) -> rusqlite::Result<Option<SpacePolicy>> {
    let registered = conn
        .query_row(
            "SELECT r.name, r.visibility, r.owner_id, r.built_in, r.shell_mode, r.shell_approver,
                    o.shell_mode, o.shell_approver,
                    (SELECT COUNT(*) FROM space_runners x WHERE x.runner_id = r.id),
                    s.runner_drop_before
             FROM space_runners a
             JOIN runners r ON r.id = a.runner_id
             JOIN spaces s ON s.id = a.space_id
             LEFT JOIN runner_space_shell o ON o.runner_id = r.id AND o.space_id = a.space_id
             WHERE a.space_id = ?1",
            [space_id],
            |r| {
                let manager = if r.get::<_, String>(1)? == "organization" {
                    Manager::Organization
                } else {
                    Manager::Personal(r.get(2)?)
                };
                let (dm, da): (String, String) = (r.get(4)?, r.get(5)?);
                let (om, oa): (Option<String>, Option<String>) = (r.get(6)?, r.get(7)?);
                let source = if om.is_some() || oa.is_some() {
                    "space_override"
                } else {
                    "runner_default"
                };
                let mode = Mode::parse(om.as_deref().unwrap_or(&dm));
                let approver = Approver::parse(oa.as_deref().unwrap_or(&da));
                let (eff, downgraded) = effective(&manager, r.get(3)?, r.get(8)?, mode);
                Ok(SpacePolicy {
                    runner_name: r.get(0)?,
                    manager,
                    mode,
                    approver,
                    source,
                    effective: eff,
                    downgraded,
                    fence: r.get(9)?,
                })
            },
        )
        .optional()?;
    if registered.is_some() {
        return Ok(registered);
    }
    conn.query_row(
        "SELECT s.runner_token_hash IS NOT NULL, s.runner_drop_before, p.shell_mode, p.shell_approver
         FROM spaces s LEFT JOIN space_shell_policy p ON p.space_id = s.id WHERE s.id = ?1",
        [space_id],
        |r| {
            if !r.get::<_, bool>(0)? {
                return Ok(None);
            }
            let mode = Mode::parse(&r.get::<_, Option<String>>(2)?.unwrap_or_default());
            let approver = Approver::parse(&r.get::<_, Option<String>>(3)?.unwrap_or_default());
            Ok(Some(SpacePolicy {
                runner_name: "Dedicated Space runner".to_owned(),
                manager: Manager::Dedicated,
                mode,
                approver,
                source: "dedicated",
                effective: mode,
                downgraded: None,
                fence: r.get(1)?,
            }))
        },
    )
    .optional()
    .map(Option::flatten)
}

/// Whether `user` may decide under `policy` in `space_id`; `None` when
/// they aren't a member. Membership comes first: no admin bypass.
pub fn eligible_in(
    conn: &Connection,
    policy: &SpacePolicy,
    space_id: &str,
    user: &str,
    is_org_admin: bool,
) -> rusqlite::Result<Option<bool>> {
    let role: Option<String> = conn
        .query_row(
            "SELECT role FROM space_members WHERE space_id = ?1 AND user_id = ?2",
            params![space_id, user],
            |r| r.get(0),
        )
        .optional()?;
    let Some(role) = role else {
        return Ok(None);
    };
    Ok(Some(match policy.approver {
        Approver::Members => true,
        Approver::Owner => match &policy.manager {
            Manager::Dedicated => role == "owner",
            Manager::Personal(owner) => owner == user,
            Manager::Organization => is_org_admin,
        },
    }))
}

/// Whose System One connection rates commands under `classify`.
fn rater_in(
    conn: &Connection,
    policy: &SpacePolicy,
    space_id: &str,
) -> rusqlite::Result<Option<String>> {
    match &policy.manager {
        Manager::Personal(owner) => Ok(Some(owner.clone())),
        Manager::Organization => Ok(None),
        Manager::Dedicated => conn
            .query_row(
                "SELECT user_id FROM space_members WHERE space_id = ?1 AND role = 'owner'",
                [space_id],
                |r| r.get(0),
            )
            .optional(),
    }
}

// ---- Approvals ----

/// What the decider said, or why nobody did.
#[derive(Debug)]
enum Resolution {
    Approved,
    Denied(String),
    Cancelled,
}

struct Pending {
    space_id: String,
    room_id: String,
    activity: Activity,
    info: Value,
    fence: i64,
    tx: oneshot::Sender<Resolution>,
}

/// Approvals waiting for a decision (memory only).
#[derive(Default)]
pub struct Approvals {
    pending: Mutex<HashMap<String, Pending>>,
}

impl Approvals {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One `workspace_shell` call, as the agent made it.
pub struct ShellCall<'a> {
    pub space_id: &'a str,
    pub room_id: &'a str,
    pub initiator: &'a Author,
    pub activity: &'a Activity,
    pub command: &'a str,
    /// The text of the message that started the run (for `classify`).
    pub request: &'a str,
}

/// Decide whether `call` may be queued. `Ok(fence)`: queue it, but only
/// while the Space's assignment fence is still `fence`. `Err`: the plain
/// text the model gets.
pub async fn authorize(state: &AppState, call: &ShellCall<'_>) -> Result<i64, String> {
    let sid = call.space_id.to_owned();
    let loaded = state
        .db
        .call(move |db| {
            let conn = db.lock();
            let Some(policy) = policy_in(&conn, &sid)? else {
                return Ok(None);
            };
            let rater = rater_in(&conn, &policy, &sid)?;
            let name: String =
                conn.query_row("SELECT name FROM spaces WHERE id = ?1", [&sid], |r| {
                    r.get(0)
                })?;
            Ok(Some((policy, rater, name)))
        })
        .await;
    let (policy, rater, space_name) = match loaded {
        Ok(Some(x)) => x,
        Ok(None) => return Err("this Space has no runner.".to_owned()),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "reading the shell policy");
            return Err("the shell policy could not be read.".to_owned());
        }
    };
    let (code, reason): (&str, String) = match policy.effective {
        Mode::Off => {
            return Err("workspace.shell is turned off for this Space's runner.".to_owned());
        }
        Mode::Allow => return Ok(policy.fence),
        Mode::Classify => match classify(state, rater.as_deref(), call, &space_name).await {
            Verdict::Safe => return Ok(policy.fence),
            Verdict::Unsafe => (
                "classify_unsafe",
                "System One rated this command unsafe.".into(),
            ),
            Verdict::Unsure => (
                "classify_unsure",
                "System One wasn't sure this command is safe.".into(),
            ),
            Verdict::Failed(why) => (
                "classify_failed",
                format!("System One couldn't rate this command: {why}."),
            ),
        },
        Mode::Approve => match policy.downgraded {
            Some(BUILT_IN_MULTIPLE_SPACES) => (
                BUILT_IN_MULTIPLE_SPACES,
                "Allow is off while this built-in runner serves more than one Space.".into(),
            ),
            Some(code) => (code, "Shell commands on this runner need approval.".into()),
            None => (
                "policy",
                "Shell commands on this runner need approval.".into(),
            ),
        },
    };
    request_approval(state, call, &policy, code, &reason).await
}

/// Removes a still-pending approval when its waiter goes away (the run
/// stopped) and records it as cancelled.
struct WaitGuard {
    state: AppState,
    id: String,
}

impl WaitGuard {
    /// Take the approval out if nobody decided it yet.
    fn take(&self) -> Option<Pending> {
        self.state.shell.lock().remove(&self.id)
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        if let Some(p) = self.take() {
            let state = self.state.clone();
            let id = self.id.clone();
            tokio::spawn(async move { resolved_event(&state, &p, &id, "cancelled", None).await });
        }
    }
}

async fn resolved_event(
    state: &AppState,
    p: &Pending,
    id: &str,
    decision: &str,
    by: Option<&Human>,
) {
    let text = resolved_text(id, decision, by);
    state
        .append_with(
            &p.space_id,
            &p.room_id,
            kind::SHELL_APPROVAL_RESOLVED,
            &Author::server(actor::SYSTEM),
            &text,
            Some(&p.activity),
        )
        .await;
}

fn resolved_text(id: &str, decision: &str, by: Option<&Human>) -> String {
    json!({
        "approval_id": id,
        "decision": decision,
        "by": by.map(|h| json!({"id": h.id, "name": h.name})),
    })
    .to_string()
}

async fn request_approval(
    state: &AppState,
    call: &ShellCall<'_>,
    policy: &SpacePolicy,
    code: &str,
    reason: &str,
) -> Result<i64, String> {
    let id = format!("sa-{}", short_id());
    let expires = now_ms() + APPROVAL_TTL.as_millis() as i64;
    let info = json!({
        "approval_id": id,
        "run_id": call.activity.id,
        "command": call.command,
        "requested_by": {"id": call.initiator.id, "name": call.initiator.name},
        "runner_name": policy.runner_name,
        "approver": policy.approver,
        "reason_code": code,
        "reason": reason,
        "expires_at": crate::db::rfc3339_utc(expires),
    });
    let (tx, mut rx) = oneshot::channel();
    // Subscribed before the approval is visible, so no change is missed;
    // a change before this is caught by the fence when someone decides.
    let mut revoked = state.jobs.subscribe_revoked();
    state.shell.lock().insert(
        id.clone(),
        Pending {
            space_id: call.space_id.to_owned(),
            room_id: call.room_id.to_owned(),
            activity: call.activity.clone(),
            info: info.clone(),
            fence: policy.fence,
            tx,
        },
    );
    let guard = WaitGuard {
        state: state.clone(),
        id: id.clone(),
    };
    state
        .append_with(
            call.space_id,
            call.room_id,
            kind::SHELL_APPROVAL,
            &Author::server(actor::SYSTEM),
            &info.to_string(),
            Some(call.activity),
        )
        .await;
    let space = call.space_id.to_owned();
    let changed = async move {
        loop {
            match revoked.recv().await {
                Ok(s) if s == space => return,
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => return,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    std::future::pending::<()>().await
                }
            }
        }
    };
    let outcome = tokio::select! {
        r = &mut rx => r.unwrap_or(Resolution::Cancelled),
        () = tokio::time::sleep(APPROVAL_TTL) => match guard.take() {
            Some(p) => {
                resolved_event(state, &p, &id, "expired", None).await;
                return Err("nobody approved the command within 10 minutes.".to_owned());
            }
            None => rx.try_recv().unwrap_or(Resolution::Cancelled),
        },
        () = changed => match guard.take() {
            Some(p) => {
                resolved_event(state, &p, &id, "cancelled", None).await;
                Resolution::Cancelled
            }
            None => rx.try_recv().unwrap_or(Resolution::Cancelled),
        },
    };
    match outcome {
        Resolution::Approved => Ok(policy.fence),
        Resolution::Denied(by) => Err(format!("the command was denied by {by}.")),
        Resolution::Cancelled => Err(
            "the approval was cancelled (the run stopped or the Space's runner changed)."
                .to_owned(),
        ),
    }
}

// ---- Classify ----

#[derive(Debug, PartialEq)]
enum Verdict {
    Safe,
    Unsafe,
    Unsure,
    Failed(String),
}

fn shell_question() -> Question {
    let criteria = [
        (
            "safe",
            json!({"label": "Safe", "description": "Low risk: reads or builds inside the project, \
                   changes nothing outside it, deletes nothing that can't be rebuilt, sends no \
                   data anywhere and reveals no secrets."}),
        ),
        (
            "unsafe",
            json!({"label": "Unsafe", "description": "Could damage or delete data, change the \
                   system or files outside the project, install or run downloaded code, contact \
                   the network, or expose secrets or private data."}),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    Question::Choice {
        instructions: json!(
            "An agent wants to run `command` in a shell on a person's computer, in the Space's \
             project folder. `request` is what the person asked for. Is the command safe to run \
             without asking anyone? Safe means low risk of damage or data exposure, not only \
             that it stays in the folder. When unsure, answer unsafe."
        ),
        criteria,
    }
}

/// One typed System One Choice on `rater`'s own connection, sent only the
/// command, the Space name and the run's request.
async fn classify(
    state: &AppState,
    rater: Option<&str>,
    call: &ShellCall<'_>,
    space_name: &str,
) -> Verdict {
    let Some(rater) = rater else {
        return Verdict::Failed("no runner owner to ask".into());
    };
    let settings = match crate::model_routing::preferences(state, rater).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "reading the runner owner's System One setting");
            return Verdict::Failed("the runner owner's settings could not be read".into());
        }
    };
    let Some(classifier) = settings.classifier_id.clone() else {
        return Verdict::Failed("the runner owner has no System One connection".into());
    };
    match state
        .routing
        .backend
        .system_one_usable(state, rater, call.space_id, &classifier)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return Verdict::Failed(
                "the runner owner's System One connection can't be used here".into(),
            );
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "checking the System One connection");
            return Verdict::Failed("the System One connection could not be checked".into());
        }
    }
    let request: String = call.request.chars().take(MAX_REQUEST_CHARS).collect();
    let payload = json!({"command": call.command, "space": space_name, "request": request});
    let only = RoutingSettings {
        classifier_id: Some(classifier),
        ..RoutingSettings::default()
    };
    let answer = crate::model_routing::ask_one(
        state,
        rater,
        Some(call.space_id),
        &only,
        payload,
        Q_SHELL,
        shell_question(),
    )
    .await;
    verdict(answer)
}

fn verdict(answer: Decision<protocol::Answer>) -> Verdict {
    let answer = match answer {
        Decision::Answered(a) => a,
        Decision::Failed(why) => return Verdict::Failed(why.to_string()),
    };
    let protocol::Answer::Choice {
        choice,
        probabilities,
        ..
    } = &answer
    else {
        return Verdict::Failed("System One's answer was malformed".into());
    };
    let p = probabilities.get(choice).copied().unwrap_or(0.0);
    match choice.as_str() {
        "safe" if p + f64::EPSILON >= SAFE_FLOOR => Verdict::Safe,
        "safe" => Verdict::Unsure,
        "unsafe" => Verdict::Unsafe,
        _ => Verdict::Failed("System One's answer was malformed".into()),
    }
}

// ---- Startup ----

/// Approvals left pending by an earlier process: their runs are gone.
pub fn expire_orphans(db: &Db) -> anyhow::Result<usize> {
    let mut conn = db.lock();
    let tx = conn.transaction()?;
    let orphans: Vec<(String, String, String, Option<String>)> = {
        let mut stmt = tx.prepare(
            "SELECT e.space_id, e.room_id, json_extract(e.text, '$.approval_id'), e.activity
             FROM space_events e
             WHERE e.kind = 'shell_approval' AND json_valid(e.text)
               AND NOT EXISTS (
                 SELECT 1 FROM space_events r
                 WHERE r.space_id = e.space_id AND r.kind = 'shell_approval_resolved'
                   AND json_valid(r.text)
                   AND json_extract(r.text, '$.approval_id') = json_extract(e.text, '$.approval_id'))",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (space, room, id, activity) in &orphans {
        let activity: Option<Activity> = activity
            .as_deref()
            .and_then(|a| serde_json::from_str(a).ok());
        Db::append_event_in(
            &tx,
            space,
            room,
            kind::SHELL_APPROVAL_RESOLVED,
            &Author::server(actor::SYSTEM),
            &resolved_text(id, "expired", None),
            &Addressed::room(),
            activity.as_ref(),
        )?;
    }
    tx.commit()?;
    Ok(orphans.len())
}

// ---- HTTP ----

/// `GET|POST /v1/spaces/{sid}/shell-approvals[/{id}]`,
/// `PUT /v1/spaces/{sid}/shell-policy`, `GET|PUT /v1/runners/{id}/shell-policy`,
/// `PUT /v1/runners/{id}/spaces/{sid}/shell-policy`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/spaces/{sid}/shell-approvals", get(list_approvals))
        .route("/v1/spaces/{sid}/shell-approvals/{aid}", post(decide))
        .route("/v1/spaces/{sid}/shell-policy", put(put_dedicated))
        .route(
            "/v1/runners/{id}/shell-policy",
            get(get_runner_policy).put(put_runner_policy),
        )
        .route(
            "/v1/runners/{id}/spaces/{sid}/shell-policy",
            put(put_space_override),
        )
}

fn valid_approval_id(id: &str) -> bool {
    id.len() == 19
        && id.starts_with("sa-")
        && id[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

async fn list_approvals(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
) -> ApiResult<Json<Value>> {
    let space = member_of(&state, &sid, &who).await?;
    let approvals = state.shell.clone();
    let (uid, admin) = (who.id.clone(), state.config.is_org_admin(&who.id));
    let list = db(&state, move |db| {
        let conn = db.lock();
        let policy = policy_in(&conn, &space.id)?;
        let can = match &policy {
            Some(p) => eligible_in(&conn, p, &space.id, &uid, admin)?.unwrap_or(false),
            None => false,
        };
        let pending = approvals.lock();
        let mut out: Vec<Value> = pending
            .values()
            .filter(|p| p.space_id == space.id)
            .map(|p| {
                let mut v = p.info.clone();
                v["room_id"] = json!(p.room_id);
                v["can_decide"] = json!(can && policy.as_ref().is_some_and(|x| x.fence == p.fence));
                v
            })
            .collect();
        out.sort_by(|a, b| a["expires_at"].as_str().cmp(&b["expires_at"].as_str()));
        Ok(out)
    })
    .await?;
    Ok(Json(json!({"approvals": list})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideBody {
    decision: Choice,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Choice {
    Approve,
    Deny,
}

enum Decided {
    Unknown,
    Forbidden,
    Gone,
    Won(Box<Pending>),
}

async fn decide(
    State(state): State<AppState>,
    who: Human,
    Path((sid, aid)): Path<(String, String)>,
    body: JsonBody<DecideBody>,
) -> ApiResult<Json<Value>> {
    let space = member_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    let no = || ApiError::not_found("no such approval");
    if !valid_approval_id(&aid) {
        return Err(no());
    }
    let approvals = state.shell.clone();
    let (uid, admin, id) = (
        who.id.clone(),
        state.config.is_org_admin(&who.id),
        aid.clone(),
    );
    let decision = if body.decision == Choice::Approve {
        "approve"
    } else {
        "deny"
    };
    let text = resolved_text(&aid, decision, Some(&who));
    let outcome = db(&state, move |db| {
        let mut conn = db.lock();
        let tx = conn.transaction()?;
        let mut pending = approvals.lock();
        let Some(p) = pending.get(&id).filter(|p| p.space_id == space.id) else {
            let seen: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM space_events WHERE space_id = ?1
                     AND kind = 'shell_approval' AND json_valid(text)
                     AND json_extract(text, '$.approval_id') = ?2)",
                params![space.id, id],
                |r| r.get(0),
            )?;
            return Ok(if seen {
                Decided::Gone
            } else {
                Decided::Unknown
            });
        };
        let policy = policy_in(&tx, &space.id)?;
        let Some(policy) = policy.filter(|x| x.fence == p.fence) else {
            // The runner changed after the request: nobody can approve it.
            if let Some(p) = pending.remove(&id) {
                let _ = p.tx.send(Resolution::Cancelled);
                Db::append_event_in(
                    &tx,
                    &p.space_id,
                    &p.room_id,
                    kind::SHELL_APPROVAL_RESOLVED,
                    &Author::server(actor::SYSTEM),
                    &resolved_text(&id, "cancelled", None),
                    &Addressed::room(),
                    Some(&p.activity),
                )?;
                tx.commit()?;
            }
            return Ok(Decided::Gone);
        };
        match eligible_in(&tx, &policy, &space.id, &uid, admin)? {
            None => return Ok(Decided::Unknown),
            Some(false) => return Ok(Decided::Forbidden),
            Some(true) => {}
        }
        let p = pending.remove(&id).expect("checked above");
        Db::append_event_in(
            &tx,
            &p.space_id,
            &p.room_id,
            kind::SHELL_APPROVAL_RESOLVED,
            &Author::server(actor::SYSTEM),
            &text,
            &Addressed::room(),
            Some(&p.activity),
        )?;
        tx.commit()?;
        Ok(Decided::Won(Box::new(p)))
    })
    .await?;
    match outcome {
        Decided::Unknown => Err(no()),
        Decided::Forbidden => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "you can't approve shell commands on this Space's runner",
        )),
        Decided::Gone => Err(ApiError::new(
            StatusCode::CONFLICT,
            "this approval was already decided, expired or was cancelled",
        )),
        Decided::Won(p) => {
            let r = match body.decision {
                Choice::Approve => Resolution::Approved,
                Choice::Deny => Resolution::Denied(who.name.clone()),
            };
            // The run stopped while this was decided: nothing will run.
            let _ = p.tx.send(r);
            Ok(Json(json!({
                "approval_id": aid,
                "decision": decision,
                "by": {"id": who.id, "name": who.name},
            })))
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyBody {
    shell_mode: Mode,
    approver: Approver,
}

/// Both keys required; `null` inherits the runner's default.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideBody {
    #[serde(deserialize_with = "required_nullable")]
    shell_mode: Option<Mode>,
    #[serde(deserialize_with = "required_nullable")]
    approver: Option<Approver>,
}

/// A key that must be present but may be `null`.
fn required_nullable<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

async fn put_dedicated(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<PolicyBody>,
) -> ApiResult<Json<Value>> {
    let space = owner_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    let policy = db(&state, move |db| {
        let mut conn = db.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let dedicated: bool = tx.query_row(
            "SELECT runner_token_hash IS NOT NULL
                 AND NOT EXISTS (SELECT 1 FROM space_runners WHERE space_id = ?1)
             FROM spaces WHERE id = ?1",
            [&space.id],
            |r| r.get(0),
        )?;
        if !dedicated {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO space_shell_policy (space_id, shell_mode, shell_approver, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (space_id) DO UPDATE SET shell_mode = excluded.shell_mode,
                 shell_approver = excluded.shell_approver, updated_at = excluded.updated_at",
            params![space.id, body.shell_mode.as_str(), body.approver.as_str(), now_ms()],
        )?;
        let policy = policy_in(&tx, &space.id)?;
        tx.commit()?;
        Ok(policy)
    })
    .await?
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "this Space has no dedicated runner; a registered runner's policy is set by whoever manages it",
        )
    })?;
    Ok(Json(json!({"shell": policy.view()})))
}

async fn get_runner_policy(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let runner = crate::runners::managed_runner(&state, &id, &who).await?;
    runner_view(&state, &runner.id, &who).await.map(Json)
}

async fn runner_view(state: &AppState, rid: &str, who: &Human) -> ApiResult<Value> {
    let (rid, uid) = (rid.to_owned(), who.id.clone());
    db(state, move |db| {
        let conn = db.lock();
        let (name, visibility, built_in, mode, approver): (String, String, bool, String, String) = conn
            .query_row(
                "SELECT name, visibility, built_in, shell_mode, shell_approver FROM runners WHERE id = ?1",
                [&rid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
        let mut stmt = conn.prepare(
            "SELECT a.space_id, s.name,
                    EXISTS (SELECT 1 FROM space_members m WHERE m.space_id = a.space_id AND m.user_id = ?2),
                    o.shell_mode, o.shell_approver
             FROM space_runners a JOIN spaces s ON s.id = a.space_id
             LEFT JOIN runner_space_shell o ON o.runner_id = a.runner_id AND o.space_id = a.space_id
             WHERE a.runner_id = ?1 ORDER BY a.space_id",
        )?;
        type Row = (String, String, bool, Option<String>, Option<String>);
        let rows: Vec<Row> = stmt
            .query_map(params![rid, uid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut spaces = Vec::new();
        for (sid, sname, member, om, oa) in rows {
            let p = policy_in(&conn, &sid)?;
            spaces.push(json!({
                "space_id": sid,
                "space_name": member.then_some(sname),
                "shell_mode": om.as_deref().map(Mode::parse),
                "approver": oa.as_deref().map(Approver::parse),
                "effective_mode": p.as_ref().map(|p| p.effective),
                "effective_approver": p.as_ref().map(|p| p.approver),
                "downgraded_reason": p.as_ref().and_then(|p| p.downgraded),
            }));
        }
        Ok(json!({
            "runner_id": rid,
            "runner_name": name,
            "visibility": visibility,
            "built_in": built_in,
            "default": {"shell_mode": Mode::parse(&mode), "approver": Approver::parse(&approver)},
            "spaces": spaces,
        }))
    })
    .await
}

fn refuse_org_classify(org: bool, mode: Option<Mode>) -> ApiResult<()> {
    if org && mode == Some(Mode::Classify) {
        return Err(ApiError::bad_request(
            "classify needs one owner's System One connection; organization runners can't use it",
        ));
    }
    Ok(())
}

async fn put_runner_policy(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: JsonBody<PolicyBody>,
) -> ApiResult<Json<Value>> {
    let runner = crate::runners::managed_runner(&state, &id, &who).await?;
    let Json(body) = body?;
    refuse_org_classify(
        runner.visibility == crate::runners::Visibility::Organization,
        Some(body.shell_mode),
    )?;
    let rid = runner.id.clone();
    db(&state, move |db| {
        db.lock().execute(
            "UPDATE runners SET shell_mode = ?2, shell_approver = ?3 WHERE id = ?1",
            params![rid, body.shell_mode.as_str(), body.approver.as_str()],
        )?;
        Ok(())
    })
    .await?;
    runner_view(&state, &runner.id, &who).await.map(Json)
}

async fn put_space_override(
    State(state): State<AppState>,
    who: Human,
    Path((id, sid)): Path<(String, String)>,
    body: JsonBody<OverrideBody>,
) -> ApiResult<Json<Value>> {
    let runner = crate::runners::managed_runner(&state, &id, &who).await?;
    let Json(body) = body?;
    refuse_org_classify(
        runner.visibility == crate::runners::Visibility::Organization,
        body.shell_mode,
    )?;
    if crate::config::validate_id(&sid).is_err() {
        return Err(ApiError::not_found("no such Space"));
    }
    let rid = runner.id.clone();
    let attached = db(&state, move |db| {
        let mut conn = db.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let attached: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM space_runners WHERE space_id = ?1 AND runner_id = ?2)",
            params![sid, rid],
            |r| r.get(0),
        )?;
        if !attached {
            return Ok(false);
        }
        if body.shell_mode.is_none() && body.approver.is_none() {
            tx.execute(
                "DELETE FROM runner_space_shell WHERE runner_id = ?1 AND space_id = ?2",
                params![rid, sid],
            )?;
        } else {
            tx.execute(
                "INSERT INTO runner_space_shell (runner_id, space_id, shell_mode, shell_approver, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (runner_id, space_id) DO UPDATE SET shell_mode = excluded.shell_mode,
                     shell_approver = excluded.shell_approver, updated_at = excluded.updated_at",
                params![
                    rid,
                    sid,
                    body.shell_mode.map(Mode::as_str),
                    body.approver.map(Approver::as_str),
                    now_ms()
                ],
            )?;
        }
        tx.commit()?;
        Ok(true)
    })
    .await?;
    if !attached {
        return Err(ApiError::not_found(
            "this runner isn't attached to that Space",
        ));
    }
    runner_view(&state, &runner.id, &who).await.map(Json)
}

/// Shared with [`crate::AppState`].
pub type SharedApprovals = Arc<Approvals>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runners::Visibility;

    fn choice(choice: &str, p: Option<f64>) -> Decision<protocol::Answer> {
        Decision::Answered(protocol::Answer::Choice {
            choice: choice.to_owned(),
            confidence: Some(0.5),
            probabilities: p.map(|p| (choice.to_owned(), p)).into_iter().collect(),
        })
    }

    #[test]
    fn only_a_confident_safe_skips_approval() {
        assert_eq!(verdict(choice("safe", Some(0.95))), Verdict::Safe);
        assert_eq!(verdict(choice("safe", Some(0.8))), Verdict::Safe);
        assert_eq!(verdict(choice("safe", Some(0.79))), Verdict::Unsure);
        assert_eq!(verdict(choice("safe", None)), Verdict::Unsure);
        assert_eq!(verdict(choice("unsafe", Some(0.99))), Verdict::Unsafe);
        assert!(matches!(
            verdict(choice("yes", Some(1.0))),
            Verdict::Failed(_)
        ));
        assert!(matches!(
            verdict(Decision::Failed(
                crate::model_routing::DecisionFailure::Timeout
            )),
            Verdict::Failed(_)
        ));
    }

    #[test]
    fn built_in_allow_needs_exactly_one_space_and_org_never_classifies() {
        let me = Manager::Personal("alice".into());
        assert_eq!(effective(&me, true, 1, Mode::Allow), (Mode::Allow, None));
        for n in [0, 2, 5] {
            assert_eq!(
                effective(&me, true, n, Mode::Allow),
                (Mode::Approve, Some(BUILT_IN_MULTIPLE_SPACES))
            );
        }
        assert_eq!(effective(&me, false, 3, Mode::Allow), (Mode::Allow, None));
        assert_eq!(effective(&me, true, 3, Mode::Off), (Mode::Off, None));
        assert_eq!(
            effective(&Manager::Organization, false, 1, Mode::Classify),
            (Mode::Approve, Some(CLASSIFY_UNAVAILABLE))
        );
        assert_eq!(Mode::parse("garbage"), Mode::Approve);
        assert_eq!(Approver::parse("garbage"), Approver::Owner);
    }

    #[test]
    fn eligibility_follows_the_manager_and_needs_membership() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_space("alice", "S", 4).unwrap().id;
        db.add_member(&sid, "bob", 8).unwrap();
        db.add_member(&sid, "carol", 8).unwrap();
        let conn = db.lock();
        let base = SpacePolicy {
            runner_name: "r".into(),
            manager: Manager::Dedicated,
            mode: Mode::Approve,
            approver: Approver::Owner,
            source: "dedicated",
            effective: Mode::Approve,
            downgraded: None,
            fence: 0,
        };
        let check = |p: &SpacePolicy, user: &str, admin: bool| {
            eligible_in(&conn, p, &sid, user, admin).unwrap()
        };
        assert_eq!(check(&base, "alice", false), Some(true));
        assert_eq!(check(&base, "bob", true), Some(false));
        assert_eq!(check(&base, "dave", true), None);
        let personal = SpacePolicy {
            manager: Manager::Personal("bob".into()),
            ..base.clone()
        };
        assert_eq!(check(&personal, "bob", false), Some(true));
        assert_eq!(check(&personal, "alice", false), Some(false));
        let org = SpacePolicy {
            manager: Manager::Organization,
            ..base.clone()
        };
        assert_eq!(check(&org, "carol", true), Some(true));
        assert_eq!(check(&org, "alice", false), Some(false));
        assert_eq!(check(&org, "dave", true), None, "no admin bypass");
        let members = SpacePolicy {
            approver: Approver::Members,
            ..org
        };
        assert_eq!(check(&members, "bob", false), Some(true));
        assert_eq!(check(&members, "dave", true), None);
    }

    #[test]
    fn overrides_inherit_and_go_with_the_assignment() {
        let db = Db::open_in_memory().unwrap();
        let sid = db.create_space("alice", "S", 4).unwrap().id;
        assert_eq!(policy_in(&db.lock(), &sid).unwrap(), None);
        let r = db
            .create_runner_marked("alice", "laptop", Visibility::Personal, b"h", 5, true)
            .unwrap();
        db.attach_runner(&sid, &r.id, "alice").unwrap();
        let p = policy_in(&db.lock(), &sid).unwrap().unwrap();
        assert_eq!(
            (p.mode, p.approver, p.source, p.effective),
            (
                Mode::Approve,
                Approver::Owner,
                "runner_default",
                Mode::Approve
            )
        );
        let fence = p.fence;
        {
            let conn = db.lock();
            conn.execute(
                "UPDATE runners SET shell_mode = 'allow', shell_approver = 'members' WHERE id = ?1",
                [&r.id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO runner_space_shell (runner_id, space_id, shell_mode, shell_approver, updated_at)
                 VALUES (?1, ?2, NULL, 'owner', 0)",
                params![r.id, sid],
            )
            .unwrap();
        }
        let p = policy_in(&db.lock(), &sid).unwrap().unwrap();
        assert_eq!(
            (p.mode, p.approver, p.source, p.effective, p.downgraded),
            (
                Mode::Allow,
                Approver::Owner,
                "space_override",
                Mode::Allow,
                None
            )
        );
        db.detach_runner(&sid).unwrap();
        db.attach_runner(&sid, &r.id, "alice").unwrap();
        let p = policy_in(&db.lock(), &sid).unwrap().unwrap();
        assert_eq!(
            (p.source, p.approver),
            ("runner_default", Approver::Members)
        );
        assert_ne!(p.fence, fence, "reattaching moves the fence");
        db.delete_runner(&r.id).unwrap();
        assert_eq!(policy_in(&db.lock(), &sid).unwrap(), None);
    }

    #[test]
    fn approval_ids_are_checked_before_lookup() {
        assert!(valid_approval_id(&format!("sa-{}", short_id())));
        for bad in [
            "",
            "sa-",
            "sa-XYZ0000000000000",
            "ab-0123456789abcdef",
            "sa-0123456789abcdef0",
        ] {
            assert!(!valid_approval_id(bad), "{bad}");
        }
    }
}
