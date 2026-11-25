//! Runner version reporting and self-update.
//!
//! # Reporting
//!
//! Every claim carries `{"runner": {version, channel, install, self_update,
//! update}}` so Settings → Runners can show the version and whether an
//! update is available. It's display data: the server never trusts it.
//!
//! # Self-update (managed installs only)
//!
//! A runner installed by `install-metro-runner-linux.sh` (`<root>/versions/<v>`
//! and `current`) whose host opted in (`METRO_RUNNER_SELF_UPDATE=1`) and that is
//! a published release build updates itself when its manager asks in Metro.
//! The server only sends `X-Metro-Runner-Update: X.Y.Z`. The runner then:
//!
//! 1. fetches `SHA256SUMS` and its Sigstore bundle **for vX.Y.Z** from GitHub
//!    and verifies them ([`metro_release`]): pinned release workflow identity,
//!    GitHub claims, exactly that version, newer than itself;
//! 2. downloads its archive, checks the signed row and the archive's own
//!    bundle, and `install-metro-runner-linux.sh` against its row;
//! 3. under `<root>/.lock`, extracts into `.stage.*`, checks `RELEASE.json`
//!    (kind, target, version, every file) and moves it to `versions/X.Y.Z`;
//! 4. runs `versions/X.Y.Z/metro-runner --check-start` (config, workspace,
//!    docker preflight, no server contact); a failure removes it;
//! 5. replaces the installer script, writes `.update-pending.json`, points
//!    `current` at the new version (atomic rename) and `execve`s it, keeping
//!    the PID (systemd sees no restart) and the environment.
//!
//! The new binary counts its starts in the pending file. After more than
//! [`MAX_STARTS`] starts without a successful claim it points `current` back
//! and `execve`s the old binary, which then reports `failed`. The first
//! successful claim confirms the update and removes older versions.
//!
//! Docker runners never self-update; their manager sees the new pinned
//! image instead. Unmanaged binaries only report.

use std::ffi::{CString, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use metro_release::archive::{Expect, extract_tar_gz};
use metro_release::fetch::BlockingFetcher;
use metro_release::{Channel, ErrorCode, ReleaseVerifier, Version};
use serde::{Deserialize, Serialize};

pub const MARKER: &str = ".metro-runner-install";
pub const PENDING: &str = ".update-pending.json";
pub const FAILED: &str = ".update-failed.json";
pub const LOCK: &str = ".lock";
pub const SCRIPT: &str = "install-metro-runner-linux.sh";
pub const BINARY: &str = "metro-runner";
/// Starts of a new version without a successful claim before rollback.
pub const MAX_STARTS: u32 = 3;
const CHECK_START_TIMEOUT: Duration = Duration::from_secs(120);
const STALE_LOCK: Duration = Duration::from_secs(10 * 60);
const MAX_SCRIPT: usize = 1024 * 1024;

/// How this runner was installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// `<root>/versions/<own>/metro-runner`, the marker, `current → versions/<own>`.
    Managed { root: PathBuf },
    /// Inside a container (never self-updates).
    Container,
    /// Anything else (only reports).
    Unmanaged,
}

impl InstallKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallKind::Managed { .. } => "managed",
            InstallKind::Container => "container",
            InstallKind::Unmanaged => "unmanaged",
        }
    }
}

/// Classify an executable path (already canonical).
pub fn detect(exe: &Path, own: &Version, in_container: bool) -> InstallKind {
    if in_container {
        return InstallKind::Container;
    }
    let managed = || -> Option<PathBuf> {
        if exe.file_name()? != BINARY {
            return None;
        }
        let version_dir = exe.parent()?;
        if version_dir.file_name()?.to_str()? != own.to_string() {
            return None;
        }
        let versions = version_dir.parent()?;
        if versions.file_name()? != "versions" {
            return None;
        }
        let root = versions.parent()?;
        let marker = fs::symlink_metadata(root.join(MARKER)).ok()?;
        let current = fs::read_link(root.join("current")).ok()?;
        (marker.is_file() && current == Path::new("versions").join(own.to_string()))
            .then(|| root.to_path_buf())
    };
    match managed() {
        Some(root) => InstallKind::Managed { root },
        None => InstallKind::Unmanaged,
    }
}

