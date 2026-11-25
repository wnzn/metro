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
//! 3. under the installer lock, extracts into `.stage.*`, checks `RELEASE.json`
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
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use metro_release::archive::Expect;
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

/// This build's version. In an e2e test build with the test release source
/// active, the version is the name of its `versions/<v>` folder, so one test
/// binary can play several releases.
pub fn own_version() -> Option<Version> {
    #[cfg(feature = "e2e-test-release")]
    if metro_release::e2e::active()
        && let Some(v) = std::env::current_exe()
            .and_then(|e| e.canonicalize())
            .ok()
            .and_then(|e| e.parent()?.file_name()?.to_str().and_then(Version::parse))
    {
        return Some(v);
    }
    Version::this_build()
}

/// This build's channel (an e2e test build with the test source: release).
pub fn channel() -> Channel {
    #[cfg(feature = "e2e-test-release")]
    if metro_release::e2e::active() {
        return Channel::Release;
    }
    Channel::current()
}

/// This process's install kind.
pub fn detect_current() -> InstallKind {
    let in_container = Path::new("/.dockerenv").exists()
        || Path::new("/run/.containerenv").exists()
        || std::env::var("METRO_RUNNER_PACKAGING").is_ok_and(|v| v == "container");
    let own = own_version();
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
#[serde(deny_unknown_fields)]
pub struct PendingFile {
    pub version: u32,
    pub from: String,
    pub to: String,
    pub attempts: u32,
    /// Durable intent: going back to `from` (set under the lock before the
    /// switch, so an interrupted rollback is finished at the next start).
    pub rolling_back: bool,
}

impl PendingFile {
    /// Stored state is untrusted: exact fields, real versions, an upgrade
    /// (`to` > `from`), a bounded counter.
    fn valid(&self) -> bool {
        self.version == 1
            && matches!((Version::parse(&self.from), Version::parse(&self.to)), (Some(f), Some(t)) if t > f)
            && self.attempts <= 1000
    }
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
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{prefix}{}.{}.{}",
        std::process::id(),
        now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
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
    })?;
    metro_release::sync_parent(path)
}

/// The pending file: `Ok(None)` when absent, `Err` when present but not a
/// well-formed pending update (then nothing acts on it or deletes it).
fn read_pending(root: &Path) -> Result<Option<PendingFile>, ()> {
    use std::io::Read;
    let path = root.join(PENDING);
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Ok(f)
            if f.metadata()
                .is_ok_and(|m| m.is_file() && m.mode() & 0o022 == 0) =>
        {
            f
        }
        _ => return Err(()),
    };
    let mut raw = Vec::new();
    file.take(4097).read_to_end(&mut raw).map_err(|_| ())?;
    if raw.len() > 4096 {
        return Err(());
    }
    let p: PendingFile = serde_json::from_slice(&raw).map_err(|_| ())?;
    if p.valid() { Ok(Some(p)) } else { Err(()) }
}

/// The version `current` names (`versions/<v>`), if it names one.
pub fn read_current(root: &Path) -> Option<Version> {
    let link = fs::read_link(root.join("current")).ok()?;
    Version::parse(link.strip_prefix("versions").ok()?.to_str()?)
}

/// Point `current` at `rel` atomically (`ln -s rel .current.new; rename`).
pub fn point_current(root: &Path, rel: &Path) -> io::Result<()> {
    let tmp = root.join(".current.new");
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(rel, &tmp)?;
    fs::rename(&tmp, root.join("current"))
}

// ---- the installer lock (flock on the installation directory) ----

pub struct Lock {
    path: PathBuf,
    dir: fs::File,
}

