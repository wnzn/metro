//! The desktop's personal registered runner on its own hosted Metro server,
//! requested by the installer.
//!
//! This is a real registered runner (`POST /v1/runners`, `visibility:
//! personal`), owned by the local account and shown in its runner list like
//! any other. It is **not** the per-Space dedicated local tools of the parent
//! module and never stands in for them, or they for it.
//!
//! - **Requested by the installer only.** Nothing happens unless an
//!   installer-made marker file ([`MARKER`], zero length, a regular file,
//!   not a link) sits at the agreed place ([`marker_path`]): next to the
//!   launcher on Windows and Linux; on macOS beside the outer `Metro.app`,
//!   inside a private install folder dedicated to it (never inside the
//!   signed bundle). See [`installer_marker`].
//! - **Own hosted server only.** It runs only with a [`HostedSession`] (the
//!   bridge's session is exactly this app's running local server).
//! - **One runner, created once.** The first bound session creates it and
//!   keeps its token in one private file (mode 600, written atomically)
//!   tied to that server and account. Later starts resume it; nothing is
//!   created on each start. If the runner is deleted or its token rotated
//!   or revoked elsewhere, it is **not** recreated automatically: the state
//!   says so, and only an explicit reconnect (`POST
//!   /v1/local-tools/runner/reconnect`) issues a new token or runner. An
//!   installer update changes nothing here: the record lives in the desktop
//!   root, not in the install folder.
//! - **Crash-safe creation.** A `creating` record (with a unique runner
//!   name) is written before the create call. If the answer is lost, the
//!   next start looks for exactly that name and, if found, rotates its token
//!   (nobody else ever held one); if not found it stops as `interrupted`
//!   rather than create another runner.
//! - **No Space is attached by Metro.** The Space owner attaches it in the
//!   existing Space runner settings. Metro never attaches, detaches or
//!   replaces any Space's runner.
//! - **Read-only, confined, no programs.** Claims with protocol 2, requires
//!   every job to name a valid Space, and serves `workspace.list` and
//!   `workspace.read` from that Space's own private `spaces/<sid>/` folder,
//!   opened handle-relative without following links ([`super::folder`]).
//!   Check, MCP and commands are refused; no Docker, no inbound port.
//! - **Stops** on sign-out, server or account switch, and app exit.
//!
//! ```text
//! <local-tools root>/runner/<sha256(server, account)[..16]>/
//!   runner.json     600: state, runner id + name, the runner token
//!   spaces/<sid>/   the only folder a job for <sid> reads
//! ```

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::RngCore;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use zeroize::Zeroize;

use super::folder::Folder;
use super::hosted::{CallError, Claim, CreateError, Hosted, Job, RunnerInfo, SpaceRunner};
use super::store::{read_capped, subdir, valid_space_id, write_atomic};
use super::{HostedSession, ToolsError, from_call};
use crate::Secret;

/// The installer's request, a zero-length regular file.
pub const MARKER: &str = "enable-local-runner";
const RUNNER_DIR: &str = "runner";
const SPACES: &str = "spaces";
const RECORD: &str = "runner.json";
const RECORD_TMP: &str = "runner.json.tmp";
const MAX_RECORD_BYTES: u64 = 16 * 1024;
const IDLE_POLL: Duration = Duration::from_millis(750);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const RESULT_ATTEMPTS: u32 = 6;
pub const TOOLS: [&str; 2] = ["workspace.list", "workspace.read"];

pub const NOT_OFFERED: &str = "is not available here: this Space is served by the Metro desktop \
     app's personal runner, which only lists and reads files in the Space's folder on this \
     computer and never runs commands or MCP servers";

// ---------------------------------------------------------------------------
// Installer marker
// ---------------------------------------------------------------------------

/// What the installer asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    /// No marker: no personal runner is provisioned or run.
    Absent,
    Accepted,
    /// A marker exists but isn't trustworthy; nothing is provisioned.
    Rejected(String),
}

impl Marker {
    pub fn accepted(&self) -> bool {
        *self == Marker::Accepted
    }
}

/// Where the marker must be for a launcher in `exe_dir` (its canonical
/// folder). Windows, Linux: `<exe_dir>/enable-local-runner`. macOS: the
/// launcher is `<parent>/<Name>.app/Contents/MacOS/<launcher>`, and the
/// marker is `<parent>/enable-local-runner`, beside the outer app bundle
/// (never inside it, so the signed bundle stays unmodified).
pub fn marker_path(exe_dir: &Path) -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        mac_install_parent(exe_dir).map(|(parent, _)| parent.join(MARKER))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(exe_dir.join(MARKER))
    }
}