/// This process's install kind.
pub fn detect_current() -> InstallKind {
    let in_container = Path::new("/.dockerenv").exists()
        || Path::new("/run/.containerenv").exists()
        || std::env::var("METRO_RUNNER_PACKAGING").is_ok_and(|v| v == "container");
    let own = Version::this_build();
    match (std::env::current_exe().and_then(|e| e.canonicalize()), own) {
        (Ok(exe), Some(own)) => detect(&exe, &own, in_container),
        _ if in_container => InstallKind::Container,
        _ => InstallKind::Unmanaged,
    }
}

/// An update outcome reported in claim bodies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub state: String,
    pub target: Option<String>,
    pub code: Option<String>,
}

impl Report {
    pub fn failed(target: &str, code: &str) -> Report {
        Report {
            state: "failed".into(),
            target: Some(target.into()),
            code: Some(code.into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingFile {
    pub from: String,
    pub to: String,
    pub attempts: u32,
}

/// A self-update failure: a stable code for the server, a detail for logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateError {
    pub code: &'static str,
    pub detail: String,
}

impl UpdateError {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        UpdateError {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

fn from_release(e: metro_release::Error) -> UpdateError {
    let code = match e.code {
        ErrorCode::NotVerified | ErrorCode::Manifest | ErrorCode::NotOffered => "verify_failed",
        ErrorCode::TrustRootStale => "trust_root_stale",
        ErrorCode::Inconsistent => "version_mismatch",
        ErrorCode::Archive => "archive_invalid",
        ErrorCode::Io => "io",
        ErrorCode::TooLarge => "too_large",
        ErrorCode::Offline
        | ErrorCode::RateLimited
        | ErrorCode::RedirectRefused
        | ErrorCode::Http => "download_failed",
    };
    let detail = if code == "trust_root_stale" {
        "verification data in this runner is too old; update it with the install script".to_string()
    } else {
        e.to_string()
    };
    UpdateError::new(code, detail)
}

fn io_err(code: &'static str, context: &str, e: io::Error) -> UpdateError {
    UpdateError::new(code, format!("{context}: {}", e.kind()))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn unique(prefix: &str) -> String {
    format!("{prefix}{}.{}", std::process::id(), now_ms())
}

/// Write a file atomically (temp, fsync, rename) with `mode`.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_file_name(unique(&format!(
        ".{}.",
        path.file_name().unwrap_or_default().to_string_lossy()
    )));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

fn read_pending(root: &Path) -> Option<PendingFile> {
    let raw = fs::read(root.join(PENDING)).ok()?;
    if raw.len() > 4096 {
        return None;
    }
    serde_json::from_slice(&raw).ok()
}

/// Point `current` at `rel` atomically (`ln -s rel .current.new; rename`).
pub fn point_current(root: &Path, rel: &Path) -> io::Result<()> {
    let tmp = root.join(".current.new");
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(rel, &tmp)?;
    fs::rename(&tmp, root.join("current"))
}

// ---- the installer lock (the scripts' `mkdir .lock` convention) ----

pub struct Lock {
    path: PathBuf,
}

impl Lock {
    /// Take `<root>/.lock`. A lock this runner left behind (dead pid, older
    /// than 10 minutes) is taken over; any other lock means another install
    /// is running.
    pub fn take(root: &Path) -> Result<Lock, UpdateError> {
        let path = root.join(LOCK);
        for _ in 0..2 {
            match fs::create_dir(&path) {
                Ok(()) => {
                    let owner = format!(
                        "{{\"pid\":{},\"started_at\":{},\"by\":\"metro-runner\"}}",
                        std::process::id(),
                        now_ms()
                    );
                    let _ = fs::write(path.join("owner"), owner);
                    return Ok(Lock { path });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if !Self::stale_own(&path) {
                        return Err(UpdateError::new(
                            "installer_busy",
                            "another install is running",
                        ));
                    }
                    let _ = fs::remove_dir_all(&path);
                }
                Err(e) => return Err(io_err("io", "taking the install lock", e)),
            }
        }
        Err(UpdateError::new(
            "installer_busy",
            "another install is running",
        ))
    }

    fn stale_own(path: &Path) -> bool {
        let Ok(raw) = fs::read(path.join("owner")) else {
            return false;
        };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            return false;
        };
        if v.get("by").and_then(|b| b.as_str()) != Some("metro-runner") {
            return false;
        }
        let pid = v.get("pid").and_then(|p| p.as_i64()).unwrap_or(0);
        let started = v.get("started_at").and_then(|s| s.as_u64()).unwrap_or(0) as u128;
        // SAFETY: kill with signal 0 only checks whether the pid exists.
        let alive =
            pid > 0 && pid <= i64::from(i32::MAX) && unsafe { libc::kill(pid as i32, 0) } == 0;
        !alive && now_ms().saturating_sub(started) > STALE_LOCK.as_millis()
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// ---- switch and exec (async-signal-safe: libc calls on prepared data) ----

/// Everything needed to move `current` and replace this process, prepared
/// up front so the final step allocates nothing.
pub struct Switch {
    root: CString,
    current: CString,
    current_new: CString,
    target_rel: CString,
    revert_rel: CString,
    pending_tmp: CString,
    pending: CString,
    pending_bytes: Option<Vec<u8>>,
    remove_pending: bool,
    exe: CString,
    // The strings and the NULL-terminated pointer arrays into them (heap
    // data, so moving the struct keeps the pointers valid).
    _argv: Vec<CString>,
    _envp: Vec<CString>,
    argv_ptrs: Vec<*const libc::c_char>,
    envp_ptrs: Vec<*const libc::c_char>,
}

fn cpath(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

impl Switch {
    /// `target` and `revert` are version strings (`versions/<v>`).
    pub fn new(
        root: &Path,
        target: &str,
        revert: &str,
        pending: Option<&PendingFile>,
        remove_pending: bool,
        argv: Vec<OsString>,
        envp: Vec<(OsString, OsString)>,
    ) -> io::Result<Switch> {
        let target_rel = Path::new("versions").join(target);
        let revert_rel = Path::new("versions").join(revert);
        let c =
            |s: Vec<u8>| CString::new(s).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput));
        let argv: Vec<CString> = argv
            .into_iter()
            .map(|a| c(a.as_bytes().to_vec()))
            .collect::<io::Result<_>>()?;
        let envp: Vec<CString> = envp
            .into_iter()
            .map(|(k, v)| {
                let mut kv = k.as_bytes().to_vec();
                kv.push(b'=');
                kv.extend_from_slice(v.as_bytes());
                c(kv)
            })
            .collect::<io::Result<_>>()?;
        let ptrs = |v: &[CString]| {
            v.iter()
                .map(|s| s.as_ptr())
                .chain([std::ptr::null()])
                .collect::<Vec<_>>()
        };
        let (argv_ptrs, envp_ptrs) = (ptrs(&argv), ptrs(&envp));
        Ok(Switch {
            root: cpath(root)?,
            current: cpath(&root.join("current"))?,
            current_new: cpath(&root.join(".current.new"))?,
            exe: cpath(&root.join(&target_rel).join(BINARY))?,
            target_rel: cpath(&target_rel)?,
            revert_rel: cpath(&revert_rel)?,
            pending_tmp: cpath(&root.join(".update-pending.json.new"))?,
            pending: cpath(&root.join(PENDING))?,
            pending_bytes: pending.map(|p| serde_json::to_vec(p).expect("json")),
            remove_pending,
            _argv: argv,
            _envp: envp,
            argv_ptrs,
            envp_ptrs,
        })
    }

    /// This process's own argv and environment.
    pub fn own_process() -> (Vec<OsString>, Vec<(OsString, OsString)>) {
        (std::env::args_os().collect(), std::env::vars_os().collect())
    }

    fn relink(&self, rel: &CString) -> bool {
        // SAFETY: plain libc calls on NUL-terminated strings we own.
        unsafe {
            libc::unlink(self.current_new.as_ptr());
            libc::symlink(rel.as_ptr(), self.current_new.as_ptr()) == 0
                && libc::rename(self.current_new.as_ptr(), self.current.as_ptr()) == 0
        }
    }

    fn sync_root(&self) {
        // SAFETY: open/fsync/close on our own path.
        unsafe {
            let fd = libc::open(
                self.root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            );
            if fd >= 0 {
                libc::fsync(fd);
                libc::close(fd);
            }
        }
    }

    /// Write the pending file, point `current` at the target and `execve` it.
    /// Returns only on failure, after pointing `current` back.
    pub fn exec(self) -> io::Error {
        // SAFETY: only libc calls on buffers prepared in `new`; nothing is
        // allocated here, so this is also safe in a forked child.
        unsafe {
            let mut wrote = false;
            if let Some(bytes) = &self.pending_bytes {
                let fd = libc::open(
                    self.pending_tmp.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_CLOEXEC,
                    0o600,
                );
                if fd < 0 {
                    return io::Error::last_os_error();
                }
                let n = libc::write(fd, bytes.as_ptr().cast(), bytes.len());
                libc::fsync(fd);
                libc::close(fd);
                if n != bytes.len() as isize
                    || libc::rename(self.pending_tmp.as_ptr(), self.pending.as_ptr()) != 0
                {
                    let e = io::Error::last_os_error();
                    libc::unlink(self.pending_tmp.as_ptr());
                    return e;
                }
                wrote = true;
            }
            if self.remove_pending {
                libc::unlink(self.pending.as_ptr());
            }
            if !self.relink(&self.target_rel) {
                let e = io::Error::last_os_error();
                if wrote {
                    libc::unlink(self.pending.as_ptr());
                }
                return e;
            }
            self.sync_root();
            libc::execve(
                self.exe.as_ptr(),
                self.argv_ptrs.as_ptr(),
                self.envp_ptrs.as_ptr(),
            );
            let e = io::Error::last_os_error();
            self.relink(&self.revert_rel);
            if wrote {
                libc::unlink(self.pending.as_ptr());
            }
            self.sync_root();
            e
        }
    }
}

// ---- start-up bookkeeping ----

/// What to do at start.
pub enum StartAction {
    Continue,
    /// Too many failed starts of a new version: go back to the old one.
    RollBack(Box<Switch>),
}

/// Call first thing at start of a managed runner (before parsing config,
/// so a config crash in the new version still counts as a failed start).
pub fn on_start(root: &Path, own: &Version) -> io::Result<StartAction> {
    let Some(mut pending) = read_pending(root) else {
        return Ok(StartAction::Continue);
    };
    let own = own.to_string();
    if pending.to == own {
        pending.attempts += 1;
        if pending.attempts > MAX_STARTS {
            let failed = Report::failed(&pending.to, "start_failed");
            write_atomic(
                &root.join(FAILED),
                &serde_json::to_vec(&failed).expect("json"),
                0o600,
            )?;
            let (argv, envp) = Switch::own_process();
            let switch = Switch::new(root, &pending.from, &pending.to, None, true, argv, envp)?;
            return Ok(StartAction::RollBack(Box::new(switch)));
        }
        write_atomic(
            &root.join(PENDING),
            &serde_json::to_vec(&pending).expect("json"),
            0o600,
        )?;
    } else {
        // We are the old version (the exec failed or we rolled back), or the
        // file is foreign: it no longer describes a running update.
        let _ = fs::remove_file(root.join(PENDING));
    }
    Ok(StartAction::Continue)
}

/// After the first successful claim of a new version: confirm it and remove
/// every version except this one and the one it replaced.
pub fn confirm_started(root: &Path, own: &Version) {
    let Some(pending) = read_pending(root) else {
        return;
    };
    if pending.to != own.to_string() {
        return;
    }
    let _ = fs::remove_file(root.join(PENDING));
    let _ = fs::remove_file(root.join(FAILED));
    if let Ok(entries) = fs::read_dir(root.join("versions")) {
        for e in entries.flatten() {
            let name = e.file_name();
            if name != *pending.to.as_str() && name != *pending.from.as_str() {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    eprintln!("metro-runner: update to {} confirmed", pending.to);
}

/// A failure the old version should report (written by a rollback).
pub fn failed_report(root: &Path) -> Option<Report> {
    let raw = fs::read(root.join(FAILED)).ok()?;
    if raw.len() > 4096 {
        return None;
    }
    serde_json::from_slice(&raw).ok()
}

// ---- the update itself ----

/// Fetches, verifies and stages a requested version.
pub struct Updater {
    pub root: PathBuf,
    pub own: Version,
    pub fetcher: BlockingFetcher,
    pub verifier: Box<dyn ReleaseVerifier>,
}

impl Updater {
    /// Steps 1–4 of the module docs; returns a [`Switch`] ready to exec.
    pub fn prepare(&self, requested: &str) -> Result<Switch, UpdateError> {
        let target = Version::parse(requested)
            .ok_or_else(|| UpdateError::new("bad_version", "the requested version isn't X.Y.Z"))?;
        if target <= self.own {
            return Err(UpdateError::new(
                "not_newer",
                format!("{target} is not newer than {}", self.own),
            ));
        }
        let release = self
            .fetcher
            .fetch_release(self.verifier.as_ref(), &target)
            .map_err(from_release)?;
        let name = release.runner_linux();
        let dl = self.root.join(".update-dl");
        let _ = fs::remove_dir_all(&dl);
        fs::create_dir_all(&dl).map_err(|e| io_err("io", "download folder", e))?;
        let result = self.stage(&release, &name, &dl);
        let _ = fs::remove_dir_all(&dl);
        result
    }

    fn stage(
        &self,
        release: &metro_release::VerifiedRelease,
        name: &str,
        dl: &Path,
    ) -> Result<Switch, UpdateError> {
        let url = |n: &str| {
            self.fetcher
                .source
                .asset_url(&release.version, n)
                .map_err(from_release)
        };
        let (digest, part) = self
            .fetcher
            .download(&url(name)?, &dl.join(name), metro_release::MAX_ARCHIVE)
            .map_err(from_release)?;
        let bundle = self
            .fetcher
            .get(
                &url(&format!("{name}.sigstore.json"))?,
                metro_release::MAX_BUNDLE,
            )
            .map_err(from_release)?;
        release
            .check_archive_signature(self.verifier.as_ref(), name, &digest, &bundle)
            .map_err(from_release)?;
        let script = self
            .fetcher
            .get(&url(SCRIPT)?, MAX_SCRIPT)
            .map_err(from_release)?;
        release.check_bytes(SCRIPT, &script).map_err(from_release)?;

        let lock = Lock::take(&self.root)?;
        let version = release.version.to_string();
        let stage = self.root.join(unique(".stage."));
        fs::create_dir(&stage).map_err(|e| io_err("io", "staging folder", e))?;
        let top = metro_release::manifest::archive_top(name)
            .unwrap_or_default()
            .to_string();
        let extracted = fs::File::open(&part)
            .map_err(|e| io_err("io", "reading the download", e))
            .and_then(|f| {
                extract_tar_gz(
                    io::BufReader::new(f),
                    &top,
                    &stage,
                    &Expect {
                        kind: "runner",
                        target: "linux-x64",
                        version: &release.version,
                    },
                )
                .map_err(from_release)
            });
        if let Err(e) = extracted {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
        let versions = self.root.join("versions");
        let dest = versions.join(&version);
        if fs::symlink_metadata(&dest).is_ok() {
            let old = self.root.join(unique(".old."));
            let _ = fs::rename(&dest, &old);
            let _ = fs::remove_dir_all(&old);
        }
        if let Err(e) = fs::create_dir_all(&versions).and_then(|_| fs::rename(&stage, &dest)) {
            let _ = fs::remove_dir_all(&stage);
            return Err(io_err("io", "moving the new version into place", e));
        }
        if let Err(e) = check_start(&dest.join(BINARY)) {
            let _ = fs::remove_dir_all(&dest);
            return Err(e);
        }
        write_atomic(&self.root.join(SCRIPT), &script, 0o755)
            .map_err(|e| io_err("io", "replacing the install script", e))?;
        let pending = PendingFile {
            from: self.own.to_string(),
            to: version.clone(),
            attempts: 0,
        };
        let (argv, envp) = Switch::own_process();
        let switch = Switch::new(
            &self.root,
            &version,
            &self.own.to_string(),
            Some(&pending),
            false,
            argv,
            envp,
        )
        .map_err(|e| io_err("io", "preparing the switch", e))?;
        drop(lock);
        Ok(switch)
    }
}

/// Run `<exe> --check-start` with this process's environment.
pub fn check_start(exe: &Path) -> Result<(), UpdateError> {
    let mut child = Command::new(exe)
        .arg("--check-start")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| io_err("start_check_failed", "starting the new version", e))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(UpdateError::new(
                    "start_check_failed",
                    format!("the new version's start check exited with {status}"),
                ));
            }
            Ok(None) if started.elapsed() > CHECK_START_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(UpdateError::new(
                    "start_check_failed",
                    "the new version's start check timed out",
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                return Err(io_err(
                    "start_check_failed",
                    "waiting for the start check",
                    e,
                ));
            }
        }
    }
}

// ---- what the claim loop holds ----

/// Presence reporting and the self-update hook for the claim loop.
pub struct SelfUpdate {
    pub install: InstallKind,
    pub channel: Channel,
    pub own: Option<Version>,
    /// Effective: managed + release + host opt-in.
    pub enabled: bool,
    pub updater: Option<Updater>,
    pub report: Option<Report>,
    confirmed: bool,
}

impl SelfUpdate {
    pub fn new(
        install: InstallKind,
        channel: Channel,
        host_opt_in: bool,
        updater: Option<Updater>,
    ) -> SelfUpdate {
        let enabled = host_opt_in
            && channel == Channel::Release
            && matches!(install, InstallKind::Managed { .. })
            && updater.is_some();
        let report = match &install {
            InstallKind::Managed { root } => failed_report(root),
            _ => None,
        };
        SelfUpdate {
            install,
            channel,
            own: Version::this_build(),
            enabled,
            updater,
            report,
            confirmed: false,
        }
    }

    /// Report only (no updater): unmanaged, container, tests.
    pub fn report_only(install: InstallKind) -> SelfUpdate {
        SelfUpdate::new(install, Channel::current(), false, None)
    }

    /// The claim body.
    pub fn body(&self) -> String {
        let mut runner = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "channel": self.channel.as_str(),
            "install": self.install.as_str(),
            "self_update": self.enabled,
        });
        if let Some(r) = &self.report {
            runner["update"] = serde_json::to_value(r).expect("json");
        }
        serde_json::json!({ "runner": runner }).to_string()
    }

    /// After a claim the server accepted (200/204).
    pub fn claim_succeeded(&mut self) {
        if !self.confirmed {
            self.confirmed = true;
            if let (InstallKind::Managed { root }, Some(own)) = (&self.install, &self.own) {
                confirm_started(root, own);
            }
        } else if let (Some(r), InstallKind::Managed { root }) = (&self.report, &self.install)
            && r.state == "failed"
        {
            // Reported once already; the server has it.
            let _ = fs::remove_file(root.join(FAILED));
            self.report = None;
        }
    }

    /// The server asked for `version`. On success this never returns.
    pub fn on_request(&mut self, version: &str) {
        let Some(updater) = self.updater.as_ref().filter(|_| self.enabled) else {
            return;
        };
        if Version::parse(version).is_some_and(|v| Some(v) <= self.own) {
            return;
        }
        if self
            .report
            .as_ref()
            .is_some_and(|r| r.target.as_deref() == Some(version))
        {
            // Already failed for this version; wait for a new request.
            return;
        }
        eprintln!("metro-runner: update to {version} requested; verifying it");
        match updater.prepare(version) {
            Ok(switch) => {
                eprintln!("metro-runner: {version} verified and passed its start check; switching");
                let e = switch.exec();
                eprintln!("metro-runner: could not start {version}: {e}; keeping this version");
                self.report = Some(Report::failed(version, "exec_failed"));
            }
            Err(e) => {
                eprintln!("metro-runner: update to {version} failed: {e}");
                self.report = Some(Report::failed(version, e.code));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let own = Version::parse("0.5.0").unwrap();
        let exe = root.join("versions/0.5.0/metro-runner");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"").unwrap();
        assert_eq!(
            detect(&exe, &own, false),
            InstallKind::Unmanaged,
            "no marker"
        );
        fs::write(root.join(MARKER), b"").unwrap();
        assert_eq!(
            detect(&exe, &own, false),
            InstallKind::Unmanaged,
            "no current"
        );
        std::os::unix::fs::symlink("versions/0.4.0", root.join("current")).unwrap();
        assert_eq!(
            detect(&exe, &own, false),
            InstallKind::Unmanaged,
            "current elsewhere"
        );
        point_current(root, Path::new("versions/0.5.0")).unwrap();
        assert_eq!(
            detect(&exe, &own, false),
            InstallKind::Managed {
                root: root.to_path_buf()
            }
        );
        assert_eq!(detect(&exe, &own, true), InstallKind::Container);
        assert_eq!(
            detect(&exe, &Version::parse("0.5.1").unwrap(), false),
            InstallKind::Unmanaged
        );
        assert_eq!(
            detect(&root.join("metro-runner"), &own, false),
            InstallKind::Unmanaged
        );
    }

    #[test]
    fn start_counter_rolls_back_after_three() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let new = Version::parse("0.5.1").unwrap();
        let p = PendingFile {
            from: "0.5.0".into(),
            to: "0.5.1".into(),
            attempts: 0,
        };
        write_atomic(&root.join(PENDING), &serde_json::to_vec(&p).unwrap(), 0o600).unwrap();
        for n in 1..=MAX_STARTS {
            assert!(matches!(
                on_start(root, &new).unwrap(),
                StartAction::Continue
            ));
            assert_eq!(read_pending(root).unwrap().attempts, n);
        }
        assert!(matches!(
            on_start(root, &new).unwrap(),
            StartAction::RollBack(_)
        ));
        assert_eq!(
            failed_report(root).unwrap(),
            Report::failed("0.5.1", "start_failed")
        );
        // The old version starting clears the pending file.
        on_start(root, &Version::parse("0.5.0").unwrap()).unwrap();
        assert!(read_pending(root).is_none());
    }

    #[test]
    fn confirm_prunes_other_versions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for v in ["0.4.0", "0.5.0", "0.5.1"] {
            fs::create_dir_all(root.join("versions").join(v)).unwrap();
        }
        let p = PendingFile {
            from: "0.5.0".into(),
            to: "0.5.1".into(),
            attempts: 1,
        };
        write_atomic(&root.join(PENDING), &serde_json::to_vec(&p).unwrap(), 0o600).unwrap();
        confirm_started(root, &Version::parse("0.5.0").unwrap());
        assert!(
            read_pending(root).is_some(),
            "only the new version confirms"
        );
        confirm_started(root, &Version::parse("0.5.1").unwrap());
        assert!(read_pending(root).is_none());
        let mut left: Vec<String> = fs::read_dir(root.join("versions"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["0.5.0", "0.5.1"]);
    }

    #[test]
    fn lock_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let held = Lock::take(root).unwrap();
        assert_eq!(Lock::take(root).err().unwrap().code, "installer_busy");
        drop(held);
        // A script's lock (no owner file) is never taken over.
        fs::create_dir(root.join(LOCK)).unwrap();
        assert_eq!(Lock::take(root).err().unwrap().code, "installer_busy");
        // Our own stale lock (dead pid, old) is taken over.
        fs::write(
            root.join(LOCK).join("owner"),
            r#"{"pid":2147483646,"started_at":1,"by":"metro-runner"}"#,
        )
        .unwrap();
        assert!(Lock::take(root).is_ok());
        // Our own recent lock with a live pid is respected.
        fs::create_dir(root.join(LOCK)).unwrap();
        let live = format!(
            r#"{{"pid":{},"started_at":1,"by":"metro-runner"}}"#,
            std::process::id()
        );
        fs::write(root.join(LOCK).join("owner"), live).unwrap();
        assert_eq!(Lock::take(root).err().unwrap().code, "installer_busy");
    }

    #[test]
    fn body_shape() {
        let s = SelfUpdate::new(InstallKind::Unmanaged, Channel::Release, true, None);
        let v: serde_json::Value = serde_json::from_str(&s.body()).unwrap();
        assert_eq!(v["runner"]["install"], "unmanaged");
        assert_eq!(
            v["runner"]["self_update"], false,
            "unmanaged never self-updates"
        );
        assert_eq!(v["runner"]["version"], env!("CARGO_PKG_VERSION"));
        assert!(s.body().len() < 2048);
    }
}