impl Lock {
    /// The shell installer locks the same inode. Never unlink or explicitly
    /// unlock: the last holder's close (including successful exec) releases it.
    pub fn take(root: &Path) -> Result<Lock, UpdateError> {
        let io = |e| io_err("io", "taking the install lock", e);
        let busy = || UpdateError::new("installer_busy", "another install is running");
        let dir = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(root)
            .map_err(io)?;
        let meta = dir.metadata().map_err(io)?;
        // SAFETY: geteuid has no preconditions.
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
            return Err(UpdateError::new(
                "install_changed",
                "the install directory is not owned and protected",
            ));
        }
        match dir.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(busy()),
            Err(std::fs::TryLockError::Error(e)) => return Err(io(e)),
        }
        match fs::symlink_metadata(root.join(LOCK)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(busy()), // an older installer owns this lock; never remove it
        }
        let lock = Self {
            path: root.to_path_buf(),
            dir,
        };
        if !lock.owns_path(root) {
            return Err(UpdateError::new(
                "install_changed",
                "the install directory changed",
            ));
        }
        Ok(lock)
    }

    fn owns_path(&self, root: &Path) -> bool {
        self.path == root
            && self
                .dir
                .metadata()
                .ok()
                .zip(fs::symlink_metadata(root).ok())
                .is_some_and(|(a, b)| b.is_dir() && a.dev() == b.dev() && a.ino() == b.ino())
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
    exe: Executable,
    authenticated_tree: Option<metro_release::archive::VerifiedTree>,
    // The strings and the NULL-terminated pointer arrays into them (heap
    // data, so moving the struct keeps the pointers valid).
    _argv: Vec<CString>,
    _envp: Vec<CString>,
    argv_ptrs: Vec<*const libc::c_char>,
    envp_ptrs: Vec<*const libc::c_char>,
    lock: Lock,
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
        process: (Vec<OsString>, Vec<(OsString, OsString)>),
        lock: Lock,
    ) -> io::Result<Switch> {
        let exe = open_version_exe(root, target)?;
        Self::with_executable(target, revert, pending, process, lock, exe)
    }

    fn with_executable(
        target: &str,
        revert: &str,
        pending: Option<&PendingFile>,
        (argv, envp): (Vec<OsString>, Vec<(OsString, OsString)>),
        lock: Lock,
        exe: Executable,
    ) -> io::Result<Switch> {
        let root = &lock.path;
        if Version::parse(target).is_none() || Version::parse(revert).is_none() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
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
            current_new: cpath(&root.join(unique(".current.")))?,
            exe,
            target_rel: cpath(&target_rel)?,
            revert_rel: cpath(&revert_rel)?,
            pending_tmp: cpath(&root.join(unique(".update-pending.")))?,
            pending: cpath(&root.join(PENDING))?,
            pending_bytes: pending.map(|p| serde_json::to_vec(p).expect("json")),
            authenticated_tree: None,
            _argv: argv,
            _envp: envp,
            argv_ptrs,
            envp_ptrs,
            lock,
        })
    }

    /// This process's own argv and environment.
    pub fn own_process() -> (Vec<OsString>, Vec<(OsString, OsString)>) {
        (std::env::args_os().collect(), std::env::vars_os().collect())
    }

    fn relink(&self, rel: &CString) -> bool {
        // SAFETY: plain libc calls on NUL-terminated strings we own.
        unsafe {
            if libc::symlink(rel.as_ptr(), self.current_new.as_ptr()) != 0 {
                return false;
            }
            if libc::rename(self.current_new.as_ptr(), self.current.as_ptr()) == 0 {
                return true;
            }
            libc::unlink(self.current_new.as_ptr());
            false
        }
    }

    fn still_current(&self, expected: &CString) -> bool {
        let mut buf = [0u8; 128];
        // SAFETY: calls write only to the supplied stack storage.
        unsafe {
            let mut held = std::mem::MaybeUninit::<libc::stat>::uninit();
            let mut current = std::mem::MaybeUninit::<libc::stat>::uninit();
            if libc::fstat(self.lock.dir.as_raw_fd(), held.as_mut_ptr()) != 0
                || libc::lstat(self.root.as_ptr(), current.as_mut_ptr()) != 0
            {
                return false;
            }
            let (held, current) = (held.assume_init(), current.assume_init());
            if held.st_dev != current.st_dev || held.st_ino != current.st_ino {
                return false;
            }
            let n = libc::readlink(self.current.as_ptr(), buf.as_mut_ptr().cast(), buf.len());
            n == expected.as_bytes().len() as isize && buf[..n as usize] == *expected.as_bytes()
        }
    }

    /// Write pending intent, durably move `current`, and execute the checked
    /// descriptor. If exec fails, the lock is still held during recovery.
    pub fn exec(self) -> io::Error {
        let error = self.execute();
        // Successful exec closed this descriptor via CLOEXEC. On error the
        // switch is over; unlock this exact file description even if an
        // unrelated concurrent fork briefly inherited a reference to it.
        let _ = self.lock.dir.unlock();
        error
    }

    fn execute(&self) -> io::Error {
        // Normal self-updates must still match the signed tree immediately
        // before switching. Prepared rollback and fixture switches use a
        // separately opened, previously installed version instead.
        if let Some(tree) = &self.authenticated_tree {
            let check = || {
                metro_release::archive::verify_tree(&tree.root, tree, &[]).map_err(from_release)?;
                let want = tree.files.get(BINARY).ok_or_else(|| {
                    UpdateError::new("not_verified", "the authenticated tree has no executable")
                })?;
                self.exe.verify(want)?;
                let path = tree.root.join(BINARY);
                let current = fs::symlink_metadata(path)
                    .map_err(|e| io_err("io", "rechecking the executable", e))?;
                let held = self
                    .exe
                    .file
                    .metadata()
                    .map_err(|e| io_err("io", "rechecking the executable", e))?;
                if !current.is_file() || (current.dev(), current.ino()) != (held.dev(), held.ino())
                {
                    return Err(UpdateError::new(
                        "not_verified",
                        "the staged executable was replaced",
                    ));
                }
                Ok(())
            };
            if let Err(e) = check() {
                return io::Error::new(io::ErrorKind::InvalidData, e.detail);
            }
        }
        // SAFETY: only libc calls on buffers prepared in `new`; nothing is
        // allocated here, so this is also safe in a forked child.
        unsafe {
            if !self.still_current(&self.revert_rel) {
                return io::ErrorKind::InvalidData.into();
            }
            let root_fd = self.lock.dir.as_raw_fd();
            let mut wrote = false;
            if let Some(bytes) = &self.pending_bytes {
                let mut existing = std::mem::MaybeUninit::<libc::stat>::uninit();
                if libc::lstat(self.pending.as_ptr(), existing.as_mut_ptr()) == 0
                    || io::Error::last_os_error().kind() != io::ErrorKind::NotFound
                {
                    return io::ErrorKind::InvalidData.into();
                }
                let fd = libc::open(
                    self.pending_tmp.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600,
                );
                if fd < 0 {
                    return io::Error::last_os_error();
                }
                let n = libc::write(fd, bytes.as_ptr().cast(), bytes.len());
                let synced = libc::fsync(fd) == 0;
                libc::close(fd);
                if n != bytes.len() as isize
                    || !synced
                    || libc::rename(self.pending_tmp.as_ptr(), self.pending.as_ptr()) != 0
                {
                    let e = io::Error::last_os_error();
                    libc::unlink(self.pending_tmp.as_ptr());
                    return e;
                }
                wrote = true;
                if libc::fsync(root_fd) != 0 {
                    return io::Error::last_os_error();
                }
            }
            if !self.relink(&self.target_rel) {
                let e = io::Error::last_os_error();
                return e;
            }
            let fd = self.exe.file.as_raw_fd();
            let mut ready = libc::fsync(root_fd) == 0;
            // A shebang interpreter must inherit the descriptor to read it.
            // Metro's actual binary is ELF and never inherits this descriptor.
            if ready && self.exe.script {
                ready = libc::fcntl(fd, libc::F_SETFD, 0) == 0;
            }
            if ready {
                libc::fexecve(fd, self.argv_ptrs.as_ptr(), self.envp_ptrs.as_ptr());
            }
            let e = io::Error::last_os_error();
            if self.still_current(&self.target_rel)
                && self.relink(&self.revert_rel)
                && libc::fsync(root_fd) == 0
                && wrote
            {
                libc::unlink(self.pending.as_ptr());
                libc::fsync(root_fd);
            }
            e
        }
    }
}