/// `(<parent>, <Name>.app)` for a launcher in `<parent>/<Name>.app/Contents/MacOS`,
/// where that bundle is the outermost one.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mac_install_parent(exe_dir: &Path) -> Result<(PathBuf, String), String> {
    let bad = || "Metro isn't in the expected Metro.app/Contents/MacOS layout".to_string();
    if exe_dir.file_name().and_then(|n| n.to_str()) != Some("MacOS") {
        return Err(bad());
    }
    let contents = exe_dir.parent().ok_or_else(bad)?;
    if contents.file_name().and_then(|n| n.to_str()) != Some("Contents") {
        return Err(bad());
    }
    let app = contents.parent().ok_or_else(bad)?;
    let app_name = app
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| n.ends_with(".app") && n.len() > 4)
        .ok_or_else(bad)?;
    let parent = app.parent().ok_or_else(bad)?;
    if parent.ancestors().any(|a| {
        a.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
    }) {
        return Err("Metro.app is inside another app bundle".into());
    }
    Ok((parent.to_path_buf(), app_name.to_owned()))
}

/// Check the installer's marker for a launcher in `exe_dir`. Accepted only
/// when it is a zero-length regular file (no link, no reparse point). On
/// Unix it must be owned by this user (or, outside macOS, root) and its
/// folder must not be writable by group or others. On macOS its folder must
/// also be the private (no group/other access), user-owned install folder
/// dedicated to the app: it holds only the app bundle, the marker and
/// Finder's `.DS_Store`.
pub fn installer_marker(exe_dir: &Path) -> Marker {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        // A launcher outside the expected layout (a developer build) has
        // no place for a marker.
        let Ok((parent, app)) = mac_install_parent(exe_dir) else {
            return Marker::Absent;
        };
        check_mac(&parent, &app, uid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        check_marker_at(exe_dir)
    }
}

/// The macOS rules, for an app at `<parent>/<app>`; `uid` is the user.
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn check_mac(parent: &Path, app: &str, uid: u32) -> Marker {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(parent.join(MARKER)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Marker::Absent,
        _ => {}
    }
    let reject = |m: &str| Marker::Rejected(m.to_owned());
    let Ok(meta) = std::fs::symlink_metadata(parent) else {
        return reject("Metro's install folder can't be checked");
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return reject("Metro's install folder is not a plain folder");
    }
    if meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return reject(
            "Metro's install folder must belong to you and be private (no access for others)",
        );
    }
    let Ok(entries) = std::fs::read_dir(parent) else {
        return reject("Metro's install folder can't be read");
    };
    for ent in entries {
        let Ok(ent) = ent else {
            return reject("Metro's install folder can't be read");
        };
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if name != app && name != MARKER && name != ".DS_Store" {
            return reject(
                "Metro's install folder must hold only Metro.app and the installer's marker",
            );
        }
    }
    check_file(&parent.join(MARKER), Some(uid), true)
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn check_marker_at(dir: &Path) -> Marker {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        check_file(&dir.join(MARKER), Some(uid), false)
    }
    #[cfg(not(unix))]
    {
        check_file(&dir.join(MARKER), None, false)
    }
}

/// `owner`: the user who must own it (Unix). `only_owner`: root doesn't
/// count as a trusted owner.
fn check_file(path: &Path, owner: Option<u32>, only_owner: bool) -> Marker {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Marker::Absent,
        Err(_) => return Marker::Rejected("the installer's marker can't be checked".into()),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Marker::Rejected("the installer's marker is not a regular file".into());
    }
    if meta.len() != 0 {
        return Marker::Rejected("the installer's marker is not empty".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Marker::Rejected("the installer's marker is a link".into());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Some(uid) = owner {
            let ok = meta.uid() == uid || (!only_owner && meta.uid() == 0);
            if !ok {
                return Marker::Rejected("the installer's marker belongs to someone else".into());
            }
        }
        if meta.nlink() != 1 {
            return Marker::Rejected("the installer's marker has other links".into());
        }
        let Some(dir) = path.parent() else {
            return Marker::Rejected("the installer's marker has no folder".into());
        };
        match std::fs::symlink_metadata(dir) {
            Ok(d) if d.is_dir() && d.mode() & 0o022 == 0 => {}
            _ => {
                return Marker::Rejected(
                    "the folder of the installer's marker is writable by others".into(),
                );
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (owner, only_owner);
    Marker::Accepted
}

// ---------------------------------------------------------------------------
// The record
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordState {
    /// The create call was (about to be) sent; its answer isn't recorded.
    Creating,
    Active,
    /// The server refused the token (rotated, revoked or runner deleted).
    Revoked,
    /// The runner is no longer on the server.
    Deleted,
    /// A creation was interrupted and the runner wasn't found afterwards.
    Interrupted,
}

impl RecordState {
    fn as_str(self) -> &'static str {
        match self {
            RecordState::Creating => "creating",
            RecordState::Active => "active",
            RecordState::Revoked => "revoked",
            RecordState::Deleted => "deleted",
            RecordState::Interrupted => "interrupted",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "creating" => RecordState::Creating,
            "active" => RecordState::Active,
            "revoked" => RecordState::Revoked,
            "deleted" => RecordState::Deleted,
            "interrupted" => RecordState::Interrupted,
            _ => return None,
        })
    }
}

/// This computer's runner on one server for one account.
#[derive(Debug, Clone)]
pub struct Record {
    pub server_url: String,
    pub account_id: String,
    pub name: String,
    pub state: RecordState,
    pub runner_id: Option<String>,
    pub token: Option<Secret>,
}

impl Record {
    fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "v": 1,
            "server_url": self.server_url,
            "account_id": self.account_id,
            "name": self.name,
            "state": self.state.as_str(),
            "runner_id": self.runner_id,
            "token": self.token.as_ref().map(Secret::expose),
        }))
        .expect("serializable")
    }

    fn from_json(bytes: &[u8]) -> Option<Self> {
        let mut v: Value = serde_json::from_slice(bytes).ok()?;
        let o = v.as_object_mut()?;
        if o.get("v")?.as_u64()? != 1 {
            return None;
        }
        let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_owned);
        let state = RecordState::parse(&s("state")?)?;
        let (server_url, account_id, name) = (s("server_url")?, s("account_id")?, s("name")?);
        let runner_id = s("runner_id");
        let token = match o.get_mut("token") {
            Some(Value::String(t)) => {
                let out = Secret::new(t.as_str());
                t.zeroize();
                Some(out)
            }
            _ => None,
        };
        if state == RecordState::Active && (runner_id.is_none() || token.is_none()) {
            return None;
        }
        Some(Record {
            server_url,
            account_id,
            name,
            state,
            runner_id,
            token,
        })
    }
}

