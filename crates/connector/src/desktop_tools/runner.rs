//! This computer's personal registered runner, built into the desktop app:
//! no separate runner install, no terminal and no extra process.
//!
//! This is a real registered runner (`POST /v1/runners`, `visibility:
//! personal`), owned by the signed-in account and shown in its runner list
//! like any other. It is **not** the per-Space dedicated local tools of the
//! parent module and never stands in for them, or they for it.
//!
//! - **Set up explicitly.** Nothing is created until the person presses Set
//!   up in Settings › Runners (`POST /v1/local-tools/runner/setup`), for the
//!   server and account they're signed in to. One exception keeps the older
//!   installer choice working: an installer-made marker file ([`MARKER`],
//!   zero length, a regular file, not a link; see [`installer_marker`])
//!   sets it up without a press, and only on this app's own hosted server.
//! - **Any signed-in server.** It runs with the bridge's bound session
//!   ([`BoundSession`]): the desktop's own local server or a remote one the
//!   person signed in to (whose URL, and consent for plain HTTP, the bridge
//!   already checked). Each (server, account) has its own record and
//!   folders.
//! - **One runner, created once.** Setup creates it and keeps its token in
//!   one private file (mode 600, written atomically) tied to that server
//!   and account. Later starts resume it; nothing is created on each start.
//!   If the runner is deleted or its token rotated or revoked elsewhere, it
//!   is **not** recreated automatically: the state says so, and only an
//!   explicit reconnect (`POST /v1/local-tools/runner/reconnect`) issues a
//!   new token or runner. After the person deletes it in Settings, `forget`
//!   turns it off here (the record becomes `off`, the Space folders stay).
//!   An app update changes nothing here: the record lives in the desktop
//!   root, not in the install folder, and the runner updates with the app.
//! - **Runners are identified by id only.** A display name never identifies
//!   a runner: names can repeat, and anyone with the runner can rename or
//!   reuse one. Nothing is ever rotated, adopted or reported as attached
//!   because of its name.
//! - **Uncertain creation stops.** A `creating` record is written before the
//!   create call. If its answer is lost (or the app stops in between), the
//!   runner may or may not exist and its id is unknown, so the record
//!   becomes `interrupted`, keeping any id it did learn, and nothing is
//!   rotated or created on its own. The person recovers explicitly: reconnect
//!   with the id of a runner they choose from their own list, or without one
//!   to rotate the recorded id (if known and still theirs) or create a new
//!   runner.
//! - **No Space is attached by Metro.** The Space owner attaches it in the
//!   existing Space runner settings. Metro never attaches, detaches or
//!   replaces any Space's runner.
//! - **Confined to the Space's folder.** Claims with protocol 2, requires
//!   every job to name a valid Space, and serves [`TOOLS`] in that Space's
//!   folder: its own private `spaces/<sid>/`, or a folder the person chose
//!   for that Space ([`super::chosen`], kept in `folders.json`, checked
//!   again before every job). Files are opened handle-relative without
//!   following links ([`super::folder`]); `workspace.shell` runs one command
//!   there ([`super::shell`]). The server decides whether a command may run
//!   before it queues the job. Check and MCP are refused; no Docker, no
//!   inbound port.
//! - **Starts and stops with the app**: resumed when a session of that
//!   server and account is bound, stopped on sign-out, server or account
//!   switch, and app exit.
//!
//! ```text
//! <local-tools root>/runner/<sha256(server, account)[..16]>/
//!   runner.json     600: state, runner id + name, the runner token
//!   folders.json    600: the folder chosen per Space, if any
//!   spaces/<sid>/   the default folder of a job for <sid>
//! ```

use std::collections::BTreeMap;
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

use super::chosen::{self, Limits};
use super::folder::{self, Folder};
use super::hosted::{CallError, Claim, CreateError, Hosted, Job, RunnerInfo};
use super::shell;
use super::store::{read_capped, subdir, valid_space_id, write_atomic};
use super::{BoundSession, ToolsError, from_call};
use crate::Secret;

/// The installer's request (on the own hosted server only), a zero-length
/// regular file.
pub const MARKER: &str = "enable-local-runner";
const RUNNER_DIR: &str = "runner";
const SPACES: &str = "spaces";
const RECORD: &str = "runner.json";
const RECORD_TMP: &str = "runner.json.tmp";
const FOLDERS: &str = "folders.json";
const FOLDERS_TMP: &str = "folders.json.tmp";
const MAX_RECORD_BYTES: u64 = 16 * 1024;
const MAX_FOLDERS_BYTES: u64 = 512 * 1024;
const MAX_FOLDERS: usize = 1000;
const IDLE_POLL: Duration = Duration::from_millis(750);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const RESULT_ATTEMPTS: u32 = 6;
const HEARTBEAT: Duration = Duration::from_secs(2);
pub const TOOLS: [&str; 4] = [
    "workspace.list",
    "workspace.read",
    "workspace.write",
    "workspace.shell",
];

pub const NOT_OFFERED: &str = "is not available here: this Space is served by the Metro desktop \
     app's personal runner, which lists, reads and writes files and runs shell commands in the \
     Space's folder on this computer, and never runs checks or MCP servers";

// ---------------------------------------------------------------------------
// Installer marker
// ---------------------------------------------------------------------------

/// What the installer asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    /// No marker: set up only by an explicit press.
    Absent,
    Accepted,
    /// A marker exists but isn't trustworthy; it is ignored.
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
    /// The person deleted it and turned it off here; set up again to use.
    Off,
}