struct Executable {
    file: fs::File,
    script: bool,
}

impl Executable {
    fn open(path: &Path) -> io::Result<Self> {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.mode() & 0o022 != 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut header = [0; 2];
        let script = file.read_at(&mut header, 0)? == 2 && &header == b"#!";
        Ok(Self { file, script })
    }

    fn verify(&self, digest: &[u8; 32]) -> Result<(), UpdateError> {
        use std::io::{Seek, SeekFrom};
        let mut file = &self.file;
        file.seek(SeekFrom::Start(0))
            .map_err(|e| io_err("io", "reading the executable", e))?;
        let actual = metro_release::archive::HashingReader::new(file)
            .finish()
            .map_err(from_release)?;
        if &actual != digest {
            return Err(UpdateError::new("not_verified", "the executable changed"));
        }
        Ok(())
    }
}

fn open_version_exe(root: &Path, version: &str) -> io::Result<Executable> {
    if Version::parse(version).is_none() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let versions = root.join("versions");
    let dir = versions.join(version);
    for path in [&versions, &dir] {
        let meta = fs::symlink_metadata(path)?;
        if !meta.is_dir() || meta.mode() & 0o022 != 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
    Executable::open(&dir.join(BINARY))
}

// ---- start-up bookkeeping ----

/// What to do at start.
pub enum StartAction {
    Continue,
    /// Too many failed starts of a new version: go back to the old one.
    RollBack(Box<Switch>),
    /// A rollback is due but another install holds the lock: exit and let
    /// the service manager start this again (the intent is kept on disk).
    Busy,
}

/// Call first thing at start of a managed runner (before parsing config,
/// so a config crash in the new version still counts as a failed start).
/// Acts only on a well-formed pending file that matches this installation
/// (`current` names the running version), and changes the install only
/// under the installers' lock.
pub fn on_start(root: &Path, own: &Version) -> io::Result<StartAction> {
    if matches!(read_pending(root), Ok(None)) {
        return Ok(StartAction::Continue);
    }
    let lock = match Lock::take(root) {
        Ok(lock) => lock,
        Err(e) if e.code == "installer_busy" => return Ok(StartAction::Busy),
        Err(e) => return Err(io::Error::other(e.detail)),
    };
    let mut pending = match read_pending(root) {
        Ok(Some(p)) => p,
        Ok(None) => return Ok(StartAction::Continue),
        Err(()) => {
            eprintln!("metro-runner: warning: {PENDING} is malformed; left untouched");
            return Ok(StartAction::Continue);
        }
    };
    if !managed_current(root, own) {
        return Ok(StartAction::Continue);
    }
    let own = own.to_string();
    if pending.to == own {
        pending.attempts = pending.attempts.saturating_add(1).min(MAX_STARTS + 1);
        if pending.attempts <= MAX_STARTS && !pending.rolling_back {
            write_atomic(
                &root.join(PENDING),
                &serde_json::to_vec(&pending).expect("json"),
                0o600,
            )?;
            return Ok(StartAction::Continue);
        }
        let old = open_version_exe(root, &pending.from)?;
        pending.rolling_back = true;
        write_atomic(
            &root.join(PENDING),
            &serde_json::to_vec(&pending).expect("json"),
            0o600,
        )?;
        let failed = Report::failed(&pending.to, "start_failed");
        write_atomic(
            &root.join(FAILED),
            &serde_json::to_vec(&failed).expect("json"),
            0o600,
        )?;
        let switch = Switch::with_executable(
            &pending.from,
            &pending.to,
            None,
            Switch::own_process(),
            lock,
            old,
        )?;
        return Ok(StartAction::RollBack(Box::new(switch)));
    }
    if pending.from == own
        && (pending.rolling_back || pending.attempts == 0)
        && open_version_exe(root, &pending.to).is_ok()
    {
        // The previous version runs again (a failed exec or a finished
        // rollback): the pending update is over.
        let path = root.join(PENDING);
        fs::remove_file(&path)?;
        metro_release::sync_parent(&path)?;
    }
    // Anything else doesn't describe this installation: left alone.
    Ok(StartAction::Continue)
}

fn managed_current(root: &Path, own: &Version) -> bool {
    fs::symlink_metadata(root.join(MARKER)).is_ok_and(|m| m.is_file())
        && read_current(root) == Some(*own)
        && open_version_exe(root, &own.to_string()).is_ok()
}

/// After the first successful claim of a new version: confirm it and remove
/// every version except this one and the one it replaced, under the lock and
/// only while `current` still names this version. `false`: try again later.
pub fn confirm_started(root: &Path, own: &Version) -> bool {
    if !matches!(read_pending(root), Ok(Some(_))) {
        return true;
    }
    let Ok(_lock) = Lock::take(root) else {
        return false;
    };
    let pending = match read_pending(root) {
        Ok(Some(p)) if p.to == own.to_string() && !p.rolling_back && p.attempts <= MAX_STARTS => p,
        _ => return true,
    };
    if !managed_current(root, own) || open_version_exe(root, &pending.from).is_err() {
        return true;
    }
    let path = root.join(PENDING);
    if fs::remove_file(&path)
        .and_then(|()| metro_release::sync_parent(&path))
        .is_err()
    {
        return false;
    }
    let _ = fs::remove_file(root.join(FAILED));
    if let Ok(entries) = fs::read_dir(root.join("versions")) {
        for e in entries.flatten() {
            let name = e.file_name();
            if name.to_str().and_then(Version::parse).is_some()
                && name != *pending.to.as_str()
                && name != *pending.from.as_str()
            {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    eprintln!("metro-runner: update to {} confirmed", pending.to);
    true
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
    /// The installers' lock is taken first and handed to the switch: every
    /// change to the install, the download folder included, happens under it.
    pub fn prepare(&self, requested: &str) -> Result<Switch, UpdateError> {
        let target = Version::parse(requested)
            .ok_or_else(|| UpdateError::new("bad_version", "the requested version isn't X.Y.Z"))?;
        if target <= self.own {
            return Err(UpdateError::new(
                "not_newer",
                format!("{target} is not newer than {}", self.own),
            ));
        }
        let lock = Lock::take(&self.root)?;
        if !managed_current(&self.root, &self.own) {
            return Err(UpdateError::new(
                "install_changed",
                "the install changed; not updating",
            ));
        }
        if !matches!(read_pending(&self.root), Ok(None)) {
            return Err(UpdateError::new(
                "installer_busy",
                "an update journal is already present",
            ));
        }
        let release = self
            .fetcher
            .fetch_release(self.verifier.as_ref(), &target)
            .map_err(from_release)?;
        let name = release.runner_linux();
        // A fresh, private download folder for this update only.
        let dl = self.root.join(unique(".update-dl."));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dl)
            .map_err(|e| io_err("io", "download folder", e))?;
        let result = self.stage(&release, &name, &dl, &lock);
        if lock.owns_path(&self.root) {
            let _ = fs::remove_dir_all(&dl);
        }
        let (exe, pending, tree) = result?;
        let mut switch = Switch::with_executable(
            &pending.to,
            &pending.from,
            Some(&pending),
            Switch::own_process(),
            lock,
            exe,
        )
        .map_err(|e| io_err("io", "preparing the switch", e))?;
        switch.authenticated_tree = Some(tree);
        Ok(switch)
    }

    fn stage(
        &self,
        release: &metro_release::VerifiedRelease,
        name: &str,
        dl: &Path,
        lock: &Lock,
    ) -> Result<
        (
            Executable,
            PendingFile,
            metro_release::archive::VerifiedTree,
        ),
        UpdateError,
    > {
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

        let version = release.version.to_string();
        let stage = dl.join("stage");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&stage)
            .map_err(|e| io_err("io", "staging folder", e))?;
        let top = metro_release::manifest::archive_top(name)
            .unwrap_or_default()
            .to_string();
        let expect = Expect {
            kind: "runner",
            target: "linux-x64",
            version: &release.version,
        };
        // Extract from one open handle, proving the bytes read are the
        // verified ones (signed row and the archive's own bundle).
        let mut tree = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&part)
            .map_err(|e| io_err("io", "reading the download", e))
            .and_then(|f| {
                metro_release::archive::extract_tar_gz_verified(f, &digest, &top, &stage, &expect)
                    .map_err(from_release)
            })?;
        let versions = self.root.join("versions");
        let dest = versions.join(&version);
        let (verify_root, publish) = match fs::symlink_metadata(&dest) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => (stage.as_path(), true),
            Ok(m) if m.is_dir() && m.mode() & 0o022 == 0 => (dest.as_path(), false),
            Ok(_) => {
                return Err(UpdateError::new(
                    "install_changed",
                    "invalid target directory",
                ));
            }
            Err(e) => return Err(io_err("io", "opening the target version", e)),
        };
        let exe = Executable::open(&verify_root.join(BINARY))
            .map_err(|e| io_err("io", "opening the executable", e))?;
        let digest = tree
            .files
            .get(BINARY)
            .ok_or_else(|| UpdateError::new("not_verified", "missing runner executable"))?;
        metro_release::archive::verify_tree(verify_root, &tree, &[]).map_err(from_release)?;
        exe.verify(digest)?;
        check_start(&exe)?;
        metro_release::archive::verify_tree(verify_root, &tree, &[]).map_err(from_release)?;
        exe.verify(digest)?;
        if !lock.owns_path(&self.root)
            || !managed_current(&self.root, &self.own)
            || !matches!(read_pending(&self.root), Ok(None))
        {
            return Err(UpdateError::new(
                "install_changed",
                "the install changed; not updating",
            ));
        }
        if publish {
            fs::rename(&stage, &dest)
                .and_then(|()| metro_release::sync_parent(&dest))
                .map_err(|e| io_err("io", "moving the new version into place", e))?;
        }
        write_atomic(&self.root.join(SCRIPT), &script, 0o755)
            .map_err(|e| io_err("io", "replacing the install script", e))?;
        let pending = PendingFile {
            version: 1,
            from: self.own.to_string(),
            to: version.clone(),
            attempts: 0,
            rolling_back: false,
        };
        // The switch keeps the installers' lock until the exec itself.
        tree.root = dest;
        Ok((exe, pending, tree))
    }
}

