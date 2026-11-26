//! Agent runs and child threads.
//!
//! Two pieces live here:
//!
//! - [`RunRegistry`]: an in-memory, thread-safe record of agent runs, so a
//!   person's Chat UI can list, inspect and stop them. It keeps every running
//!   entry and a bounded history of finished ones, and caps how many runs may
//!   be active at once.
//! - The `thread_*` Rig tools, through which a running parent agent spawns a
//!   child thread, sends it a follow-up, reads its status and stops it. The
//!   tools only check the model's arguments; the work happens in a
//!   [`ThreadControl`] that the server builds per parent run.
//!
//! # Authorization
//!
//! Nothing in this module authorizes anyone. The registry trusts every id it
//! is given and answers every caller; the server must decide who may start a
//! run in a space and room, and must check, before calling [`RunRegistry::get`]
//! or [`RunRegistry::stop`] for a request, that the requester may see or stop
//! that run (for example: same space, and the run's `actor` is the requester
//! or the requester may manage the room). [`RunRegistry::list`] returns every
//! run; filter it before it leaves the server.
//!
//! Likewise the tools never take an identity, space, room or parent id from
//! the model. A [`ThreadControl`] captures those from the trusted request that
//! started the parent run, and must refuse thread ids that do not belong to
//! that parent.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use rig_core::completion::ToolAnnotations;
use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::tools::truncate;

/// Default cap on runs in the `running` state at once.
pub const DEFAULT_MAX_RUNNING: usize = 8;
/// Default number of finished runs kept for listing.
pub const DEFAULT_MAX_HISTORY: usize = 200;
/// Characters of a run's task kept in [`RunInfo::task`].
pub const MAX_TASK_PREVIEW_CHARS: usize = 280;
/// Longest space, room or actor id the registry accepts, in bytes.
pub const MAX_ID_BYTES: usize = 256;

/// Longest thread name a model may give, in characters.
pub const MAX_THREAD_NAME_CHARS: usize = 80;
/// Longest task or message a model may send to a thread, in bytes.
pub const MAX_THREAD_TEXT_BYTES: usize = 16 * 1024;
/// Longest thread id a model may pass, in bytes.
pub const MAX_THREAD_ID_BYTES: usize = 128;
/// Longest tool result handed back to the model, in bytes.
pub const MAX_THREAD_OUTPUT_BYTES: usize = 32 * 1024;

// ---------------------------------------------------------------------------
// Run registry
// ---------------------------------------------------------------------------

/// Where a run is. Every state but `Running` is terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        self != RunStatus::Running
    }
}

/// A snapshot of one run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunInfo {
    /// Unique within the registry's lifetime (`run_<uuid>`).
    pub id: String,
    pub space_id: String,
    pub room_id: String,
    /// Stable id of whoever started the run, as given by the server.
    pub actor: String,
    /// The task, cut to [`MAX_TASK_PREVIEW_CHARS`] characters.
    pub task: String,
    pub status: RunStatus,
    /// Every registered run answers a message in a Space room or thread.
    pub kind: RunKind,
    /// The Space agent working the run, when known.
    pub agent_id: Option<String>,
    /// The stored event the run answers, when known: what a retry answers.
    pub trigger_event_id: Option<i64>,
    /// When the run was registered, RFC 3339 UTC.
    #[serde(serialize_with = "rfc3339_ms")]
    pub started_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunKind {
    Room,
}

/// What a room run works for: its agent and the event it answers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOrigin {
    pub agent_id: Option<String>,
    pub trigger_event_id: Option<i64>,
}