impl RecordState {
    fn as_str(self) -> &'static str {
        match self {
            RecordState::Creating => "creating",
            RecordState::Active => "active",
            RecordState::Revoked => "revoked",
            RecordState::Deleted => "deleted",
            RecordState::Interrupted => "interrupted",
            RecordState::Off => "off",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "creating" => RecordState::Creating,
            "active" => RecordState::Active,
            "revoked" => RecordState::Revoked,
            "deleted" => RecordState::Deleted,
            "interrupted" => RecordState::Interrupted,
            "off" => RecordState::Off,
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

/// `<root>/runner/<identity>/`, created private when `create`. A folder an
/// earlier build made under the server's other spelling
/// ([`BoundSession::legacy_url`]) is renamed to it first, with its Space
/// folders: one server never gets a second runner.
fn identity_handle(root: &File, h: &BoundSession, create: bool) -> io::Result<File> {
    let r = subdir(root, RUNNER_DIR, create)?;
    let name = identity_dir(&h.server_url, &h.account_id);
    if let Some(old) = h.legacy_url() {
        let old = identity_dir(old, &h.account_id);
        let missing =
            |n: &str| matches!(subdir(&r, n, false), Err(e) if e.kind() == io::ErrorKind::NotFound);
        if missing(&name) && !missing(&old) {
            cap_primitives::fs::rename(&r, Path::new(&old), &r, Path::new(&name))?;
        }
    }
    subdir(&r, &name, create)
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
pub fn load(root: &File, h: &BoundSession) -> io::Result<Option<Record>> {
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
    let Some(mut rec) = rec.filter(|r| {
        r.account_id == h.account_id
            && (r.server_url == h.server_url || h.legacy_url() == Some(r.server_url.as_str()))
    }) else {
        return Ok(None);
    };
    // Written by an earlier build under the other spelling: now this one's.
    if rec.server_url != h.server_url {
        rec.server_url = h.server_url.clone();
        save(root, h, &rec)?;
    }
    Ok(Some(rec))
}

/// Write this identity's record atomically, mode 600.
pub fn save(root: &File, h: &BoundSession, rec: &Record) -> io::Result<()> {
    if rec.server_url != h.server_url || rec.account_id != h.account_id {
        return Err(io::Error::other("record for another identity"));
    }
    let dir = identity_handle(root, h, true)?;
    let mut data = rec.to_json();
    let r = write_atomic(&dir, RECORD, RECORD_TMP, &data);
    data.zeroize();
    r
}

fn remove_record(root: &File, h: &BoundSession) -> io::Result<()> {
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
pub fn space_folder(root: &File, h: &BoundSession, sid: &str) -> io::Result<File> {
    if !valid_space_id(sid) {
        return Err(io::Error::other("invalid Space id"));
    }
    let d = identity_handle(root, h, true)?;
    let s = subdir(&d, SPACES, true)?;
    subdir(&s, sid, true)
}

/// The folder chosen per Space for this (server, account). A file that is
/// there but can't be read is an error, never "no choice": a job must not
/// run in the default folder when another one was chosen.
pub fn load_folders(root: &File, h: &BoundSession) -> io::Result<BTreeMap<String, PathBuf>> {
    let dir = match identity_handle(root, h, false) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e),
    };
    let bytes = match read_capped(&dir, FOLDERS, MAX_FOLDERS_BYTES)? {
        None => return Ok(BTreeMap::new()),
        Some(None) => return Err(io::Error::other("folder choices too large")),
        Some(Some(b)) => b,
    };
    let bad = || io::Error::other("folder choices unreadable");
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    if v.get("v").and_then(Value::as_u64) != Some(1) {
        return Err(bad());
    }
    let map = v
        .get("folders")
        .and_then(Value::as_object)
        .ok_or_else(bad)?;
    if map.len() > MAX_FOLDERS {
        return Err(bad());
    }
    map.iter()
        .map(|(sid, p)| match p.as_str().map(Path::new) {
            Some(p) if valid_space_id(sid) && p.is_absolute() => Ok((sid.clone(), p.to_owned())),
            _ => Err(bad()),
        })
        .collect()
}

pub fn save_folders(
    root: &File,
    h: &BoundSession,
    folders: &BTreeMap<String, PathBuf>,
) -> io::Result<()> {
    let map: Map<String, Value> = folders
        .iter()
        .map(|(k, v)| (k.clone(), json!(v.to_string_lossy())))
        .collect();
    let data = serde_json::to_vec(&json!({"v": 1, "folders": map})).expect("serializable");
    write_atomic(
        &identity_handle(root, h, true)?,
        FOLDERS,
        FOLDERS_TMP,
        &data,
    )
}

fn as_others(folders: &BTreeMap<String, PathBuf>) -> Vec<(String, PathBuf)> {
    folders
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// The folder a job for `sid` runs in, opened without following links, and
/// its path: the chosen one (checked again now) or the default
/// `spaces/<sid>/` (created if needed). Never the default instead of a
/// chosen folder that can't be used.
fn job_folder(spec: &Spec, sid: &str) -> Result<(File, PathBuf), String> {
    let folders = {
        let _g = spec.store_lock.lock();
        load_folders(&spec.root, &spec.session)
    }
    .map_err(|_| "Metro couldn't read which folder was chosen for this Space on this computer")?;
    if let Some(p) = folders.get(sid) {
        let limits = Limits {
            home: Limits::home_from_env(),
            data_root: &spec.data_root,
        };
        chosen::recheck(p, sid, &limits, &as_others(&folders)).map_err(|r| {
            format!(
                "the folder chosen for this Space on this computer can't be used: {} Choose \
                 another one, or go back to the default, in this computer's runner settings.",
                r.message
            )
        })?;
        let f = folder::open_dir_path(p)
            .map_err(|e| format!("the folder chosen for this Space on this computer: {e}"))?;
        return Ok((f, p.clone()));
    }
    let opened = {
        let _g = spec.store_lock.lock();
        space_folder(&spec.root, &spec.session, sid)
    };
    let f = opened.map_err(|_| {
        "this Space's folder on this computer is missing or is not a plain folder".to_string()
    })?;
    let path = spaces_path(
        &spec.root_path,
        &spec.session.server_url,
        &spec.session.account_id,
    )
    .join(sid);
    Ok((f, path))
}

// ---------------------------------------------------------------------------
// State shown to the person
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerState {
    /// Not set up for this server and account (or turned off).
    NotSetUp,
    /// No session is bound.
    Unavailable,
    /// Set up, but not started yet.
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

const REVOKED: &str = "This computer's runner is no longer accepted by the Metro server \
     (its token was rotated or revoked, or the runner was deleted). Metro doesn't recreate it on \
     its own; reconnect it to continue.";
const DELETED: &str = "This computer's runner was deleted from the Metro server. Metro \
     doesn't recreate it on its own; reconnect it to create a new one.";
const INTERRUPTED: &str = "Setting up this computer's runner was interrupted, so Metro can't \
     tell which runner (if any) was created. It won't guess or try again on its own. Reconnect to \
     finish: choose the runner from your runner list, or create a new one (then delete any \
     unused runner in Settings).";

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
    /// Metro's own data folder, which no chosen folder may overlap.
    pub data_root: &'a Path,
    pub session: &'a BoundSession,
    pub api: &'a Hosted,
    pub store_lock: &'a Arc<Mutex<()>>,
}

impl Ctx<'_> {
    fn load_folders(&self) -> Result<BTreeMap<String, PathBuf>, ToolsError> {
        let _g = self.store_lock.lock();
        load_folders(self.root, self.session)
            .map_err(|_| super::fs_error("read the folders chosen for this computer's runner"))
    }
    fn limits(&self) -> Limits<'_> {
        Limits {
            home: Limits::home_from_env(),
            data_root: self.data_root,
        }
    }
    fn load(&self) -> io::Result<Option<Record>> {
        let _g = self.store_lock.lock();
        load(self.root, self.session)
    }
    fn save(&self, r: &Record) -> io::Result<()> {
        let _g = self.store_lock.lock();
        save(self.root, self.session, r)
    }
    fn remove(&self) -> io::Result<()> {
        let _g = self.store_lock.lock();
        remove_record(self.root, self.session)
    }
}

/// The `local_runner` block of the status.
pub fn status(
    slot: &Slot,
    marker: &Marker,
    cx: Option<(&Path, &BoundSession, Option<Record>)>,
) -> Value {
    let (marker_s, marker_detail) = match marker {
        Marker::Absent => ("absent", None),
        Marker::Accepted => ("accepted", None),
        Marker::Rejected(m) => ("rejected", Some(m.clone())),
    };
    let base = |state: RunnerState, detail: Option<String>| {
        json!({
            "marker": marker_s,
            "marker_detail": marker_detail,
            "available": false,
            "state": state,
            "detail": detail,
            "runner": null,
            "spaces_folder": null,
            "tools": TOOLS,
            "can_set_up": false,
            "can_reconnect": false,
        })
    };
    let Some((root_path, h, rec)) = cx else {
        return base(RunnerState::Unavailable, Some(NO_SESSION.into()));
    };
    let live = slot.worker.as_ref().is_some_and(Handle::live);
    let off = rec.as_ref().is_none_or(|r| r.state == RecordState::Off);
    let n = match &slot.worker {
        Some(w) => w.note(),
        None => slot.last.clone().unwrap_or_else(|| {
            note(
                if off {
                    RunnerState::NotSetUp
                } else {
                    RunnerState::Idle
                },
                None,
            )
        }),
    };
    let mut v = base(n.state, n.detail);
    v["available"] = json!(true);
    v["spaces_folder"] =
        json!(spaces_path(root_path, &h.server_url, &h.account_id).to_string_lossy());
    if let Some(r) = rec.as_ref().filter(|r| r.state != RecordState::Off) {
        match &r.runner_id {
            Some(id) => v["runner"] = json!({"id": id, "name": r.name}),
            // The name an unconfirmed creation asked for: only a hint for
            // the person, never used to pick a runner.
            None if r.state != RecordState::Active => {
                v["unconfirmed_name"] = json!(r.name);
            }
            None => {}
        }
    }
    v["can_set_up"] = json!(!live && off);
    v["can_reconnect"] = json!(!live && !off);
    v
}

const NO_SESSION: &str = "Sign in to a Metro server to use this computer's runner.";

fn spawn(cx: &Ctx<'_>, runner_id: String, token: Secret) -> Handle {
    let (tx, rx) = watch::channel(false);
    let status = Arc::new(Mutex::new(note(RunnerState::Starting, None)));
    let spec = Spec {
        hosted: cx.api.clone(),
        root: cx.root.clone(),
        root_path: cx.root_path.to_path_buf(),
        data_root: cx.data_root.to_path_buf(),
        store_lock: cx.store_lock.clone(),
        session: cx.session.clone(),
        runner_id: runner_id.clone(),
        token,
    };
    let crash_status = status.clone();
    let task = tokio::spawn({
        let status = status.clone();
        super::worker::supervise(
            rx,
            super::worker::FIRST_RESTART,
            move |delay| {
                eprintln!(
                    "metro-desktop: this computer's runner stopped unexpectedly; restarting in {}s",
                    delay.as_secs()
                );
                set(
                    &crash_status,
                    RunnerState::Failed,
                    Some(format!(
                        "This computer's runner stopped unexpectedly. Metro restarts it in {} s.",
                        delay.as_secs().max(1)
                    )),
                );
            },
            move |rx| run(spec.clone(), rx, status.clone()),
        )
    });
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

/// The manager couldn't get as far as starting this computer's runner
/// (`reason` says why). Shown in its status until the next start attempt.
pub(super) fn start_failed(slot: &mut Slot, reason: &str) {
    if !slot.worker.as_ref().is_some_and(Handle::live) {
        slot.set(note(RunnerState::Failed, Some(reason)));
    }
}

/// The start failure `reason` no longer applies (nothing was started
/// since): back to not started, unless something else was recorded.
pub(super) fn clear_start_failed(slot: &mut Slot, reason: &str) {
    if slot.worker.is_none()
        && slot
            .last
            .as_ref()
            .is_some_and(|n| n.state == RunnerState::Failed && n.detail.as_deref() == Some(reason))
    {
        slot.last = None;
    }
}

fn fs_failed(slot: &mut Slot, what: &str) {
    slot.set(note(
        RunnerState::Failed,
        Some(&format!(
            "Metro couldn't {what} for this computer's runner in its private folder."
        )),
    ));
}

/// After a session of this account is bound: resume the runner once it is
/// set up (on the own hosted server with the installer's marker, create it
/// the first time). Never recreates one that was revoked or deleted.
pub async fn ensure(slot: &mut Slot, cx: &Ctx<'_>) {
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
        None if cx.marker.accepted() && cx.session.is_hosted() => provision(slot, cx).await,
        None => slot.set(note(RunnerState::NotSetUp, None)),
        Some(r) => match r.state {
            RecordState::Off => slot.set(note(RunnerState::NotSetUp, None)),
            RecordState::Creating => interrupt(slot, cx, r),
            RecordState::Active => start(slot, cx, r).await,
            RecordState::Revoked => slot.set(note(RunnerState::Revoked, Some(REVOKED))),
            RecordState::Deleted => slot.set(note(RunnerState::Deleted, Some(DELETED))),
            RecordState::Interrupted => slot.set(note(RunnerState::Interrupted, Some(INTERRUPTED))),
        },
    }
}

/// The person explicitly asks to reconnect a runner that is set up here
/// (Set up creates the first one). Idempotent while the runner runs.
/// Otherwise, by id only, never by name:
///
/// - `choice`: a runner id the person picked from their own runner list;
///   it gets a new token and becomes this computer's runner.
/// - no `choice`: the recorded runner id, if any and still theirs, gets a
///   new token; with no usable id (deleted, or a creation whose answer was
///   lost) a new runner is created.
pub(super) async fn reconnect(
    slot: &mut Slot,
    cx: &Ctx<'_>,
    choice: Option<String>,
) -> Result<(), ToolsError> {
    if slot.worker.as_ref().is_some_and(Handle::live) {
        return match (&choice, running_id(slot)) {
            (Some(c), Some(id)) if c != id => Err(ToolsError::new(
                409,
                "runner_running",
                "This computer's runner is already running.",
            )),
            _ => Ok(()),
        };
    }
    if let Some(w) = slot.worker.take() {
        w.stop().await;
    }
    let rec = cx
        .load()
        .map_err(|_| super::fs_error("read the runner's record"))?;
    if rec.as_ref().is_none_or(|r| r.state == RecordState::Off) {
        return Err(ToolsError::new(
            409,
            "not_set_up",
            "This computer's runner isn't set up here. Use Set up.",
        ));
    }
    if let Some(r) = rec.as_ref().filter(|r| r.state == RecordState::Active) {
        if choice
            .as_ref()
            .is_some_and(|c| Some(c) != r.runner_id.as_ref())
        {
            return Err(ToolsError::new(
                409,
                "runner_running",
                "This computer already has a working runner.",
            ));
        }
        start(slot, cx, r.clone()).await;
        return failed_as_error(slot);
    }
    let target = choice
        .clone()
        .or_else(|| rec.as_ref().and_then(|r| r.runner_id.clone()));
    let Some(target) = target else {
        provision(slot, cx).await;
        return failed_as_error(slot);
    };
    let list = cx
        .api
        .personal_runners(&cx.session.token)
        .await
        .map_err(from_call)?;
    // Ids are unique on the server; exactly one or none.
    match list.into_iter().find(|r| r.id == target) {
        Some(info) => {
            let base = rec.unwrap_or_else(|| Record {
                server_url: cx.session.server_url.clone(),
                account_id: cx.session.account_id.clone(),
                name: info.name.clone(),
                state: RecordState::Interrupted,
                runner_id: None,
                token: None,
            });
            rotate_into(slot, cx, &base, &info).await?;
            Ok(())
        }
        None if choice.is_some() => Err(ToolsError::new(
            404,
            "not_found",
            "That runner isn't one of your personal runners.",
        )),
        None => {
            provision(slot, cx).await;
            failed_as_error(slot)
        }
    }
}

/// The person pressed Set up: create this computer's runner for this
/// server and account. Only while it isn't set up (or was turned off);
/// otherwise Reconnect is the way back.
pub(super) async fn setup(slot: &mut Slot, cx: &Ctx<'_>) -> Result<(), ToolsError> {
    if slot.worker.as_ref().is_some_and(Handle::live) {
        return Ok(());
    }
    let rec = cx
        .load()
        .map_err(|_| super::fs_error("read the runner's record"))?;
    if rec.as_ref().is_some_and(|r| r.state != RecordState::Off) {
        return Err(ToolsError::new(
            409,
            "already_set_up",
            "This computer's runner is already set up here. Use Reconnect.",
        ));
    }
    if let Some(w) = slot.worker.take() {
        w.stop().await;
    }
    provision(slot, cx).await;
    failed_as_error(slot)
}

/// The person deleted this computer's runner (`runner_id`) in Settings:
/// stop it and turn it off here. Only for the recorded runner, and only
/// once the server no longer lists it. Its Space folders stay.
pub(super) async fn forget(
    slot: &mut Slot,
    cx: &Ctx<'_>,
    runner_id: &str,
) -> Result<(), ToolsError> {
    let rec = cx
        .load()
        .map_err(|_| super::fs_error("read the runner's record"))?;
    let Some(mut rec) = rec.filter(|r| r.runner_id.as_deref() == Some(runner_id)) else {
        return Err(ToolsError::new(
            409,
            "runner_changed",
            "That isn't this computer's runner any more.",
        ));
    };
    let list = cx
        .api
        .personal_runners(&cx.session.token)
        .await
        .map_err(from_call)?;
    if list.iter().any(|r| r.id == runner_id) {
        return Err(ToolsError::new(
            409,
            "runner_exists",
            "Delete this computer's runner first.",
        ));
    }
    slot.stop().await;
    rec.state = RecordState::Off;
    rec.runner_id = None;
    rec.token = None;
    cx.save(&rec)
        .map_err(|_| super::fs_error("save the runner's record"))?;
    slot.set(note(RunnerState::NotSetUp, None));
    Ok(())
}

fn failed_as_error(slot: &Slot) -> Result<(), ToolsError> {
    match slot.last.as_ref() {
        Some(n) if n.state == RunnerState::Failed && slot.worker.is_none() => Err(ToolsError::new(
            502,
            "runner_failed",
            n.detail.clone().unwrap_or_default(),
        )),
        _ => Ok(()),
    }
}

/// An unconfirmed creation: stop as interrupted, keep any id we know.
fn interrupt(slot: &mut Slot, cx: &Ctx<'_>, rec: Record) {
    let mut r = rec;
    r.state = RecordState::Interrupted;
    r.token = None;
    if cx.save(&r).is_err() {
        return fs_failed(slot, "save its record");
    }
    slot.set(note(RunnerState::Interrupted, Some(INTERRUPTED)));
}

async fn start(slot: &mut Slot, cx: &Ctx<'_>, rec: Record) {
    let (Some(id), Some(token)) = (rec.runner_id.clone(), rec.token.clone()) else {
        return fs_failed(slot, "read its record");
    };
    // Still ours on the server? If the server says it's gone, say so and
    // don't recreate. If it can't be asked now, let the worker find out.
    match cx.api.personal_runners(&cx.session.token).await {
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
                Some("The Metro server didn't accept this sign-in."),
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
        server_url: cx.session.server_url.clone(),
        account_id: cx.session.account_id.clone(),
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
        .create_personal_runner(&cx.session.token, &rec.name)
        .await
    {
        Ok((info, token)) => {
            rec.state = RecordState::Active;
            rec.runner_id = Some(info.id.clone());
            rec.token = Some(token.clone());
            if cx.save(&rec).is_err() {
                // Nobody could use it: take it back if we can. If not, try
                // to remember its id (without a token) so the person can
                // reconnect to exactly that runner.
                if cx
                    .api
                    .delete_runner(&cx.session.token, &info.id)
                    .await
                    .is_ok()
                {
                    let _ = cx.remove();
                } else {
                    rec.state = RecordState::Interrupted;
                    rec.token = None;
                    let _ = cx.save(&rec);
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
                    "The Metro server didn't create this computer's runner: {}",
                    error.message()
                )),
            ));
        }
        Err(CreateError {
            definite: false, ..
        }) => {
            // The answer was lost: the runner may or may not exist, and its
            // id is unknown. Don't guess.
            interrupt(slot, cx, rec);
        }
    }
}