/// Run the authenticated descriptor, not a path that can change after verification.
fn check_start(exe: &Executable) -> Result<(), UpdateError> {
    let fd = exe.file.as_raw_fd();
    let mut command = Command::new(format!("/proc/self/fd/{fd}"));
    command
        .arg("--check-start")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if exe.script {
        // SAFETY: only the async-signal-safe fcntl call runs in the child.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            })
        };
    }
    let mut child = command
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
            own: own_version(),
            enabled,
            updater,
            report,
            confirmed: false,
        }
    }

    /// Report only (no updater): unmanaged, container, tests.
    pub fn report_only(install: InstallKind) -> SelfUpdate {
        SelfUpdate::new(install, channel(), false, None)
    }

    /// The claim body.
    pub fn body(&self) -> String {
        let mut runner = serde_json::json!({
            "version": self.own.map_or_else(|| env!("CARGO_PKG_VERSION").to_string(), |v| v.to_string()),
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
            self.confirmed = match (&self.install, &self.own) {
                (InstallKind::Managed { root }, Some(own)) => confirm_started(root, own),
                _ => true,
            };
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
        // The server settles a request when it sees our failure report, so a
        // header after that is a new click by the runner's manager: retry.
        self.report = None;
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

    /// A managed root with versions 0.5.0 and 0.5.1 and `current` → `cur`.
    fn managed(cur: &str) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for v in ["0.5.0", "0.5.1"] {
            let version = dir.path().join("versions").join(v);
            fs::create_dir_all(&version).unwrap();
            for d in [&version, version.parent().unwrap()] {
                fs::set_permissions(d, fs::Permissions::from_mode(0o700)).unwrap();
            }
            let exe = version.join(BINARY);
            fs::write(&exe, b"").unwrap();
            fs::set_permissions(exe, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(dir.path().join(MARKER), b"").unwrap();
        point_current(dir.path(), &Path::new("versions").join(cur)).unwrap();
        dir
    }

    fn pending(root: &Path, attempts: u32) {
        let p = PendingFile {
            version: 1,
            from: "0.5.0".into(),
            to: "0.5.1".into(),
            attempts,
            rolling_back: false,
        };
        write_atomic(&root.join(PENDING), &serde_json::to_vec(&p).unwrap(), 0o600).unwrap();
    }

    #[test]
    fn start_counter_rolls_back_after_three() {
        let dir = managed("0.5.1");
        let root = dir.path();
        let new = Version::parse("0.5.1").unwrap();
        pending(root, 0);
        for n in 1..=MAX_STARTS {
            assert!(matches!(
                on_start(root, &new).unwrap(),
                StartAction::Continue
            ));
            assert_eq!(read_pending(root).unwrap().unwrap().attempts, n);
        }
        let StartAction::RollBack(switch) = on_start(root, &new).unwrap() else {
            panic!("rollback")
        };
        // The intent is durable before anything moves, and the lock is held.
        let p = read_pending(root).unwrap().unwrap();
        assert!(p.rolling_back);
        assert_eq!(Lock::take(root).err().unwrap().code, "installer_busy");
        assert_eq!(
            failed_report(root).unwrap(),
            Report::failed("0.5.1", "start_failed")
        );
        drop(switch);
        assert!(
            Lock::take(root).is_ok(),
            "an abandoned switch releases its own lock"
        );
        // An interrupted rollback is finished at the next start.
        assert!(matches!(
            on_start(root, &new).unwrap(),
            StartAction::RollBack(_)
        ));
        // The old version running again clears the pending file.
        point_current(root, Path::new("versions/0.5.0")).unwrap();
        on_start(root, &Version::parse("0.5.0").unwrap()).unwrap();
        assert!(read_pending(root).unwrap().is_none());
    }

    #[test]
    fn a_rollback_waits_for_another_installs_lock() {
        let dir = managed("0.5.1");
        let root = dir.path();
        let new = Version::parse("0.5.1").unwrap();
        pending(root, MAX_STARTS);
        let lock_shell = fs::File::open(root).unwrap();
        lock_shell.try_lock().unwrap();
        let before = fs::read(root.join(PENDING)).unwrap();
        assert!(matches!(on_start(root, &new).unwrap(), StartAction::Busy));
        assert_eq!(read_current(root), Some(new), "nothing switched");
        assert_eq!(
            fs::read(root.join(PENDING)).unwrap(),
            before,
            "no write outside lock"
        );
        drop(lock_shell);
        assert!(matches!(
            on_start(root, &new).unwrap(),
            StartAction::RollBack(_)
        ));
    }

    #[test]
    fn malformed_or_foreign_pending_files_are_left_alone() {
        let new = Version::parse("0.5.1").unwrap();
        for raw in [
            &b"not json"[..],
            br#"{"version":1,"from":"0.5.0","to":"0.5.1","attempts":1,"rolling_back":false,"extra":true}"#,
            br#"{"version":1,"from":"0.5.1","to":"0.5.0","attempts":9,"rolling_back":false}"#,
            br#"{"version":1,"from":"0.5.0","to":"v0.5.1","attempts":9,"rolling_back":false}"#,
            br#"{"version":1,"from":"0.5.0","to":"0.5.1","attempts":99999,"rolling_back":false}"#,
            br#"{"version":2,"from":"0.5.0","to":"0.5.1","attempts":3,"rolling_back":false}"#,
        ] {
            let dir = managed("0.5.1");
            let root = dir.path();
            fs::write(root.join(PENDING), raw).unwrap();
            assert!(read_pending(root).is_err());
            assert!(matches!(on_start(root, &new).unwrap(), StartAction::Continue));
            assert_eq!(fs::read(root.join(PENDING)).unwrap(), raw, "untouched");
            assert_eq!(read_current(root), Some(new));
        }
        // Well-formed but about another state of the install (`current` was
        // moved by a manual install): no action, nothing deleted.
        let dir = managed("0.5.0");
        let root = dir.path();
        pending(root, MAX_STARTS);
        assert!(matches!(
            on_start(root, &new).unwrap(),
            StartAction::Continue
        ));
        assert_eq!(read_pending(root).unwrap().unwrap().attempts, MAX_STARTS);
    }

    #[test]
    fn confirm_prunes_other_versions_under_the_lock() {
        let dir = managed("0.5.1");
        let root = dir.path();
        fs::create_dir_all(root.join("versions/0.4.0")).unwrap();
        pending(root, 1);
        assert!(confirm_started(root, &Version::parse("0.5.0").unwrap()));
        assert!(
            read_pending(root).unwrap().is_some(),
            "only the new version confirms"
        );
        fs::create_dir(root.join(LOCK)).unwrap();
        assert!(
            !confirm_started(root, &Version::parse("0.5.1").unwrap()),
            "busy: try later"
        );
        assert!(read_pending(root).unwrap().is_some());
        fs::remove_dir(root.join(LOCK)).unwrap();
        assert!(confirm_started(root, &Version::parse("0.5.1").unwrap()));
        assert!(read_pending(root).unwrap().is_none());
        let mut left: Vec<String> = fs::read_dir(root.join("versions"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["0.5.0", "0.5.1"]);
    }

    #[test]
    fn lock_rules() {
        let dir = managed("0.5.0");
        let root = dir.path();
        let held = Lock::take(root).unwrap();
        assert!(held.owns_path(root));
        assert_eq!(Lock::take(root).err().unwrap().code, "installer_busy");
        drop(held);
        assert!(!root.join(LOCK).exists());
        // A legacy installer lock is never taken over.
        fs::create_dir(root.join(LOCK)).unwrap();
        assert_eq!(Lock::take(root).err().unwrap().code, "installer_busy");
    }

    #[test]
    fn a_switch_never_releases_a_lock_it_no_longer_owns() {
        let parent = tempfile::tempdir().unwrap();
        let original = managed("0.5.0");
        let root = parent.path().join("install");
        fs::rename(original.path(), &root).unwrap();
        let switch = Switch::new(
            &root,
            "0.5.1",
            "0.5.0",
            None,
            (vec![], vec![]),
            Lock::take(&root).unwrap(),
        )
        .unwrap();
        let replacement = managed("0.5.0");
        fs::rename(&root, parent.path().join("earlier")).unwrap();
        fs::rename(replacement.path(), &root).unwrap();
        let other_lock = Lock::take(&root).unwrap();
        assert_eq!(switch.exec().kind(), io::ErrorKind::InvalidData);
        assert_eq!(Lock::take(&root).err().unwrap().code, "installer_busy");
        assert_eq!(read_current(&root), Version::parse("0.5.0"));
        drop(other_lock);
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