/// Whether run `run`'s task preview may still be shown: it copies the start
/// of its trigger, so not once that message was deleted (or is a deleted
/// thread anchor). Readers call this at read time, as the viewer.
pub fn trigger_live(conn: &rusqlite::Connection, run: &RunInfo) -> rusqlite::Result<bool> {
    use rusqlite::OptionalExtension;
    let Some(id) = run.trigger_event_id else {
        return Ok(true);
    };
    let kind: Option<String> = conn
        .query_row(
            "SELECT kind FROM space_events WHERE id = ?1 AND space_id = ?2 AND room_id = ?3",
            rusqlite::params![id, run.space_id, run.room_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(kind.is_some_and(|k| k != crate::db::kind::DELETED))
}

/// `runs` with the task of each run whose trigger is gone emptied.
pub fn live_tasks(conn: &rusqlite::Connection, runs: &mut [RunInfo]) -> rusqlite::Result<()> {
    for run in runs.iter_mut() {
        if !run.task.is_empty() && !trigger_live(conn, run)? {
            run.task.clear();
        }
    }
    Ok(())
}

fn rfc3339_ms<S: serde::Serializer>(ms: &i64, s: S) -> Result<S::Ok, S::Error> {
    match crate::db::rfc3339_utc(*ms) {
        Some(t) => s.serialize_str(&t),
        None => s.serialize_none(),
    }
}

struct Entry {
    info: RunInfo,
    /// Present while the run is `Running`; taken when it ends.
    cancel: Option<watch::Sender<bool>>,
    /// Why the run failed, in full, for its initiator only. Never in
    /// [`RunInfo`], so listing or serializing runs can't expose it.
    failure: Option<RunFailure>,
    /// Whether the run's worker has recorded its end
    /// ([`RunRegistry::finish_with`]). A stopped run stays in the history
    /// until then, so its worker always learns that the stop won.
    settled: bool,
    /// Turns `true` once the worker is gone and everything it publishes is
    /// stored ([`Registration`] drop). Dropped with the entry on eviction.
    done: watch::Sender<bool>,
}

/// The full reason a run failed, kept in memory with the run (never
/// stored) and shown only to the person who started it
/// (`GET /v1/runs`, `GET /v1/runs/{id}`). In a Space room the run posted
/// a generic error instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunFailure {
    /// `model_error` (the model or its provider failed), or why the
    /// initiator's model is no longer usable by them: `not_member`,
    /// `not_allowed`, `busy`, `offline`, `misconfigured` or `internal`.
    pub reason: String,
    /// What happened, for the initiator.
    pub message: String,
}

#[derive(Default)]
struct Inner {
    /// Oldest first.
    entries: VecDeque<Entry>,
}

impl Inner {
    /// Runs whose work may still be going on: running, or stopped while
    /// their worker hasn't recorded its end yet. They count against the
    /// cap.
    fn running(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.info.status == RunStatus::Running || !e.settled)
            .count()
    }

    fn find(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.info.id == id)
    }

    /// Drop the oldest settled runs beyond `max_history`. Running entries,
    /// and stopped ones whose worker hasn't recorded its end yet, are never
    /// dropped.
    fn evict(&mut self, max_history: usize) {
        let evictable = |e: &Entry| e.info.status.is_terminal() && e.settled;
        let mut terminal = self.entries.iter().filter(|e| evictable(e)).count();
        while terminal > max_history {
            match self.entries.iter().position(&evictable) {
                Some(i) => {
                    self.entries.remove(i);
                    terminal -= 1;
                }
                None => break,
            }
        }
    }
}

/// Thread-safe record of agent runs. Cheap to share behind an `Arc`; all
/// methods are synchronous and hold a short lock, so they are safe to call
/// from async code without `spawn_blocking`.
///
/// It holds at most `max_running` running runs plus `max_history` finished
/// ones. State is in memory only and is lost on restart.
///
/// See the module docs: authorization is the server's job.
pub struct RunRegistry {
    inner: Mutex<Inner>,
    max_running: usize,
    max_history: usize,
}