/// Give `info` (our own runner) a new token, save it, start serving.
async fn rotate_into(
    slot: &mut Slot,
    cx: &Ctx<'_>,
    rec: &Record,
    info: &RunnerInfo,
) -> Result<(), ToolsError> {
    let token = match cx.api.rotate_runner(&cx.session.token, &info.id).await {
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

/// The runner is set up, the person is a member of `sid`, and whether this
/// runner (by id) is the one attached there.
async fn attached_to(cx: &Ctx<'_>, sid: &str) -> Result<bool, ToolsError> {
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
        .role(&cx.session.token, sid)
        .await
        .map_err(from_call)?
        .is_none()
    {
        return Err(ToolsError::new(404, "not_found", "no such Space"));
    }
    // By the stable runner id, never by display name.
    Ok(cx
        .api
        .space_runner_id(&cx.session.token, sid)
        .await
        .map_err(from_call)?
        .is_some_and(|id| Some(&id) == rec.runner_id.as_ref()))
}

/// The folder a Space's jobs use on this computer: `folder` (chosen or
/// default), `default_folder`, `custom`, and `folder_problem` when a chosen
/// folder can't be used now (its jobs then fail; nothing falls back).
fn folder_view(cx: &Ctx<'_>, sid: &str, attached: Option<bool>) -> Result<Value, ToolsError> {
    let default =
        spaces_path(cx.root_path, &cx.session.server_url, &cx.session.account_id).join(sid);
    let folders = cx.load_folders()?;
    let chosen = folders.get(sid);
    let problem = chosen.and_then(|p| {
        chosen::recheck(p, sid, &cx.limits(), &as_others(&folders))
            .err()
            .map(|r| r.message)
    });
    let mut v = json!({
        "space_id": sid,
        "folder": chosen.unwrap_or(&default).to_string_lossy(),
        "default_folder": default.to_string_lossy(),
        "custom": chosen.is_some(),
        "folder_problem": problem,
        "tools": TOOLS,
    });
    if let Some(a) = attached {
        v["attached"] = json!(a);
    }
    Ok(v)
}

/// `POST /v1/local-tools/runner/spaces/{sid}/folder`: the Space's folder on
/// this computer (the default one created if needed) and whether this
/// runner serves it. Members only; attaching stays in the Space's runner
/// settings.
pub(super) async fn folder_for(cx: &Ctx<'_>, sid: &str) -> Result<Value, ToolsError> {
    let attached = attached_to(cx, sid).await?;
    {
        let _g = cx.store_lock.lock();
        space_folder(cx.root, cx.session, sid)
            .map_err(|_| super::fs_error("create the Space's folder"))?;
    }
    folder_view(cx, sid, Some(attached))
}

/// `PUT /v1/local-tools/runner/spaces/{sid}/folder` `{"path"}`: use `path`
/// for this Space's jobs from the next job on, only while this runner is
/// attached there. Moves nothing.
pub(super) async fn set_folder(cx: &Ctx<'_>, sid: &str, raw: &str) -> Result<Value, ToolsError> {
    if !attached_to(cx, sid).await? {
        return Err(ToolsError::new(
            409,
            "not_attached",
            "This computer's runner isn't attached to this Space.",
        ));
    }
    {
        let _g = cx.store_lock.lock();
        let mut folders = load_folders(cx.root, cx.session)
            .map_err(|_| super::fs_error("read the folders chosen for this computer's runner"))?;
        let canon = chosen::check(raw, sid, &cx.limits(), &as_others(&folders))
            .map_err(|r| ToolsError::new(422, r.code, r.message))?;
        folders.insert(sid.to_owned(), canon);
        if folders.len() > MAX_FOLDERS {
            return Err(ToolsError::new(422, "too_many", "Too many chosen folders."));
        }
        save_folders(cx.root, cx.session, &folders)
            .map_err(|_| super::fs_error("save the folder chosen for this Space"))?;
    }
    folder_view(cx, sid, Some(true))
}

/// `DELETE /v1/local-tools/runner/spaces/{sid}/folder`: back to the default
/// folder from the next job on. Local only; moves nothing.
pub(super) fn clear_folder(cx: &Ctx<'_>, sid: &str) -> Result<Value, ToolsError> {
    if !valid_space_id(sid) {
        return Err(ToolsError::new(404, "not_found", "no such Space"));
    }
    {
        let _g = cx.store_lock.lock();
        let mut folders = load_folders(cx.root, cx.session)
            .map_err(|_| super::fs_error("read the folders chosen for this computer's runner"))?;
        if folders.remove(sid).is_some() {
            save_folders(cx.root, cx.session, &folders).map_err(|_| {
                super::fs_error("save the folders chosen for this computer's runner")
            })?;
        }
    }
    folder_view(cx, sid, None)
}

/// `GET /v1/local-tools/runner/folders`: every chosen folder, including
/// those of Spaces this runner is no longer attached to.
pub(super) fn list_folders(cx: &Ctx<'_>) -> Result<Value, ToolsError> {
    let folders = cx.load_folders()?;
    let rows: Vec<Value> = folders
        .iter()
        .map(|(sid, p)| json!({"space_id": sid, "folder": p.to_string_lossy()}))
        .collect();
    Ok(json!({ "folders": rows }))
}

// ---------------------------------------------------------------------------
// The worker
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Spec {
    hosted: Hosted,
    root: Arc<File>,
    root_path: PathBuf,
    data_root: PathBuf,
    store_lock: Arc<Mutex<()>>,
    session: BoundSession,
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
                let (ok, output) = execute(&spec, &job, rx.clone()).await;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}

/// Parse a job's arguments object into `T`.
fn args<T: for<'de> Deserialize<'de>>(v: &Value) -> Result<T, String> {
    match v.as_object() {
        Some(m) => serde_json::from_value(Value::Object(Map::clone(m)))
            .map_err(|e| format!("invalid arguments: {e}")),
        None => Err("arguments must be a JSON object".into()),
    }
}

enum Call {
    Files(String, Option<String>),
    Write(WriteArgs),
    Shell(shell::Request),
}

async fn execute(spec: &Spec, job: &Job, stop: watch::Receiver<bool>) -> (bool, String) {
    // Protocol 2: every job names its Space. No Space, or one that can't be
    // a folder name, touches nothing.
    let Some(sid) = job.space_id.clone().filter(|s| valid_space_id(s)) else {
        return (
            false,
            "refusing job: it doesn't name a valid Space for this runner".into(),
        );
    };
    let tool = job.tool.as_str();
    let call = match tool {
        "workspace.list" | "workspace.read" => {
            args::<PathArgs>(&job.arguments).map(|a| Call::Files(tool.to_owned(), a.path))
        }
        "workspace.write" => args::<WriteArgs>(&job.arguments).map(Call::Write),
        "workspace.shell" => shell::parse(&job.arguments).map(Call::Shell),
        _ => {
            return (
                false,
                format!("{} {NOT_OFFERED}", crate::secret::clean_line(tool, 64)),
            );
        }
    };
    let call = match call {
        Ok(c) => c,
        Err(e) => return (false, e),
    };
    // Opened fresh per job, without following links: this Space's folder.
    let s = spec.clone();
    let opened = tokio::task::spawn_blocking(move || job_folder(&s, &sid)).await;
    let (dir, path) = match opened {
        Ok(Ok(d)) => d,
        Ok(Err(m)) => return (false, m),
        Err(_) => return (false, "internal error in this computer's runner".into()),
    };
    let r = match call {
        Call::Shell(req) => shell::run(&req, &dir, &path, stop, job_ended(spec, &job.id)).await,
        Call::Files(tool, p) => {
            in_folder(dir, move |ws| super::worker::run_tool(ws, &tool, p)).await
        }
        Call::Write(a) => in_folder(dir, move |ws| write_tool(ws, &a)).await,
    };
    (
        r.0,
        super::worker::truncate_utf8(r.1, super::worker::MAX_OUTPUT_BYTES),
    )
}

/// Finishes once the server says the job is over, asked every
/// [`HEARTBEAT`].
async fn job_ended(spec: &Spec, id: &str) {
    loop {
        tokio::time::sleep(HEARTBEAT).await;
        if !spec.hosted.heartbeat(&spec.token, id).await {
            return;
        }
    }
}

async fn in_folder(
    dir: File,
    f: impl FnOnce(&Folder) -> (bool, String) + Send + 'static,
) -> (bool, String) {
    tokio::task::spawn_blocking(move || f(&Folder::from_handle(dir)))
        .await
        .unwrap_or_else(|_| (false, "internal error in this computer's runner".into()))
}

fn write_tool(ws: &Folder, a: &WriteArgs) -> (bool, String) {
    match ws.write(&a.path, a.content.as_bytes()) {
        Ok(w) => (true, format!("wrote {} bytes to {}", w.bytes, w.path)),
        Err(e) => (false, format!("workspace.write {:?}: {e}", a.path)),
    }
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
    fn one_server_spelled_differently_keeps_one_runner_record() {
        let t = tempfile::tempdir().unwrap();
        let root = crate::desktop_tools::store::open_root(&t.path().join("tools")).unwrap();
        let tok = Secret::new("bearer");
        let session = |url: &str| BoundSession::signed_in(url, &tok, "alice").unwrap();
        // An earlier build keyed it by the URL as typed.
        let old = "https://Metro.Example:443/";
        let earlier = BoundSession {
            server_url: old.trim_end_matches('/').into(),
            legacy_url: None,
            ..session(old)
        };
        let rec = Record {
            server_url: earlier.server_url.clone(),
            account_id: "alice".into(),
            name: "n".into(),
            state: RecordState::Active,
            runner_id: Some("rn-1".into()),
            token: Some(Secret::new("metro_rt_v2_secret")),
        };
        save(&root, &earlier, &rec).unwrap();
        std::fs::create_dir_all(
            spaces_path(
                t.path().join("tools").as_path(),
                &earlier.server_url,
                "alice",
            )
            .join("s-1"),
        )
        .unwrap();
        // Now every spelling is the same server: the old record is found,
        // moved under the one name with its folders, and rewritten.
        for url in [old, "https://metro.example", "HTTPS://METRO.EXAMPLE/"] {
            let h = session(url);
            assert_eq!(h.server_url(), "https://metro.example");
            let back = load(&root, &h).unwrap().expect("the earlier record");
            assert_eq!(back.runner_id.as_deref(), Some("rn-1"));
            assert_eq!(back.server_url, "https://metro.example");
        }
        let tools = t.path().join("tools");
        assert!(
            spaces_path(&tools, "https://metro.example", "alice")
                .join("s-1")
                .is_dir()
        );
        assert_eq!(
            std::fs::read_dir(tools.join(RUNNER_DIR)).unwrap().count(),
            1
        );
        // A path keeps its case; only the trailing slash goes.
        assert_eq!(
            session("http://10.0.0.2:8080/Metro/").server_url(),
            "http://10.0.0.2:8080/Metro"
        );
        assert_ne!(
            session("https://metro.example:8443").server_url(),
            "https://metro.example"
        );
    }

    fn spec(t: &Path) -> Spec {
        let tools = t.join("data").join("local-tools");
        Spec {
            hosted: Hosted::new("http://127.0.0.1:9").unwrap(),
            root: Arc::new(crate::desktop_tools::store::open_root(&tools).unwrap()),
            root_path: tools,
            data_root: t.join("data"),
            store_lock: Arc::new(Mutex::new(())),
            session: BoundSession::signed_in("http://127.0.0.1:9", &Secret::new("s"), "alice")
                .unwrap(),
            runner_id: "rn-1".into(),
            token: Secret::new("metro_rt_v2_x"),
        }
    }

    async fn call(spec: &Spec, tool: &str, args: Value) -> (bool, String) {
        let job = Job {
            id: "j1".into(),
            tool: tool.into(),
            arguments: args,
            space_id: Some("s-a".into()),
        };
        let (_tx, rx) = watch::channel(false);
        execute(spec, &job, rx).await
    }

    #[tokio::test]
    async fn jobs_use_the_chosen_folder_or_the_default_never_both() {
        let t = tempfile::tempdir().unwrap();
        let s = spec(t.path());
        let default = spaces_path(&s.root_path, "http://127.0.0.1:9", "alice").join("s-a");
        let (ok, out) = call(
            &s,
            "workspace.write",
            json!({"path": "notes/a.md", "content": "hello"}),
        )
        .await;
        assert!(ok, "{out}");
        assert_eq!(out, "wrote 5 bytes to notes/a.md");
        assert_eq!(std::fs::read(default.join("notes/a.md")).unwrap(), b"hello");
        let (ok, out) = call(&s, "workspace.read", json!({"path": "notes/a.md"})).await;
        assert_eq!((ok, out.as_str()), (true, "hello"));
        #[cfg(unix)]
        {
            let (ok, out) = call(&s, "workspace.shell", json!({"command": "cat notes/a.md"})).await;
            assert!(ok, "{out}");
            assert!(out.starts_with("exit: 0\n--- stdout ---\nhello\n"), "{out}");
        }

        // A chosen folder: the next job runs there; nothing was moved.
        let chosen = chosen::canonical(t.path()).unwrap().join("project");
        std::fs::create_dir(&chosen).unwrap();
        let folders = BTreeMap::from([("s-a".to_string(), chosen.clone())]);
        save_folders(&s.root, &s.session, &folders).unwrap();
        assert_eq!(load_folders(&s.root, &s.session).unwrap(), folders);
        let (ok, out) = call(
            &s,
            "workspace.write",
            json!({"path": "b.txt", "content": "b"}),
        )
        .await;
        assert!(ok, "{out}");
        assert!(chosen.join("b.txt").is_file());
        assert!(!default.join("b.txt").exists());
        assert!(default.join("notes/a.md").is_file());
        let (ok, out) = call(&s, "workspace.list", json!({})).await;
        assert!(
            ok && out.contains("b.txt") && !out.contains("notes"),
            "{out}"
        );
        #[cfg(unix)]
        {
            let (ok, out) = call(&s, "workspace.shell", json!({"command": "pwd"})).await;
            assert!(ok, "{out}");
            assert!(out.contains(&format!("\n{}\n", chosen.display())), "{out}");
        }

        // Gone: the job fails visibly; the default folder isn't used instead.
        std::fs::remove_dir_all(&chosen).unwrap();
        let (ok, out) = call(
            &s,
            "workspace.write",
            json!({"path": "c.txt", "content": "c"}),
        )
        .await;
        assert!(!ok);
        assert!(out.contains("folder chosen for this Space"), "{out}");
        assert!(!default.join("c.txt").exists());
        // Unreadable choices: also no fallback.
        let dir = identity_handle(&s.root, &s.session, false).unwrap();
        write_atomic(&dir, FOLDERS, FOLDERS_TMP, b"{not json").unwrap();
        assert!(load_folders(&s.root, &s.session).is_err());
        let (ok, out) = call(&s, "workspace.list", json!({})).await;
        assert!(!ok);
        assert!(out.contains("couldn't read which folder"), "{out}");
        // Cleared: the default again.
        save_folders(&s.root, &s.session, &BTreeMap::new()).unwrap();
        let (ok, out) = call(&s, "workspace.list", json!({"path": "notes"})).await;
        assert!(ok && out.contains("a.md"), "{out}");
    }

    #[tokio::test]
    async fn jobs_are_refused_without_a_space_tool_or_valid_arguments() {
        let t = tempfile::tempdir().unwrap();
        let s = spec(t.path());
        let (_tx, rx) = watch::channel(false);
        let job = Job {
            id: "j1".into(),
            tool: "workspace.write".into(),
            arguments: json!({"path": "x", "content": "x"}),
            space_id: None,
        };
        let (ok, out) = execute(&s, &job, rx).await;
        assert!(!ok && out.starts_with("refusing job"), "{out}");
        for (tool, args) in [
            ("workspace.check", json!({})),
            ("workspace.mcp_call", json!({})),
            ("workspace.write", json!({"path": "x"})),
            ("workspace.write", json!({"path": "x", "content": 1})),
            (
                "workspace.write",
                json!({"path": "x", "content": "", "mode": 7}),
            ),
            ("workspace.write", json!({"path": "../x", "content": "x"})),
            (
                "workspace.shell",
                json!({"command": "ls", "timeout_secs": 601}),
            ),
            ("workspace.shell", json!({"cmd": "ls"})),
            ("workspace.read", json!({"path": "x", "extra": true})),
        ] {
            let (ok, out) = call(&s, tool, args.clone()).await;
            assert!(!ok, "{tool} {args}: {out}");
        }
        let (_, out) = call(&s, "workspace.check", json!({})).await;
        assert!(out.contains("never runs checks or MCP servers"), "{out}");
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
