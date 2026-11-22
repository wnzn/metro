//! One Space's worker: claims that Space's jobs with its dedicated token and
//! answers only `workspace.list` and `workspace.read` from its own folder.
//! Anything else (check, MCP, commands) is refused: this worker runs no
//! programs. A 401 means the token was revoked or replaced; the worker then
//! marks its pairing revoked and stops. It never asks for a new token.

use std::fs::File;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::sync::watch;

use super::folder::Folder;
use super::hosted::{CallError, Claim, Hosted, Job};
use super::store;
use crate::Secret;

pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const IDLE_POLL: Duration = Duration::from_millis(750);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const RESULT_ATTEMPTS: u32 = 6;

pub const NOT_OFFERED: &str = "is not available here: this Space is served by the Metro desktop \
     app's local tools, which only list and read files in the Space's local folder and never run \
     commands or MCP servers";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Starting,
    Online,
    /// The server is unreachable for now; retrying.
    Offline,
    /// The token was revoked or replaced; stopped for good.
    Revoked,
    Stopped,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub state: WorkerState,
    pub detail: Option<String>,
}

pub struct Handle {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    pub status: Arc<Mutex<Status>>,
    pub etag: String,
}

impl Handle {
    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or(Status {
            state: WorkerState::Stopped,
            detail: None,
        })
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Stop and wait (bounded) for the loop to end.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let mut task = self.task;
        if tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
    }
}

/// What the worker needs; all owned, nothing shared with other Spaces but
/// the root handle (from which it opens only its own folder).
pub struct Spec {
    pub hosted: Hosted,
    pub root: Arc<File>,
    /// Serializes pairing-file writes with the manager.
    pub store_lock: Arc<Mutex<()>>,
    pub account_id: String,
    pub space_id: String,
    pub etag: String,
    pub token: Secret,
}

pub fn spawn(spec: Spec) -> Handle {
    let (tx, rx) = watch::channel(false);
    let status = Arc::new(Mutex::new(Status {
        state: WorkerState::Starting,
        detail: None,
    }));
    let etag = spec.etag.clone();
    let task = tokio::spawn(run(spec, rx, status.clone()));
    Handle {
        stop: tx,
        task,
        status,
        etag,
    }
}

fn set(status: &Mutex<Status>, state: WorkerState, detail: Option<String>) {
    if let Ok(mut s) = status.lock() {
        s.state = state;
        s.detail = detail;
    }
}

/// Sleep unless asked to stop; false when stopping.
async fn pause(rx: &mut watch::Receiver<bool>, d: Duration) -> bool {
    if *rx.borrow() {
        return false;
    }
    tokio::select! {
        _ = tokio::time::sleep(d) => !*rx.borrow(),
        _ = rx.changed() => false,
    }
}