impl Default for RunRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RunRegistry {
    /// A registry with [`DEFAULT_MAX_RUNNING`] and [`DEFAULT_MAX_HISTORY`].
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_RUNNING, DEFAULT_MAX_HISTORY)
    }

    /// A registry with explicit caps. `max_running` is raised to at least 1.
    pub fn with_limits(max_running: usize, max_history: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            max_running: max_running.max(1),
            max_history,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock cannot leave `Inner` half-updated in
        // a way that matters, so keep serving.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record a new running run. Returns its snapshot and a receiver that
    /// starts at `false` and turns `true` when [`stop`](Self::stop) cancels
    /// the run. The run's worker should watch the receiver (for example
    /// `tokio::select!` on `rx.wait_for(|c| *c)`) and abort, and must call
    /// [`finish`](Self::finish) whatever happens.
    ///
    /// Fails when `max_running` runs are already running, when the task is
    /// blank, or when an id is empty or longer than [`MAX_ID_BYTES`].
    pub fn start(
        &self,
        space_id: &str,
        room_id: &str,
        actor: &str,
        task: &str,
    ) -> Result<(RunInfo, watch::Receiver<bool>), String> {
        self.start_for(space_id, room_id, actor, task, RunOrigin::default())
    }

    /// [`start`](Self::start) recording the run's agent and trigger event.
    pub fn start_for(
        &self,
        space_id: &str,
        room_id: &str,
        actor: &str,
        task: &str,
        origin: RunOrigin,
    ) -> Result<(RunInfo, watch::Receiver<bool>), String> {
        for (what, v) in [
            ("space_id", space_id),
            ("room_id", room_id),
            ("actor", actor),
        ] {
            if v.is_empty() || v.len() > MAX_ID_BYTES {
                return Err(format!("{what} must be 1..={MAX_ID_BYTES} bytes"));
            }
        }
        let task = task.trim();
        if task.is_empty() {
            return Err("task must not be empty".into());
        }

        let mut inner = self.lock();
        if inner.running() >= self.max_running {
            return Err(format!(
                "too many runs in progress (limit {}); wait for one to finish or stop one",
                self.max_running
            ));
        }
        let id = loop {
            let id = format!("run_{}", uuid::Uuid::new_v4().simple());
            if !inner.entries.iter().any(|e| e.info.id == id) {
                break id;
            }
        };
        let info = RunInfo {
            id,
            space_id: space_id.to_owned(),
            room_id: room_id.to_owned(),
            actor: actor.to_owned(),
            task: preview(task, MAX_TASK_PREVIEW_CHARS),
            status: RunStatus::Running,
            kind: RunKind::Room,
            agent_id: origin.agent_id,
            trigger_event_id: origin.trigger_event_id,
            started_at: crate::db::now_ms(),
        };
        let (tx, rx) = watch::channel(false);
        inner.entries.push_back(Entry {
            info: info.clone(),
            cancel: Some(tx),
            failure: None,
            settled: false,
            done: watch::channel(false).0,
        });
        Ok((info, rx))
    }

    /// Record how a run ended. Only a `running` run changes: a finished or
    /// cancelled run keeps its status, and `RunStatus::Running` is ignored.
    /// Unknown ids are ignored.
    pub fn finish(&self, id: &str, status: RunStatus) {
        self.finish_with(id, status, None);
    }

    /// Record how a run ended and, for a failed run, why (for its
    /// initiator only), under one lock. A run that already ended (a stop
    /// won the race) keeps its status and gets no failure. Returns the
    /// run's status afterwards: the caller reports that one. `None` when
    /// the registry no longer knows the run: the caller then publishes
    /// nothing, since whatever it would say may be stale.
    pub fn finish_with(
        &self,
        id: &str,
        status: RunStatus,
        failure: Option<RunFailure>,
    ) -> Option<RunStatus> {
        if !status.is_terminal() {
            return self.get(id).map(|r| r.status);
        }
        let mut inner = self.lock();
        let entry = inner.find(id)?;
        entry.settled = true;
        let after = if entry.info.status == RunStatus::Running {
            entry.info.status = status;
            entry.cancel = None;
            if status == RunStatus::Failed {
                entry.failure = failure;
            }
            status
        } else {
            entry.info.status
        };
        inner.evict(self.max_history);
        Some(after)
    }

    /// One run and its initiator-only failure, from one snapshot. The
    /// caller must show the failure only to the run's `actor`.
    pub fn get_with_failure(&self, id: &str) -> Option<(RunInfo, Option<RunFailure>)> {
        self.lock()
            .entries
            .iter()
            .find(|e| e.info.id == id)
            .map(|e| (e.info.clone(), e.failure.clone()))
    }

    /// [`list`](Self::list) with each run's failure, from one snapshot.
    pub fn list_with_failures(&self) -> Vec<(RunInfo, Option<RunFailure>)> {
        self.lock()
            .entries
            .iter()
            .rev()
            .map(|e| (e.info.clone(), e.failure.clone()))
            .collect()
    }

    /// Forget the task preview of runs answering event `event_id` of
    /// `room_id` in `space_id` (the message was deleted). Status and the
    /// rest stay.
    pub fn redact_trigger(&self, space_id: &str, room_id: &str, event_id: i64) {
        for e in self.lock().entries.iter_mut() {
            let i = &mut e.info;
            if i.space_id == space_id
                && i.room_id == room_id
                && i.trigger_event_id == Some(event_id)
            {
                i.task.clear();
            }
        }
    }

    /// Every run held, newest first. Unfiltered: see the module docs.
    pub fn list(&self) -> Vec<RunInfo> {
        self.lock()
            .entries
            .iter()
            .rev()
            .map(|e| e.info.clone())
            .collect()
    }

    /// Resolves (`Ok` or `Err`) once run `id`'s worker is gone and all it
    /// published is stored, or the registry forgot the run. `None` for an
    /// unknown run.
    pub fn done(&self, id: &str) -> Option<watch::Receiver<bool>> {
        self.lock()
            .entries
            .iter()
            .find(|e| e.info.id == id)
            .map(|e| e.done.subscribe())
    }

    pub fn get(&self, id: &str) -> Option<RunInfo> {
        self.lock()
            .entries
            .iter()
            .find(|e| e.info.id == id)
            .map(|e| e.info.clone())
    }

    /// Cancel a running run: mark it `cancelled` and signal its receiver.
    /// Returns the cancelled snapshot even if the worker immediately
    /// settles and evicts the run. `None` if it is missing or already ended.
    pub fn stop(&self, id: &str) -> Option<RunInfo> {
        let mut inner = self.lock();
        let entry = inner.find(id)?;
        if entry.info.status != RunStatus::Running {
            return None;
        }
        entry.info.status = RunStatus::Cancelled;
        let stopped = entry.info.clone();
        if let Some(tx) = entry.cancel.take() {
            // `send_replace` stores the value even if the receiver is gone.
            tx.send_replace(true);
        }
        inner.evict(self.max_history);
        Some(stopped)
    }
}