/// The directory name for one (server, account): a hash, so any shape is
/// safe and another server or account never shares a record or folder.
pub fn identity_dir(server_url: &str, account_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"metro-desktop-runner\0");
    h.update(server_url.as_bytes());
    h.update(b"\0");
    h.update(account_id.as_bytes());
    h.finalize()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `<root>/runner/<identity>/`, created private when `create`.
fn identity_handle(root: &File, h: &HostedSession, create: bool) -> io::Result<File> {
    let r = subdir(root, RUNNER_DIR, create)?;
    subdir(&r, &identity_dir(&h.server_url, &h.account_id), create)
}

/// Where the Space folders of this (server, account) are, for showing.
pub fn spaces_path(root_path: &Path, server_url: &str, account_id: &str) -> PathBuf {
    root_path
        .join(RUNNER_DIR)
        .join(identity_dir(server_url, account_id))
        .join(SPACES)
}

/// This identity's record. One made for another server or account (copied
/// or tampered) counts as absent.
pub fn load(root: &File, h: &HostedSession) -> io::Result<Option<Record>> {
    let dir = match identity_handle(root, h, false) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let Some(bytes) = read_capped(&dir, RECORD, MAX_RECORD_BYTES)? else {
        return Ok(None);
    };
    let Some(mut bytes) = bytes else {
        return Ok(None);
    };
    let rec = Record::from_json(&bytes);
    bytes.zeroize();
    Ok(rec.filter(|r| r.server_url == h.server_url && r.account_id == h.account_id))
}

/// Write this identity's record atomically, mode 600.
pub fn save(root: &File, h: &HostedSession, rec: &Record) -> io::Result<()> {
    if rec.server_url != h.server_url || rec.account_id != h.account_id {
        return Err(io::Error::other("record for another identity"));
    }
    let dir = identity_handle(root, h, true)?;
    let mut data = rec.to_json();
    let r = write_atomic(&dir, RECORD, RECORD_TMP, &data);
    data.zeroize();
    r
}