async fn run(spec: Spec, mut rx: watch::Receiver<bool>, status: Arc<Mutex<Status>>) {
    let mut backoff = Duration::from_millis(500);
    loop {
        if *rx.borrow() {
            break;
        }
        let claim = tokio::select! {
            c = spec.hosted.claim(&spec.token) => c,
            _ = rx.changed() => break,
        };
        match claim {
            Ok(Claim::Idle) => {
                set(&status, WorkerState::Online, None);
                backoff = Duration::from_millis(500);
                if !pause(&mut rx, IDLE_POLL).await {
                    break;
                }
            }
            Ok(Claim::Invalid { id, reason }) => {
                set(&status, WorkerState::Online, None);
                if !report(&spec, &mut rx, &status, &id, false, &reason).await {
                    break;
                }
            }
            Ok(Claim::Job(job)) => {
                set(&status, WorkerState::Online, None);
                let (ok, output) = execute(&spec, &job).await;
                if !report(&spec, &mut rx, &status, &job.id, ok, &output).await {
                    break;
                }
            }
            Err(CallError::Unauthorized) => {
                mark_revoked(&spec, &status);
                break;
            }
            Err(e) => {
                set(&status, WorkerState::Offline, Some(e.message()));
                if !pause(&mut rx, backoff).await {
                    break;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
    let mut s = status.lock().ok();
    if let Some(s) = s.as_mut()
        && s.state != WorkerState::Revoked
    {
        s.state = WorkerState::Stopped;
    }
}

/// Deliver a result, retrying transient failures (never the tool call).
/// False when the worker must stop (token refused or asked to stop).
async fn report(
    spec: &Spec,
    rx: &mut watch::Receiver<bool>,
    status: &Mutex<Status>,
    id: &str,
    ok: bool,
    output: &str,
) -> bool {
    let mut delay = Duration::from_millis(250);
    for _ in 0..RESULT_ATTEMPTS {
        match spec.hosted.post_result(&spec.token, id, ok, output).await {
            Ok(()) => return true,
            Err(CallError::Unauthorized) => {
                mark_revoked(spec, status);
                return false;
            }
            Err(CallError::Unreachable(_)) => {
                if !pause(rx, delay).await {
                    return false;
                }
                delay = (delay * 2).min(Duration::from_secs(5));
            }
            // Rejected for another reason: don't retry, keep serving.
            Err(_) => return true,
        }
    }
    true
}

/// The server refused our token: remember that (only if the file still
/// holds this same pairing), and stop.
fn mark_revoked(spec: &Spec, status: &Mutex<Status>) {
    set(
        status,
        WorkerState::Revoked,
        Some(
            "the local Metro server no longer accepts this Space's local tools pairing \
             (revoked or replaced); turn local tools on again to pair anew"
                .into(),
        ),
    );
    let _guard = spec.store_lock.lock();
    if let Ok(Some(mut p)) = store::load(&spec.root, &spec.account_id, &spec.space_id)
        && p.etag == spec.etag
        && p.state == store::PairingState::Active
    {
        p.state = store::PairingState::Revoked;
        let _ = store::save(&spec.root, &p);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    #[serde(default)]
    path: Option<String>,
}

async fn execute(spec: &Spec, job: &Job) -> (bool, String) {
    // Defense in depth: a dedicated token serves exactly one Space; a job
    // naming another Space never touches any folder.
    if let Some(sid) = &job.space_id
        && sid != &spec.space_id
    {
        return (
            false,
            "refusing job: it names a different Space than this local tools pairing".into(),
        );
    }
    let tool = job.tool.clone();
    if tool != "workspace.list" && tool != "workspace.read" {
        return (
            false,
            format!("{} {NOT_OFFERED}", crate::secret::clean_line(&tool, 64)),
        );
    }
    let args: PathArgs = match job.arguments.as_object() {
        Some(m) => match serde_json::from_value(Value::Object(Map::clone(m))) {
            Ok(a) => a,
            Err(e) => return (false, format!("invalid arguments: {e}")),
        },
        None => return (false, "arguments must be a JSON object".into()),
    };
    let root = spec.root.clone();
    let (account, sid) = (spec.account_id.clone(), spec.space_id.clone());
    let r = tokio::task::spawn_blocking(move || {
        // Opened fresh per job, without following links, from the root.
        let ws = match store::workspace(&root, &account, &sid, false) {
            Ok(f) => Folder::from_handle(f),
            Err(_) => {
                return (
                    false,
                    "this Space's local folder is missing or is not a plain folder".to_string(),
                );
            }
        };
        run_tool(&ws, &tool, args.path)
    })
    .await
    .unwrap_or_else(|_| (false, "internal error in the local tools".into()));
    (r.0, truncate_utf8(r.1, MAX_OUTPUT_BYTES))
}

/// The two read-only tools, with the runner's output shapes.
pub fn run_tool(ws: &Folder, tool: &str, path: Option<String>) -> (bool, String) {
    match tool {
        "workspace.list" => {
            let path = path.unwrap_or_else(|| ".".into());
            match ws.list(&path) {
                Ok(l) => (true, serde_json::to_string_pretty(&l).unwrap_or_default()),
                Err(e) => (false, format!("workspace.list {path:?}: {e}")),
            }
        }
        "workspace.read" => {
            let Some(path) = path.filter(|p| !p.is_empty()) else {
                return (false, "invalid arguments: missing field `path`".into());
            };
            match ws.read(&path) {
                Ok(c) if c.truncated => (
                    true,
                    format!(
                        "{}\n[metro-desktop: file truncated, {} bytes total, first {} shown]",
                        c.text,
                        c.size,
                        c.text.len()
                    ),
                ),
                Ok(c) => (true, c.text),
                Err(e) => (false, format!("workspace.read {path:?}: {e}")),
            }
        }
        other => (false, format!("{other} {NOT_OFFERED}")),
    }
}

pub fn truncate_utf8(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s
}