/// A run's registration, owned by whoever is responsible for ending it:
/// first the code starting it, then its worker. Dropped without the worker
/// having recorded the run's end (the start was abandoned or cancelled
/// before the hand-over, or the worker panicked), it ends the run as
/// failed and settles it, so it never holds a slot or the history.
pub struct Registration {
    runs: std::sync::Arc<RunRegistry>,
    id: String,
}

impl Registration {
    pub fn new(runs: std::sync::Arc<RunRegistry>, id: String) -> Self {
        Self { runs, id }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // A no-op beyond settling when the worker already ended the run.
        self.runs.finish_with(&self.id, RunStatus::Failed, None);
        if let Some(e) = self.runs.lock().find(&self.id) {
            e.done.send_replace(true);
        }
    }
}

/// The first `max` characters of `text`, with an ellipsis when cut.
fn preview(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_owned(),
        Some((end, _)) => format!("{}…", &text[..end]),
    }
}

// ---------------------------------------------------------------------------
// Thread tools
// ---------------------------------------------------------------------------

/// One request from a parent agent about its child threads. Built only from
/// checked model arguments (see [`ThreadAction::validated`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadAction {
    Create { name: String, task: String },
    Send { thread_id: String, message: String },
    Status { thread_id: String },
    Stop { thread_id: String },
}

impl ThreadAction {
    /// Trim and check every field. The tools call this before
    /// [`ThreadControl::execute`]; the server may reuse it on other paths.
    pub fn validated(self) -> Result<Self, String> {
        Ok(match self {
            ThreadAction::Create { name, task } => ThreadAction::Create {
                name: check_name(&name)?,
                task: check_text("task", &task)?,
            },
            ThreadAction::Send { thread_id, message } => ThreadAction::Send {
                thread_id: check_thread_id(&thread_id)?,
                message: check_text("message", &message)?,
            },
            ThreadAction::Status { thread_id } => ThreadAction::Status {
                thread_id: check_thread_id(&thread_id)?,
            },
            ThreadAction::Stop { thread_id } => ThreadAction::Stop {
                thread_id: check_thread_id(&thread_id)?,
            },
        })
    }
}

/// Carries out [`ThreadAction`]s for one parent run.
///
/// The server builds one per parent run and captures, from the trusted
/// request, who started it, the space and room, and the parent run id. The
/// model never supplies any of these. An implementation must:
///
/// - create children under the captured parent, space and actor only;
/// - refuse (with an `Err`) any `thread_id` that is not a child of that
///   parent, without revealing whether it exists elsewhere;
/// - return promptly: `Create` and `Send` should start or queue work and
///   return, not wait for the child to finish;
/// - return short plain text for the model, such as the thread id and
///   status. `Err` text is shown to the model as the tool's failure.
pub trait ThreadControl: Send + Sync {
    fn execute<'a>(
        &'a self,
        action: ThreadAction,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;
}

/// The typed `ToolExecutionError` the thread tools fail with.
pub type ThreadToolError = ToolExecutionError;

fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("`name` must not be empty".into());
    }
    if name.chars().count() > MAX_THREAD_NAME_CHARS {
        return Err(format!(
            "`name` must be at most {MAX_THREAD_NAME_CHARS} characters"
        ));
    }
    if name.chars().any(char::is_control) {
        return Err("`name` must be a single line without control characters".into());
    }
    Ok(name.to_owned())
}

fn check_text(field: &str, text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(format!("`{field}` must not be empty"));
    }
    if text.len() > MAX_THREAD_TEXT_BYTES {
        return Err(format!(
            "`{field}` must be at most {MAX_THREAD_TEXT_BYTES} bytes; summarize it"
        ));
    }
    if text.contains('\0') {
        return Err(format!("`{field}` must not contain NUL"));
    }
    Ok(text.to_owned())
}