fn remove_record(root: &File, h: &HostedSession) -> io::Result<()> {
    let dir = match identity_handle(root, h, false) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for name in [RECORD_TMP, RECORD] {
        match cap_primitives::fs::remove_file(&dir, Path::new(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// The Space's folder (`spaces/<sid>/`), created private if needed, opened
/// without following links.
pub fn space_folder(root: &File, h: &HostedSession, sid: &str) -> io::Result<File> {
    if !valid_space_id(sid) {
        return Err(io::Error::other("invalid Space id"));
    }
    let d = identity_handle(root, h, true)?;
    let s = subdir(&d, SPACES, true)?;
    subdir(&s, sid, true)
}

// ---------------------------------------------------------------------------
// State shown to the person
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerState {
    /// The installer didn't ask for it.
    NotRequested,
    /// Asked for, but the bound session isn't this app's own server.
    Unavailable,
    /// Asked for and own server bound, but not started yet.
    Idle,
    Starting,
    Online,
    /// The server is unreachable for now; retrying.
    Offline,
    Revoked,
    Deleted,
    Interrupted,
    Failed,
    Stopped,
}

#[derive(Debug, Clone)]
struct Note {
    state: RunnerState,
    detail: Option<String>,
}

const REVOKED: &str = "This computer's runner is no longer accepted by the local Metro server \
     (its token was rotated or revoked, or the runner was deleted). Metro doesn't recreate it on \
     its own; reconnect it to continue.";
const DELETED: &str = "This computer's runner was deleted from the local Metro server. Metro \
     doesn't recreate it on its own; reconnect it to create a new one.";
const INTERRUPTED: &str = "Setting up this computer's runner was interrupted and Metro couldn't \
     confirm it was created. To avoid duplicates it won't try again on its own; reconnect it to \
     finish.";

fn note(state: RunnerState, detail: Option<&str>) -> Note {
    Note {
        state,
        detail: detail.map(|d| crate::secret::clean_line(d, 300)),
    }
}

struct Handle {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    status: Arc<Mutex<Note>>,
    runner_id: String,
}

impl Handle {
    fn note(&self) -> Note {
        self.status
            .lock()
            .map(|n| n.clone())
            .unwrap_or_else(|_| note(RunnerState::Stopped, None))
    }

    fn live(&self) -> bool {
        !self.task.is_finished() && self.note().state != RunnerState::Revoked
    }

    async fn stop(self) {
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

/// The runner's slot in the tools manager (behind its lock).
#[derive(Default)]
pub struct Slot {
    worker: Option<Handle>,
    last: Option<Note>,
}

impl Slot {
    /// Stop the worker (session switch, sign-out, exit).
    pub async fn stop(&mut self) {
        if let Some(w) = self.worker.take() {
            w.stop().await;
        }
        self.last = None;
    }

    fn set(&mut self, n: Note) {
        self.last = Some(n);
    }
}

/// What the manager hands each call.
pub struct Ctx<'a> {
    pub marker: &'a Marker,
    pub root: &'a Arc<File>,
    pub root_path: &'a Path,
    pub hosted: &'a HostedSession,
    pub api: &'a Hosted,
    pub store_lock: &'a Arc<Mutex<()>>,
}

impl Ctx<'_> {
    fn load(&self) -> io::Result<Option<Record>> {
        let _g = self.store_lock.lock();
        load(self.root, self.hosted)
    }
    fn save(&self, r: &Record) -> io::Result<()> {
        let _g = self.store_lock.lock();
        save(self.root, self.hosted, r)
    }
    fn remove(&self) -> io::Result<()> {
        let _g = self.store_lock.lock();
        remove_record(self.root, self.hosted)
    }
}

/// The `local_runner` block of the status.
pub fn status(
    slot: &Slot,
    marker: &Marker,
    cx: Option<(&Path, &HostedSession, Option<Record>)>,
) -> Value {
    let (marker_s, marker_detail) = match marker {
        Marker::Absent => ("absent", None),
        Marker::Accepted => ("accepted", None),
        Marker::Rejected(m) => ("rejected", Some(m.clone())),
    };
    let base = |state: RunnerState, detail: Option<String>| {
        json!({
            "requested": marker.accepted(),
            "marker": marker_s,
            "marker_detail": marker_detail,
            "available": false,
            "state": state,
            "detail": detail,
            "runner": null,
            "spaces_folder": null,
            "tools": TOOLS,
            "can_reconnect": false,
        })
    };
    if !marker.accepted() {
        return base(RunnerState::NotRequested, marker_detail.clone());
    }
    let Some((root_path, h, rec)) = cx else {
        return base(
            RunnerState::Unavailable,
            Some(
                "This computer's runner works only in this computer's own Metro workspace. \
                 Open the local workspace to use it."
                    .into(),
            ),
        );
    };
    let n = match &slot.worker {
        Some(w) => w.note(),
        None => slot
            .last
            .clone()
            .unwrap_or_else(|| note(RunnerState::Idle, None)),
    };
    let mut v = base(n.state, n.detail);
    v["available"] = json!(true);
    v["spaces_folder"] =
        json!(spaces_path(root_path, &h.server_url, &h.account_id).to_string_lossy());
    if let Some(id) = rec
        .as_ref()
        .and_then(|r| r.runner_id.as_ref().map(|i| (i, &r.name)))
    {
        v["runner"] = json!({"id": id.0, "name": id.1});
    }
    v["can_reconnect"] = json!(!slot.worker.as_ref().is_some_and(Handle::live));
    v
}

fn spawn(cx: &Ctx<'_>, runner_id: String, token: Secret) -> Handle {
    let (tx, rx) = watch::channel(false);
    let status = Arc::new(Mutex::new(note(RunnerState::Starting, None)));
    let spec = Spec {
        hosted: cx.api.clone(),
        root: cx.root.clone(),
        store_lock: cx.store_lock.clone(),
        session: cx.hosted.clone(),
        runner_id: runner_id.clone(),
        token,
    };
    let task = tokio::spawn(run(spec, rx, status.clone()));
    Handle {
        stop: tx,
        task,
        status,
        runner_id,
    }
}

fn new_name() -> String {
    let mut b = [0u8; 3];
    rand::rng().fill_bytes(&mut b);
    let tag: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("Metro desktop on this computer ({tag})")
}

fn fs_failed(slot: &mut Slot, what: &str) {
    slot.set(note(
        RunnerState::Failed,
        Some(&format!(
            "Metro couldn't {what} for this computer's runner in its private folder."
        )),
    ));
}

/// After a session of this account on the own server is bound: resume the
/// runner, or create it the first time. Never recreates one that was
/// revoked or deleted.
pub async fn ensure(slot: &mut Slot, cx: &Ctx<'_>) {
    if !cx.marker.accepted() {
        slot.stop().await;
        return;
    }
    if slot.worker.as_ref().is_some_and(Handle::live) {
        return;
    }
    if let Some(w) = slot.worker.take() {
        let n = w.note();
        w.stop().await;
        slot.set(n);
    }
    let rec = match cx.load() {
        Ok(r) => r,
        Err(_) => return fs_failed(slot, "read its record"),
    };
    match rec {
        None => provision(slot, cx).await,
        Some(r) => match r.state {
            RecordState::Creating => recover(slot, cx, r).await,
            RecordState::Active => start(slot, cx, r).await,
            RecordState::Revoked => slot.set(note(RunnerState::Revoked, Some(REVOKED))),
            RecordState::Deleted => slot.set(note(RunnerState::Deleted, Some(DELETED))),
            RecordState::Interrupted => slot.set(note(RunnerState::Interrupted, Some(INTERRUPTED))),
        },
    }
}

/// The person explicitly asks to reconnect: issue a new token for the same
/// runner if it still exists, else create a new one. Idempotent while the
/// runner is running.
pub(super) async fn reconnect(slot: &mut Slot, cx: &Ctx<'_>) -> Result<(), ToolsError> {
    if !cx.marker.accepted() {
        return Err(ToolsError::new(
            409,
            "not_requested",
            "This installation didn't ask for a runner on this computer.",
        ));
    }
    if slot.worker.as_ref().is_some_and(Handle::live) {
        return Ok(());
    }
    if let Some(w) = slot.worker.take() {
        w.stop().await;
    }
    let rec = cx
        .load()
        .map_err(|_| super::fs_error("read the runner's record"))?;
    let Some(rec) = rec else {
        provision(slot, cx).await;
        return Ok(());
    };
    match rec.state {
        RecordState::Creating => recover(slot, cx, rec).await,
        RecordState::Active => start(slot, cx, rec).await,
        RecordState::Revoked | RecordState::Deleted | RecordState::Interrupted => {
            let list = cx
                .api
                .personal_runners(&cx.hosted.token)
                .await
                .map_err(from_call)?;
            let same = list.iter().find(|r| {
                Some(&r.id) == rec.runner_id.as_ref()
                    || (rec.runner_id.is_none() && r.name == rec.name)
            });
            match same {
                Some(info) => {
                    let info = info.clone();
                    rotate_into(slot, cx, &rec, &info).await?;
                }
                None => provision(slot, cx).await,
            }
        }
    }
    match slot.last.as_ref().map(|n| n.state) {
        Some(RunnerState::Failed) if slot.worker.is_none() => Err(ToolsError::new(
            502,
            "runner_failed",
            slot.last
                .as_ref()
                .and_then(|n| n.detail.clone())
                .unwrap_or_default(),
        )),
        _ => Ok(()),
    }
}

async fn start(slot: &mut Slot, cx: &Ctx<'_>, rec: Record) {
    let (Some(id), Some(token)) = (rec.runner_id.clone(), rec.token.clone()) else {
        return fs_failed(slot, "read its record");
    };
    // Still ours on the server? If the server says it's gone, say so and
    // don't recreate. If it can't be asked now, let the worker find out.
    match cx.api.personal_runners(&cx.hosted.token).await {
        Ok(list) if !list.iter().any(|r| r.id == id) => {
            let mut r = rec;
            r.state = RecordState::Deleted;
            r.token = None;
            let _ = cx.save(&r);
            slot.set(note(RunnerState::Deleted, Some(DELETED)));
            return;
        }
        Err(CallError::Unauthorized) => {
            slot.set(note(
                RunnerState::Failed,
                Some("The local Metro server didn't accept this sign-in."),
            ));
            return;
        }
        _ => {}
    }
    slot.worker = Some(spawn(cx, id, token));
}

/// Create the runner (first time, or explicitly after it was deleted).
async fn provision(slot: &mut Slot, cx: &Ctx<'_>) {
    let mut rec = Record {
        server_url: cx.hosted.server_url.clone(),
        account_id: cx.hosted.account_id.clone(),
        name: new_name(),
        state: RecordState::Creating,
        runner_id: None,
        token: None,
    };
    // The intent is on disk before the server can create anything.
    if cx.save(&rec).is_err() {
        return fs_failed(slot, "save its record");
    }
    slot.set(note(RunnerState::Starting, None));
    match cx
        .api
        .create_personal_runner(&cx.hosted.token, &rec.name)
        .await
    {
        Ok((info, token)) => {
            rec.state = RecordState::Active;
            rec.runner_id = Some(info.id.clone());
            rec.token = Some(token.clone());
            if cx.save(&rec).is_err() {
                // Nobody could use it: take it back if we can. If that fails
                // too, the `creating` record lets the next start find it by
                // its name.
                if cx
                    .api
                    .delete_runner(&cx.hosted.token, &info.id)
                    .await
                    .is_ok()
                {
                    let _ = cx.remove();
                }
                return fs_failed(slot, "save its token");
            }
            slot.worker = Some(spawn(cx, info.id, token));
        }
        Err(CreateError {
            definite: true,
            error,
        }) => {
            // The server refused: nothing was created.
            let _ = cx.remove();
            slot.set(note(
                RunnerState::Failed,
                Some(&format!(
                    "The local Metro server didn't create this computer's runner: {}",
                    error.message()
                )),
            ));
        }
        Err(CreateError {
            definite: false, ..
        }) => {
            // The answer was lost: the runner may or may not exist.
            recover(slot, cx, rec).await;
        }
    }
}

/// A `creating` record: find exactly that runner by its unique name and
/// give it a token we hold; if it isn't there, stop as interrupted.
async fn recover(slot: &mut Slot, cx: &Ctx<'_>, rec: Record) {
    let list = match cx.api.personal_runners(&cx.hosted.token).await {
        Ok(l) => l,
        Err(e) => {
            slot.set(note(
                RunnerState::Offline,
                Some(&format!(
                    "Metro couldn't finish setting up this computer's runner and will check \
                     again next time: {}",
                    e.message()
                )),
            ));
            return;
        }
    };
    let found: Vec<&RunnerInfo> = list.iter().filter(|r| r.name == rec.name).collect();
    if found.len() != 1 {
        let mut r = rec;
        r.state = RecordState::Interrupted;
        let _ = cx.save(&r);
        slot.set(note(RunnerState::Interrupted, Some(INTERRUPTED)));
        return;
    }
    let info = found[0].clone();
    if let Err(e) = rotate_into(slot, cx, &rec, &info).await
        && e.code == "not_found"
    {
        let mut r = rec;
        r.state = RecordState::Interrupted;
        let _ = cx.save(&r);
        slot.set(note(RunnerState::Interrupted, Some(INTERRUPTED)));
    }
}

/// Give `info` (our own runner) a new token, save it, start serving.
async fn rotate_into(
    slot: &mut Slot,
    cx: &Ctx<'_>,
    rec: &Record,
    info: &RunnerInfo,
) -> Result<(), ToolsError> {
    let token = match cx.api.rotate_runner(&cx.hosted.token, &info.id).await {
        Ok(t) => t,
        Err(e) => {
            let err = from_call(e);
            slot.set(note(RunnerState::Failed, Some(&err.message)));
            return Err(err);
        }
    };
    let next = Record {
        server_url: rec.server_url.clone(),
        account_id: rec.account_id.clone(),
        name: info.name.clone(),
        state: RecordState::Active,
        runner_id: Some(info.id.clone()),
        token: Some(token.clone()),
    };
    if cx.save(&next).is_err() {
        fs_failed(slot, "save its token");
        return Err(super::fs_error("save the runner's token"));
    }
    slot.worker = Some(spawn(cx, info.id.clone(), token));
    Ok(())
}

/// `POST /v1/local-tools/runner/spaces/{sid}/folder`: the Space's folder on
/// this computer (created if needed) and whether this runner serves it.
/// Members only; attaching stays in the Space's runner settings.
pub(super) async fn folder_for(cx: &Ctx<'_>, sid: &str) -> Result<Value, ToolsError> {
    if !cx.marker.accepted() {
        return Err(ToolsError::new(
            409,
            "not_requested",
            "This installation didn't ask for a runner on this computer.",
        ));
    }
    if !valid_space_id(sid) {
        return Err(ToolsError::new(404, "not_found", "no such Space"));
    }
    let rec = cx
        .load()
        .map_err(|_| super::fs_error("read the runner's record"))?
        .filter(|r| r.state == RecordState::Active)
        .ok_or_else(|| {
            ToolsError::new(
                409,
                "runner_inactive",
                "This computer's runner isn't set up.",
            )
        })?;
    if cx
        .api
        .role(&cx.hosted.token, sid)
        .await
        .map_err(from_call)?
        .is_none()
    {
        return Err(ToolsError::new(404, "not_found", "no such Space"));
    }
    let attached = match cx.api.space_runner(&cx.hosted.token, sid).await {
        Ok(SpaceRunner::Registered(name)) => name == crate::secret::clean_line(&rec.name, 80),
        Ok(_) => false,
        Err(e) => return Err(from_call(e)),
    };
    {
        let _g = cx.store_lock.lock();
        space_folder(cx.root, cx.hosted, sid)
            .map_err(|_| super::fs_error("create the Space's folder"))?;
    }
    let path = spaces_path(cx.root_path, &cx.hosted.server_url, &cx.hosted.account_id).join(sid);
    Ok(json!({
        "space_id": sid,
        "folder": path.to_string_lossy(),
        "attached": attached,
        "tools": TOOLS,
    }))
}

// ---------------------------------------------------------------------------
// The worker
// ---------------------------------------------------------------------------

struct Spec {
    hosted: Hosted,
    root: Arc<File>,
    store_lock: Arc<Mutex<()>>,
    session: HostedSession,
    runner_id: String,
    token: Secret,
}

fn set(status: &Mutex<Note>, state: RunnerState, detail: Option<String>) {
    if let Ok(mut s) = status.lock() {
        s.state = state;
        s.detail = detail;
    }
}

async fn pause(rx: &mut watch::Receiver<bool>, d: Duration) -> bool {
    if *rx.borrow() {
        return false;
    }
    tokio::select! {
        _ = tokio::time::sleep(d) => !*rx.borrow(),
        _ = rx.changed() => false,
    }
}

async fn run(spec: Spec, mut rx: watch::Receiver<bool>, status: Arc<Mutex<Note>>) {
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
                set(&status, RunnerState::Online, None);
                backoff = Duration::from_millis(500);
                if !pause(&mut rx, IDLE_POLL).await {
                    break;
                }
            }
            Ok(Claim::Invalid { id, reason }) => {
                set(&status, RunnerState::Online, None);
                if !report(&spec, &mut rx, &status, &id, false, &reason).await {
                    break;
                }
            }
            Ok(Claim::Job(job)) => {
                set(&status, RunnerState::Online, None);
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
                set(&status, RunnerState::Offline, Some(e.message()));
                if !pause(&mut rx, backoff).await {
                    break;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
    if let Ok(mut s) = status.lock()
        && s.state != RunnerState::Revoked
    {
        s.state = RunnerState::Stopped;
    }
}

async fn report(
    spec: &Spec,
    rx: &mut watch::Receiver<bool>,
    status: &Mutex<Note>,
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
            Err(_) => return true,
        }
    }
    true
}

/// The server refused the token: record it (only if the record still holds
/// this same runner and token) and stop. Never asks for a new one.
fn mark_revoked(spec: &Spec, status: &Mutex<Note>) {
    set(status, RunnerState::Revoked, Some(REVOKED.into()));
    let _g = spec.store_lock.lock();
    if let Ok(Some(mut r)) = load(&spec.root, &spec.session)
        && r.state == RecordState::Active
        && r.runner_id.as_deref() == Some(spec.runner_id.as_str())
        && r.token.as_ref().map(Secret::expose) == Some(spec.token.expose())
    {
        r.state = RecordState::Revoked;
        r.token = None;
        let _ = save(&spec.root, &spec.session, &r);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    #[serde(default)]
    path: Option<String>,
}

async fn execute(spec: &Spec, job: &Job) -> (bool, String) {
    // Protocol 2: every job names its Space. No Space, or one that can't be
    // a folder name, touches nothing.
    let Some(sid) = job.space_id.clone().filter(|s| valid_space_id(s)) else {
        return (
            false,
            "refusing job: it doesn't name a valid Space for this runner".into(),
        );
    };
    let tool = job.tool.clone();
    if !TOOLS.contains(&tool.as_str()) {
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
    let (root, session, lock) = (
        spec.root.clone(),
        spec.session.clone(),
        spec.store_lock.clone(),
    );
    let r = tokio::task::spawn_blocking(move || {
        // Opened fresh per job, from the root handle, without following
        // links: only this Space's own folder.
        let opened = {
            let _g = lock.lock();
            space_folder(&root, &session, &sid)
        };
        let ws = match opened {
            Ok(f) => Folder::from_handle(f),
            Err(_) => {
                return (
                    false,
                    "this Space's folder on this computer is missing or is not a plain folder"
                        .to_string(),
                );
            }
        };
        super::worker::run_tool(&ws, &tool, args.path)
    })
    .await
    .unwrap_or_else(|_| (false, "internal error in this computer's runner".into()));
    (
        r.0,
        super::worker::truncate_utf8(r.1, super::worker::MAX_OUTPUT_BYTES),
    )
}

/// The runner id the worker serves, if one runs (tests, status).
pub fn running_id(slot: &Slot) -> Option<&str> {
    slot.worker
        .as_ref()
        .filter(|w| !w.task.is_finished())
        .map(|w| w.runner_id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_must_be_an_empty_regular_file() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("app");
        std::fs::create_dir(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(check_marker_at(&dir), Marker::Absent);
        std::fs::write(dir.join(MARKER), b"x").unwrap();
        assert!(matches!(check_marker_at(&dir), Marker::Rejected(_)));
        std::fs::write(dir.join(MARKER), b"").unwrap();
        assert_eq!(check_marker_at(&dir), Marker::Accepted);
        std::fs::remove_file(dir.join(MARKER)).unwrap();
        std::fs::create_dir(dir.join(MARKER)).unwrap();
        assert!(matches!(check_marker_at(&dir), Marker::Rejected(_)));
        std::fs::remove_dir(dir.join(MARKER)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let real = t.path().join("real");
            std::fs::write(&real, b"").unwrap();
            symlink(&real, dir.join(MARKER)).unwrap();
            assert!(matches!(check_marker_at(&dir), Marker::Rejected(_)));
            std::fs::remove_file(dir.join(MARKER)).unwrap();
            // Hard link to a file elsewhere.
            std::fs::hard_link(&real, dir.join(MARKER)).unwrap();
            assert!(matches!(check_marker_at(&dir), Marker::Rejected(_)));
            std::fs::remove_file(dir.join(MARKER)).unwrap();
            // A folder others can write to.
            std::fs::write(dir.join(MARKER), b"").unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert!(matches!(check_marker_at(&dir), Marker::Rejected(_)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn mac_layout_and_private_dedicated_parent() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let t = tempfile::tempdir().unwrap();
        let parent = t.path().join("Metro");
        let exe_dir = parent.join("Metro.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (p, app) = mac_install_parent(&exe_dir).unwrap();
        assert_eq!(p, parent);
        assert_eq!(app, "Metro.app");
        assert_eq!(check_mac(&p, &app, uid), Marker::Absent);
        std::fs::write(parent.join(MARKER), b"").unwrap();
        assert_eq!(check_mac(&p, &app, uid), Marker::Accepted);
        // Not inside the bundle, not under another bundle, right shape only.
        assert!(mac_install_parent(&parent.join("Metro.app/Contents")).is_err());
        assert!(
            mac_install_parent(&t.path().join("x/Outer.app/In/Metro.app/Contents/MacOS")).is_err()
        );
        assert!(mac_install_parent(&t.path().join("x/Metro/Contents/MacOS")).is_err());
        // Something else in the folder: not dedicated.
        std::fs::write(parent.join("other.txt"), b"").unwrap();
        assert!(matches!(check_mac(&p, &app, uid), Marker::Rejected(_)));
        std::fs::remove_file(parent.join("other.txt")).unwrap();
        std::fs::write(parent.join(".DS_Store"), b"").unwrap();
        assert_eq!(check_mac(&p, &app, uid), Marker::Accepted);
        // Not private, or not ours.
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(check_mac(&p, &app, uid), Marker::Rejected(_)));
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            check_mac(&p, &app, uid.wrapping_add(1)),
            Marker::Rejected(_)
        ));
        // Marker not empty.
        std::fs::write(parent.join(MARKER), b"1").unwrap();
        assert!(matches!(check_mac(&p, &app, uid), Marker::Rejected(_)));
    }

    #[test]
    fn identity_dirs_differ_by_server_and_account() {
        let a = identity_dir("http://127.0.0.1:1", "local");
        assert_eq!(a.len(), 32);
        assert_ne!(a, identity_dir("http://127.0.0.1:2", "local"));
        assert_ne!(a, identity_dir("http://127.0.0.1:1", "other"));
    }

    #[test]
    fn record_round_trip_keeps_no_token_in_debug() {
        let r = Record {
            server_url: "http://127.0.0.1:1".into(),
            account_id: "local".into(),
            name: "n".into(),
            state: RecordState::Active,
            runner_id: Some("rn-1".into()),
            token: Some(Secret::new("metro_rt_v2_secret")),
        };
        let back = Record::from_json(&r.to_json()).unwrap();
        assert_eq!(back.token.unwrap().expose(), "metro_rt_v2_secret");
        assert!(!format!("{r:?}").contains("metro_rt_v2_secret"));
        // Active without a token is not a valid record.
        let mut bad = r.clone();
        bad.token = None;
        assert!(Record::from_json(&bad.to_json()).is_none());
    }
}