fn check_thread_id(id: &str) -> Result<String, String> {
    let id = id.trim();
    if id.is_empty() || id.len() > MAX_THREAD_ID_BYTES {
        return Err(format!(
            "`thread_id` must be 1..={MAX_THREAD_ID_BYTES} bytes, as returned by thread_create"
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
    {
        return Err("`thread_id` must be an id returned by thread_create".into());
    }
    Ok(id.to_owned())
}

async fn run_action(
    control: &dyn ThreadControl,
    action: ThreadAction,
) -> Result<String, ToolExecutionError> {
    let action = action
        .validated()
        .map_err(ToolExecutionError::invalid_args)?;
    match control.execute(action).await {
        Ok(text) => Ok(truncate(&text, MAX_THREAD_OUTPUT_BYTES)),
        Err(e) => Err(ToolExecutionError::other(truncate(
            &e,
            MAX_THREAD_OUTPUT_BYTES,
        ))),
    }
}

fn thread_id_schema() -> Value {
    json!({
        "type": "string",
        "description": "Id of one of your child threads, exactly as returned by thread_create.",
        "minLength": 1,
        "maxLength": MAX_THREAD_ID_BYTES
    })
}

/// Arguments of `thread_create`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadCreateArgs {
    pub name: String,
    pub task: String,
}

/// Arguments of `thread_send`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadSendArgs {
    pub thread_id: String,
    pub message: String,
}

/// Arguments of `thread_status` and `thread_stop`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadIdArgs {
    pub thread_id: String,
}

/// `thread_create`: start a child thread on a self-contained task.
#[derive(Clone)]
pub struct ThreadCreateTool {
    control: Arc<dyn ThreadControl>,
}

/// `thread_send`: send a follow-up message to a child thread.
#[derive(Clone)]
pub struct ThreadSendTool {
    control: Arc<dyn ThreadControl>,
}

/// `thread_status`: read a child thread's status and latest output.
#[derive(Clone)]
pub struct ThreadStatusTool {
    control: Arc<dyn ThreadControl>,
}

/// `thread_stop`: cancel a child thread.
#[derive(Clone)]
pub struct ThreadStopTool {
    control: Arc<dyn ThreadControl>,
}

impl ThreadCreateTool {
    pub fn new(control: Arc<dyn ThreadControl>) -> Self {
        Self { control }
    }
}

impl ThreadSendTool {
    pub fn new(control: Arc<dyn ThreadControl>) -> Self {
        Self { control }
    }
}

impl ThreadStatusTool {
    pub fn new(control: Arc<dyn ThreadControl>) -> Self {
        Self { control }
    }
}

impl ThreadStopTool {
    pub fn new(control: Arc<dyn ThreadControl>) -> Self {
        Self { control }
    }
}

impl Tool for ThreadCreateTool {
    const NAME: &'static str = "thread_create";
    type Args = ThreadCreateArgs;
    type Output = String;
    type Error = ThreadToolError;

    fn description(&self) -> String {
        "Start a child thread: a separate agent that works on one self-contained task in the \
         background while you continue. Returns the new thread's id right away; it does not wait \
         for the result. Use it only for work that can be done independently, create one thread \
         per task, and do not create a second thread for the same task. Write the task so it can \
         be done without this conversation: include the goal, the needed context and what to \
         report back."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Short label for the thread, shown to people (for example \"Check failing tests\").",
                    "minLength": 1,
                    "maxLength": MAX_THREAD_NAME_CHARS
                },
                "task": {
                    "type": "string",
                    "description": "Complete instructions for the child: goal, context and expected answer.",
                    "minLength": 1,
                    "maxLength": MAX_THREAD_TEXT_BYTES
                }
            },
            "required": ["name", "task"],
            "additionalProperties": false
        })
    }

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations {
            title: Some("Start child thread".into()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
        }
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, ToolExecutionError> {
        let action = ThreadAction::Create {
            name: args.name,
            task: args.task,
        };
        run_action(self.control.as_ref(), action).await
    }
}

impl Tool for ThreadSendTool {
    const NAME: &'static str = "thread_send";
    type Args = ThreadSendArgs;
    type Output = String;
    type Error = ThreadToolError;

    fn description(&self) -> String {
        "Send a follow-up message to one of your child threads, such as a correction or an \
         answer to its question. Returns once the message is delivered, not when the child has \
         acted on it. Do not use it to ask for progress; use thread_status for that."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "thread_id": thread_id_schema(),
                "message": {
                    "type": "string",
                    "description": "The message for the child thread.",
                    "minLength": 1,
                    "maxLength": MAX_THREAD_TEXT_BYTES
                }
            },
            "required": ["thread_id", "message"],
            "additionalProperties": false
        })
    }

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations {
            title: Some("Message child thread".into()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
        }
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, ToolExecutionError> {
        let action = ThreadAction::Send {
            thread_id: args.thread_id,
            message: args.message,
        };
        run_action(self.control.as_ref(), action).await
    }
}

impl Tool for ThreadStatusTool {
    const NAME: &'static str = "thread_status";
    type Args = ThreadIdArgs;
    type Output = String;
    type Error = ThreadToolError;

    fn description(&self) -> String {
        "Read one child thread's status (running, completed, failed or cancelled) and its latest \
         output. It does not wait. Check when you need the result; if the thread is still \
         running, do not call this again in a loop: continue with other work or finish your \
         reply and say the thread is still working."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "thread_id": thread_id_schema() },
            "required": ["thread_id"],
            "additionalProperties": false
        })
    }

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations {
            title: Some("Child thread status".into()),
            read_only_hint: Some(true),
            destructive_hint: None,
            idempotent_hint: None,
            open_world_hint: Some(false),
        }
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, ToolExecutionError> {
        let action = ThreadAction::Status {
            thread_id: args.thread_id,
        };
        run_action(self.control.as_ref(), action).await
    }
}

impl Tool for ThreadStopTool {
    const NAME: &'static str = "thread_stop";
    type Args = ThreadIdArgs;
    type Output = String;
    type Error = ThreadToolError;

    fn description(&self) -> String {
        "Stop one of your child threads that is no longer needed. Its unfinished work is \
         discarded. Stopping a thread that has already ended changes nothing."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "thread_id": thread_id_schema() },
            "required": ["thread_id"],
            "additionalProperties": false
        })
    }

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations {
            title: Some("Stop child thread".into()),
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
        }
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, ToolExecutionError> {
        let action = ThreadAction::Stop {
            thread_id: args.thread_id,
        };
        run_action(self.control.as_ref(), action).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn start(reg: &RunRegistry, task: &str) -> (RunInfo, watch::Receiver<bool>) {
        reg.start("space", "room", "user_1", task).unwrap()
    }

    #[test]
    fn status_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(RunStatus::Cancelled).unwrap(),
            json!("cancelled")
        );
        let reg = RunRegistry::new();
        let (info, _rx) = start(&reg, "t");
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["status"], "running");
        assert_eq!(v["actor"], "user_1");
    }

    #[test]
    fn stop_cancels_and_signals() {
        let reg = RunRegistry::new();
        let (info, rx) = start(&reg, "work");
        assert!(!*rx.borrow());
        assert!(reg.stop(&info.id).is_some());
        assert!(*rx.borrow());
        assert_eq!(reg.get(&info.id).unwrap().status, RunStatus::Cancelled);
        // Already ended: no second stop, and finish does not overwrite.
        assert!(reg.stop(&info.id).is_none());
        reg.finish(&info.id, RunStatus::Completed);
        assert_eq!(reg.get(&info.id).unwrap().status, RunStatus::Cancelled);
        assert!(reg.stop("run_missing").is_none());
    }

    #[test]
    fn finish_keeps_first_terminal_status() {
        let reg = RunRegistry::new();
        let (info, _rx) = start(&reg, "work");
        reg.finish(&info.id, RunStatus::Running);
        assert_eq!(reg.get(&info.id).unwrap().status, RunStatus::Running);
        reg.finish(&info.id, RunStatus::Failed);
        reg.finish(&info.id, RunStatus::Completed);
        assert_eq!(reg.get(&info.id).unwrap().status, RunStatus::Failed);
        assert!(reg.stop(&info.id).is_none());
        reg.finish("run_missing", RunStatus::Completed);
    }

    #[test]
    fn a_stopped_run_outlives_the_history_bound_until_its_worker_settles() {
        // History of one finished run.
        let reg = RunRegistry::with_limits(10, 1);
        let (stopped, _rx) = start(&reg, "stopped");
        let cancelled = reg.stop(&stopped.id).unwrap();
        // Other runs finish meanwhile, past the bound.
        for task in ["a", "b", "c"] {
            let (other, _r) = start(&reg, task);
            assert_eq!(
                reg.finish_with(&other.id, RunStatus::Completed, None),
                Some(RunStatus::Completed)
            );
        }
        // The stopped run is still there for its worker, which learns the
        // stop won (rather than publishing its own result).
        assert_eq!(reg.get(&stopped.id).unwrap().status, RunStatus::Cancelled);
        let failure = RunFailure {
            reason: "model_error".into(),
            message: "The agent failed.".into(),
        };
        assert_eq!(
            reg.finish_with(&stopped.id, RunStatus::Failed, Some(failure)),
            Some(RunStatus::Cancelled)
        );
        // Settled, it is evicted like any other finished run.
        let (last, _r) = start(&reg, "last");
        reg.finish_with(&last.id, RunStatus::Completed, None);
        assert!(reg.get(&stopped.id).is_none());
        // An API response assembled after settlement and eviction still
        // has the cancelled snapshot, not the original running snapshot.
        assert_eq!(
            cancelled,
            RunInfo {
                status: RunStatus::Cancelled,
                ..stopped.clone()
            }
        );
        // A run the registry no longer knows: nothing to report.
        assert_eq!(reg.finish_with(&stopped.id, RunStatus::Failed, None), None);
        assert_eq!(
            reg.finish_with("run_missing", RunStatus::Completed, None),
            None
        );
    }

    #[test]
    fn a_won_cancellation_keeps_the_run_cancelled_without_a_failure() {
        let reg = RunRegistry::new();
        let failure = || RunFailure {
            reason: "model_error".into(),
            message: "The agent failed.".into(),
        };
        // A stop lands while the run works out why it failed.
        let (info, _rx) = start(&reg, "raced");
        assert!(reg.stop(&info.id).is_some());
        let status = reg.finish_with(&info.id, RunStatus::Failed, Some(failure()));
        assert_eq!(
            status,
            Some(RunStatus::Cancelled),
            "the caller reports the stop"
        );
        let (run, kept) = reg.get_with_failure(&info.id).unwrap();
        assert_eq!(run.status, RunStatus::Cancelled);
        assert_eq!(kept, None, "a cancelled run carries no failure");
        assert!(
            reg.list_with_failures()
                .iter()
                .all(|(r, f)| r.status != RunStatus::Cancelled || f.is_none())
        );

        // Without a stop, status and failure land together, once.
        let (info, _rx) = start(&reg, "failed");
        let status = reg.finish_with(&info.id, RunStatus::Failed, Some(failure()));
        assert_eq!(status, Some(RunStatus::Failed));
        assert_eq!(
            reg.get_with_failure(&info.id).unwrap(),
            (reg.get(&info.id).unwrap(), Some(failure()))
        );
        assert!(
            reg.stop(&info.id).is_none(),
            "a finished run can't be stopped"
        );
        let later = reg.finish_with(&info.id, RunStatus::Completed, None);
        assert_eq!(later, Some(RunStatus::Failed));
        assert_eq!(reg.get_with_failure(&info.id).unwrap().1, Some(failure()));

        // A failure never sticks to a completed run.
        let (info, _rx) = start(&reg, "done");
        reg.finish_with(&info.id, RunStatus::Completed, Some(failure()));
        assert_eq!(reg.get_with_failure(&info.id).unwrap().1, None);
    }

    #[test]
    fn an_abandoned_registration_ends_and_frees_its_run() {
        let reg = std::sync::Arc::new(RunRegistry::with_limits(1, 10));
        // Registered, then cancelled before any worker took it over.
        let (info, _rx) = reg.start("s", "r", "u", "first").unwrap();
        let registration = Registration::new(reg.clone(), info.id.clone());
        assert!(reg.stop(&info.id).is_some());
        // Unsettled, it still holds the only slot.
        assert!(reg.start("s", "r", "u", "second").is_err());
        // The start is abandoned: its registration settles the run.
        drop(registration);
        assert_eq!(reg.get(&info.id).unwrap().status, RunStatus::Cancelled);
        assert_eq!(reg.get_with_failure(&info.id).unwrap().1, None);
        let (next, _rx) = reg.start("s", "r", "u", "second").unwrap();

        // Abandoned without a stop: the run ends as failed.
        let registration = Registration::new(reg.clone(), next.id.clone());
        drop(registration);
        assert_eq!(reg.get(&next.id).unwrap().status, RunStatus::Failed);
        assert!(reg.start("s", "r", "u", "third").is_ok());
    }

    #[test]
    fn caps_running_runs() {
        let reg = RunRegistry::with_limits(2, 10);
        let (a, _ra) = start(&reg, "a");
        let (_b, _rb) = start(&reg, "b");
        assert!(reg.start("s", "r", "u", "c").is_err());
        reg.finish(&a.id, RunStatus::Completed);
        assert!(reg.start("s", "r", "u", "c").is_ok());
    }

    #[test]
    fn history_is_bounded_without_evicting_running() {
        let reg = RunRegistry::with_limits(3, 2);
        let (long, _rl) = start(&reg, "long-running");
        for i in 0..5 {
            let (info, _rx) = start(&reg, &format!("short {i}"));
            reg.finish(&info.id, RunStatus::Completed);
        }
        let list = reg.list();
        assert_eq!(list.len(), 3);
        // Newest first; the running entry survives although it is the oldest.
        assert_eq!(list[0].task, "short 4");
        assert_eq!(list[1].task, "short 3");
        assert_eq!(list[2].id, long.id);
        assert_eq!(list[2].status, RunStatus::Running);
    }

    #[test]
    fn ids_unique_and_task_bounded() {
        let reg = RunRegistry::with_limits(100, 100);
        let long = "é".repeat(MAX_TASK_PREVIEW_CHARS + 50);
        let mut ids = HashSet::new();
        for _ in 0..50 {
            let (info, _rx) = start(&reg, &long);
            assert!(info.task.chars().count() <= MAX_TASK_PREVIEW_CHARS + 1);
            assert!(info.task.ends_with('…'));
            assert!(ids.insert(info.id));
        }
        assert!(reg.start("s", "r", "u", "   ").is_err());
        assert!(reg.start("", "r", "u", "t").is_err());
        assert!(
            reg.start("s", "r", &"x".repeat(MAX_ID_BYTES + 1), "t")
                .is_err()
        );
    }

    #[test]
    fn args_reject_unknown_fields() {
        assert!(
            serde_json::from_value::<ThreadCreateArgs>(json!({"name": "a", "task": "b"})).is_ok()
        );
        assert!(
            serde_json::from_value::<ThreadCreateArgs>(
                json!({"name": "a", "task": "b", "space_id": "x"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ThreadIdArgs>(json!({"thread_id": "t", "actor": "x"}))
                .is_err()
        );
        assert!(serde_json::from_value::<ThreadSendArgs>(json!({"thread_id": "t"})).is_err());
    }

    #[test]
    fn actions_are_checked() {
        let ok = ThreadAction::Create {
            name: "  Tests ".into(),
            task: " run them ".into(),
        }
        .validated()
        .unwrap();
        assert_eq!(
            ok,
            ThreadAction::Create {
                name: "Tests".into(),
                task: "run them".into()
            }
        );
        let bad = [
            ThreadAction::Create {
                name: "".into(),
                task: "t".into(),
            },
            ThreadAction::Create {
                name: "a\nb".into(),
                task: "t".into(),
            },
            ThreadAction::Create {
                name: "x".repeat(MAX_THREAD_NAME_CHARS + 1),
                task: "t".into(),
            },
            ThreadAction::Create {
                name: "n".into(),
                task: "x".repeat(MAX_THREAD_TEXT_BYTES + 1),
            },
            ThreadAction::Send {
                thread_id: "t".into(),
                message: " ".into(),
            },
            ThreadAction::Status {
                thread_id: "../etc".into(),
            },
            ThreadAction::Stop {
                thread_id: "x".repeat(MAX_THREAD_ID_BYTES + 1),
            },
        ];
        for action in bad {
            assert!(action.clone().validated().is_err(), "{action:?}");
        }
        assert!(
            ThreadAction::Status {
                thread_id: "run_abc-1".into()
            }
            .validated()
            .is_ok()
        );
    }

    struct Fake;

    impl ThreadControl for Fake {
        fn execute<'a>(
            &'a self,
            action: ThreadAction,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>> {
            Box::pin(async move {
                match action {
                    ThreadAction::Create { name, .. } => Ok(format!("created thread_1 ({name})")),
                    ThreadAction::Stop { .. } => Err("not your thread".into()),
                    other => Ok(format!("{other:?}")),
                }
            })
        }
    }

    #[tokio::test]
    async fn tools_validate_then_delegate() {
        let control: Arc<dyn ThreadControl> = Arc::new(Fake);
        let mut cx = ToolContext::default();

        let create = ThreadCreateTool::new(control.clone());
        assert_eq!(ThreadCreateTool::NAME, "thread_create");
        let out = create
            .call(
                &mut cx,
                ThreadCreateArgs {
                    name: " Docs ".into(),
                    task: "write".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(out, "created thread_1 (Docs)");

        let err = create
            .call(
                &mut cx,
                ThreadCreateArgs {
                    name: "".into(),
                    task: "t".into(),
                },
            )
            .await
            .unwrap_err();
        assert!(err.message().contains("name"));

        let stop = ThreadStopTool::new(control.clone());
        let err = stop
            .call(
                &mut cx,
                ThreadIdArgs {
                    thread_id: "thread_9".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.message(), "not your thread");

        assert_eq!(ThreadSendTool::NAME, "thread_send");
        assert_eq!(ThreadStatusTool::NAME, "thread_status");
        assert_eq!(ThreadStopTool::NAME, "thread_stop");
        for params in [
            create.parameters(),
            ThreadSendTool::new(control.clone()).parameters(),
            ThreadStatusTool::new(control.clone()).parameters(),
            stop.parameters(),
        ] {
            assert_eq!(params["additionalProperties"], json!(false));
        }
    }
}
