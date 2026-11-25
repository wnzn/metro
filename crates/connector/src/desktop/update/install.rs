//! Install layouts, the update journal, staging, commit and rollback.
//!
//! Script installs (`install-metro-linux.sh`, `install-metro-windows.ps1`)
//! keep each version in `<root>/versions/<v>` with a pointer (`current`
//! symlink on Linux, `current.txt` on Windows). An update is staged next to
//! the running version, tried, and only then the pointer moves (one atomic
//! rename). The journal `<root>/.update.json` is written before every step,
//! so a crash at any point ends consistent at the next start.
//!
//! MSI installs keep their journal in `<desktop_root>/update/journal.json`;
//! the MSI's own transaction replaces the files.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use metro_release::archive::{self, Expect, HashingReader};
use metro_release::{Channel, ReleaseVerifier, VerifiedRelease, Version};
use serde::{Deserialize, Serialize};

use crate::desktop::store::InstanceLock;

use super::{LastUpdate, now_ms};

pub const MARKER: &str = ".metro-desktop-install";
pub const JOURNAL: &str = ".update.json";
pub const JOURNAL_UNCERTAIN: &str = "update_journal_unknown";

/// The update record could not be matched to this install. Unlike a bad
/// payload, it is not safe to recommend that the installer replace it.
#[derive(Debug)]
pub struct JournalUncertain;

impl std::fmt::Display for JournalUncertain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("update journal could not be verified against this installation")
    }
}

impl std::error::Error for JournalUncertain {}

pub(super) fn journal_uncertain() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, JournalUncertain)
}
pub const MSI_JOURNAL: &str = "journal.json";
pub const LOCK: &str = ".lock";
pub const LOCAL_RUNNER_MARKER: &str = "enable-local-runner";
#[cfg(windows)]
pub const LAUNCHER: &str = "Metro.exe";
#[cfg(not(windows))]
pub const LAUNCHER: &str = "Metro";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    LinuxInstaller { root: PathBuf },
    WindowsScript { root: PathBuf },
    WindowsMsi { dir: PathBuf },
    Portable,
    Unsupported(&'static str),
}

impl InstallKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallKind::LinuxInstaller { .. } => "linux-installer",
            InstallKind::WindowsScript { .. } => "windows-script",
            InstallKind::WindowsMsi { .. } => "windows-msi",
            InstallKind::Portable => "portable",
            InstallKind::Unsupported(_) => "unsupported",
        }
    }

    /// The script install root, if any.
    pub fn root(&self) -> Option<&Path> {
        match self {
            InstallKind::LinuxInstaller { root } | InstallKind::WindowsScript { root } => {
                Some(root)
            }
            _ => None,
        }
    }
}

/// Classify the running launcher's folder. In a trial start (`trial` is the
/// journal id), the pointer still names the previous version; the journal
/// vouches for this one.
pub fn detect(
    install_dir: &Path,
    own: &Version,
    channel: Channel,
    trial: Option<&str>,
) -> InstallKind {
    if channel != Channel::Release {
        return InstallKind::Unsupported(if channel == Channel::Preview {
            "preview_build"
        } else {
            "development_build"
        });
    }
    if cfg!(target_os = "macos") {
        return InstallKind::Unsupported("unsupported_platform");
    }
    let script_root = || -> Option<PathBuf> {
        if install_dir.file_name()?.to_str()? != own.to_string() {
            return None;
        }
        let versions = install_dir.parent()?;
        if versions.file_name()? != "versions" {
            return None;
        }
        let root = versions.parent()?;
        fs::symlink_metadata(root.join(MARKER))
            .ok()?
            .is_file()
            .then(|| root.to_path_buf())
    };
    if let Some(root) = script_root() {
        let pointer = read_pointer(&root);
        let trial_ok = trial.is_some_and(|id| {
            read_journal(&root.join(JOURNAL))
                .is_some_and(|j| j.id == id && j.to == own.to_string() && j.phase == "trial")
        });
        if pointer.as_deref() == Some(own.to_string().as_str()) || trial_ok {
            return if cfg!(windows) {
                InstallKind::WindowsScript { root }
            } else {
                InstallKind::LinuxInstaller { root }
            };
        }
        return InstallKind::Portable;
    }
    #[cfg(windows)]
    if let Some(dir) = msi::registered_dir(own)
        && fs::canonicalize(&dir).ok() == fs::canonicalize(install_dir).ok()
    {
        return InstallKind::WindowsMsi { dir };
    }
    InstallKind::Portable
}

/// The version the install pointer names (`current` → `versions/<v>`, or
/// `current.txt`).
pub fn read_pointer(root: &Path) -> Option<String> {
    if cfg!(windows) {
        let raw = read_evidence_file(&root.join("current.txt"), 128).ok()?;
        let raw = std::str::from_utf8(&raw).ok()?;
        let v = raw.trim().to_string();
        Version::parse(&v).map(|_| v)
    } else {
        let link = fs::read_link(root.join("current")).ok()?;
        let rest = link.strip_prefix("versions").ok()?;
        let v = rest.to_str()?.to_string();
        Version::parse(&v).map(|_| v)
    }
}

/// Point the install at `version` with one atomic rename.
pub fn set_pointer(root: &Path, version: &str) -> io::Result<()> {
    if Version::parse(version).is_none() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    if cfg!(windows) {
        write_atomic(&root.join("current.txt"), version.as_bytes())
    } else {
        #[cfg(unix)]
        {
            let tmp = root.join(format!(".current.{}", new_id()));
            std::os::unix::fs::symlink(Path::new("versions").join(version), &tmp)?;
            let path = root.join("current");
            fs::rename(&tmp, &path).inspect_err(|_| {
                let _ = fs::remove_file(&tmp);
            })?;
            metro_release::sync_parent(&path)?;
        }
        Ok(())
    }
}

/// Write a file atomically (temp, fsync, rename).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    metro_release::write_atomic(path, bytes)
}

/// Open the regular file itself, never a symlink, junction, pipe or device.
/// Windows also denies new conflicting writes and renames while held.
pub fn open_input(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x0020_0000); // FILE_SHARE_READ, OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
    Ok(file)
}

// ---- the journal ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhase {
    Staged,
    BackingUp,
    Trial,
    Installing,
    Committing,
    RollingBack,
    Committed,
    Failed,
}

impl JournalPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::BackingUp => "backing_up",
            Self::Trial => "trial",
            Self::Installing => "installing",
            Self::Committing => "committing",
            Self::RollingBack => "rolling_back",
            Self::Committed => "committed",
            Self::Failed => "failed",
        }
    }
}

impl PartialEq<&str> for JournalPhase {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub version: u32,
    pub id: String,
    pub from: String,
    pub to: String,
    pub phase: JournalPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub local_db_snapshot: bool,
    /// Digest of this attempt's published snapshot manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_manifest: Option<String>,
    /// The watchdog (old launcher) during a trial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watchdog_pid: Option<u32>,
    /// MSI only: the old launcher must exit before msiexec can replace it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pid: Option<u32>,
    /// An outcome kept for later (MSI snapshot) was already shown once.
    #[serde(default)]
    pub reported: bool,
    pub started_at: i64,
}

pub fn read_journal(path: &Path) -> Option<Journal> {
    load_journal(path).ok().flatten()
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

impl Journal {
    fn valid(&self, msi: bool) -> bool {
        let phase = self.phase.as_str();
        let outcome = match phase {
            "failed" => matches!(
                self.outcome.as_deref(),
                Some("rolled_back" | "not_confirmed" | "install_failed")
            ),
            "committed" => self.outcome.as_deref() == Some("updated"),
            "staged" | "backing_up" | "trial" | "committing" | "rolling_back" | "installing" => {
                self.outcome.is_none()
            }
            _ => false,
        };
        (self.version == 1 || (!msi && self.version == 2))
            && (if self.version == 1
                || matches!(self.phase, JournalPhase::Staged | JournalPhase::BackingUp)
            {
                self.backup_manifest.is_none()
            } else {
                self.backup_manifest
                    .as_deref()
                    .and_then(metro_release::parse_hex32)
                    .is_some()
            })
            && (phase != "backing_up"
                || (!msi
                    && self.version == 2
                    && !self.local_db_snapshot
                    && self.watchdog_pid.is_none()))
            && valid_id(&self.id)
            && matches!((Version::parse(&self.from), Version::parse(&self.to)), (Some(a), Some(b)) if a < b)
            && outcome
            && self.watchdog_pid != Some(0)
            && self.source_pid != Some(0)
            && (phase != "staged" || (!self.local_db_snapshot && self.watchdog_pid.is_none()))
            && (msi || (phase != "installing" && self.source_pid.is_none()))
            && (!msi || !matches!(phase, "committing" | "rolling_back"))
            && (!msi
                || !matches!(phase, "installing" | "trial")
                || (self.source_pid.is_some() && self.watchdog_pid.is_some()))
    }
}

pub fn load_journal(path: &Path) -> io::Result<Option<Journal>> {
    let f = match open_input(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut raw = Vec::new();
    f.take(64 * 1024 + 1).read_to_end(&mut raw)?;
    if raw.len() > 64 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let j: Journal = serde_json::from_slice(&raw).map_err(io::Error::other)?;
    if !j.valid(path.file_name() == Some(MSI_JOURNAL.as_ref())) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(Some(j))
}

pub fn write_journal(path: &Path, j: &Journal) -> io::Result<()> {
    if !j.valid(path.file_name() == Some(MSI_JOURNAL.as_ref())) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    write_atomic(path, &serde_json::to_vec(j).map_err(io::Error::other)?)
}

/// Where this install keeps its journal.
pub fn journal_path(kind: &InstallKind, update_dir: &Path) -> Option<PathBuf> {
    match kind {
        InstallKind::LinuxInstaller { root } | InstallKind::WindowsScript { root } => {
            Some(root.join(JOURNAL))
        }
        InstallKind::WindowsMsi { .. } => Some(update_dir.join(MSI_JOURNAL)),
        _ => None,
    }
}

pub(super) fn new_id() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom_fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn getrandom_fill(b: &mut [u8]) -> Result<(), ()> {
    use rand::RngCore;
    rand::rng().fill_bytes(b);
    Ok(())
}

/// A journal that ended (committed/failed) since the last start, as
/// `last_update`; it is removed. Cleans leftovers of finished updates.
pub fn take_finished(kind: &InstallKind, update_dir: &Path, own: &Version) -> Option<LastUpdate> {
    let own_s = own.to_string();
    let last = |j: &Journal, detail: Option<String>| LastUpdate {
        from: j.from.clone(),
        to: j.to.clone(),
        outcome: j.outcome.clone().unwrap_or_else(|| {
            if j.phase == "committed" {
                "updated".into()
            } else {
                "rolled_back".into()
            }
        }),
        at: now_ms(),
        detail: detail.or_else(|| j.detail.clone()),
        failure_code: None,
    };
    // Legacy MSI records and their unowned backup are never consumed here.
    // A failed record from the old helper could mean its wait call failed,
    // not that the machine-wide Windows Installer transaction ended.
    if let Some(root) = kind.root()
        && let Ok(_lock) = Lock::take(root)
        && script_state(root, &own_s)
    {
        let path = root.join(JOURNAL);
        if let Some(j) = read_journal(&path).filter(|j| finished_for(j, &own_s)) {
            retire_finished(&path, update_dir, &j).ok()?;
            return Some(last(&j, None));
        }
    }
    None
}

fn retire_finished(path: &Path, update_dir: &Path, j: &Journal) -> io::Result<()> {
    // Keep the terminal journal until all owned cleanup succeeds. A restart
    // can finish this without restoring an already restored DB.
    clean_backup(update_dir, j)?;
    remove_owned_dir(&stage_assets(update_dir, &j.id))?;
    fs::remove_file(path)?;
    metro_release::sync_parent(path)
}

fn finished_for(j: &Journal, own: &str) -> bool {
    (j.phase == "committed" && own == j.to) || (j.phase == "failed" && own == j.from)
}

/// Drop a staged update that is no longer offered (design 3.5: "otherwise
/// it is deleted"): its version folder (never the current one) and journal.
pub fn discard_staged(kind: &InstallKind, update_dir: &Path) {
    let Some(path) = journal_path(kind, update_dir) else {
        return;
    };
    let Ok(_lock) = Lock::take(lock_root(kind, update_dir)) else {
        return;
    };
    let Some(j) = read_journal(&path).filter(|j| j.phase == "staged") else {
        return;
    };
    match kind {
        InstallKind::LinuxInstaller { root } | InstallKind::WindowsScript { root }
            if script_state(root, &j.from) =>
        {
            let dest = root.join("versions").join(&j.to);
            if plain_dir(&dest) {
                let _ = fs::remove_dir_all(dest);
            }
            let _ = fs::remove_dir_all(stage_assets(update_dir, &j.id));
            let _ = fs::remove_file(&path);
        }
        _ => {}
    }
}

/// A trial whose watchdog is still alive.
pub fn trial_in_progress(root: &Path) -> bool {
    read_journal(&root.join(JOURNAL))
        .is_some_and(|j| j.phase == "trial" && j.watchdog_pid.is_some_and(pid_alive))
}

/// No new in-app MSI transactions are supported. A legacy helper's PID,
/// installed version, or `failed` record does not prove the installer has
/// stopped. Block startup for ambiguous records, preserving both copies.
pub fn gate_legacy_msi(update_dir: &Path) -> io::Result<()> {
    let path = update_dir.join(MSI_JOURNAL);
    if load_journal(&path)?
        .is_some_and(|j| !matches!(j.phase, JournalPhase::Staged | JournalPhase::Committed))
    {
        return Err(io::Error::other(
            "Windows Installer completion could not be established",
        ));
    }
    Ok(())
}

/// A staged update waiting for its restart.
pub fn staged(kind: &InstallKind, update_dir: &Path) -> Option<(Version, String)> {
    let j = read_journal(&journal_path(kind, update_dir)?)?;
    if j.phase != "staged" {
        return None;
    }
    let v = Version::parse(&j.to)?;
    let present = match kind {
        InstallKind::LinuxInstaller { root } | InstallKind::WindowsScript { root } => {
            script_state(root, &j.from) && version_present(root, &j.to)
        }
        InstallKind::WindowsMsi { .. } => {
            open_input(&stage_assets(update_dir, &j.id).join("package.msi")).is_ok()
        }
        _ => false,
    };
    present.then_some((v, j.id))
}

fn stage_assets(update_dir: &Path, id: &str) -> PathBuf {
    debug_assert!(valid_id(id));
    update_dir.join(format!("stage-{id}"))
}

fn package_name(kind: &InstallKind) -> &'static str {
    match kind {
        InstallKind::LinuxInstaller { .. } => "package.tar.gz",
        InstallKind::WindowsScript { .. } => "package.zip",
        _ => "package.msi",
    }
}

fn asset_name(kind: &InstallKind, release: &VerifiedRelease) -> Result<String, StageError> {
    match kind {
        InstallKind::LinuxInstaller { .. } => Ok(release.desktop_linux()),
        InstallKind::WindowsScript { .. } => Ok(release.desktop_windows_zip()),
        InstallKind::WindowsMsi { .. } => Ok(release.msi.clone()),
        _ => Err(stage_err("not_in_app")),
    }
}

fn expect<'a>(kind: &InstallKind, target: &'a Version) -> Expect<'a> {
    Expect {
        kind: "desktop",
        target: if matches!(kind, InstallKind::WindowsScript { .. }) {
            "windows-x64"
        } else {
            "linux-x64"
        },
        version: target,
    }
}

fn archive_err(e: metro_release::Error) -> StageError {
    let code = if e.code == metro_release::ErrorCode::NotVerified {
        "not_verified"
    } else {
        "archive"
    };
    stage_err(code)
}

fn read_evidence_file(path: &Path, cap: usize) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    open_input(path)?
        .take((cap + 1) as u64)
        .read_to_end(&mut data)?;
    if data.len() > cap {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(data)
}

/// Authenticated staged package/launcher. The caller keeps the file open
/// through use. In particular, Windows uses a deny-write/delete handle on
/// the MSI for the entire msiexec call and on the launcher through spawn.
pub struct CheckedFile {
    pub path: PathBuf,
    pub file: fs::File,
    #[cfg(windows)]
    _parents: Vec<fs::File>,
}

pub fn verify_staged(
    kind: &InstallKind,
    update_dir: &Path,
    j: &Journal,
    verifier: &dyn ReleaseVerifier,
    offer_manifest: Option<&[u8]>,
    lock: &Lock,
) -> io::Result<CheckedFile> {
    let no = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "staged update no longer verifies",
        )
    };
    lock.check(lock_root(kind, update_dir))?;
    let path = journal_path(kind, update_dir).ok_or_else(no)?;
    if load_journal(&path)
        .map_err(|_| journal_uncertain())?
        .as_ref()
        != Some(j)
    {
        return Err(journal_uncertain());
    }
    check_staged_current(kind, j)?;
    let version = Version::parse(&j.to).ok_or_else(no)?;
    let assets = stage_assets(update_dir, &j.id);
    if !plain_dir(&assets) {
        return Err(no());
    }
    let manifest = read_evidence_file(&assets.join("SHA256SUMS"), metro_release::MAX_MANIFEST)?;
    if offer_manifest.is_some_and(|m| m != manifest) {
        return Err(no());
    }
    let bundle = read_evidence_file(
        &assets.join("SHA256SUMS.sigstore.json"),
        metro_release::MAX_BUNDLE,
    )?;
    let release = metro_release::release::verify_manifest(verifier, &manifest, &bundle)
        .map_err(io::Error::other)?;
    if release.version != version {
        return Err(no());
    }
    let name = asset_name(kind, &release).map_err(|_| no())?;
    let digest = release.sha256_of(&name).map_err(io::Error::other)?;
    let package = assets.join(package_name(kind));
    #[cfg(windows)]
    let package_parents = archive::pin_windows_dirs(&assets)?;
    let mut input = open_input(&package)?;
    let Some(root) = kind.root() else {
        if !matches!(kind, InstallKind::WindowsMsi { .. })
            || !matches!(j.phase.as_str(), "staged" | "installing")
            || HashingReader::new(&input)
                .finish()
                .map_err(io::Error::other)?
                != digest
        {
            return Err(no());
        }
        return Ok(CheckedFile {
            path: package,
            file: input,
            #[cfg(windows)]
            _parents: package_parents,
        });
    };
    ensure_script_transition(root, j, &["staged"])?;
    let top = metro_release::manifest::archive_top(&name).ok_or_else(no)?;
    let expect = expect(kind, &version);
    let tree = if name.ends_with(".zip") {
        // The Windows input handle excludes in-place writes and deletion.
        use std::io::{Seek, SeekFrom};
        if HashingReader::new(&input)
            .finish()
            .map_err(io::Error::other)?
            != digest
        {
            return Err(no());
        }
        input.seek(SeekFrom::Start(0))?;
        archive::inspect_zip(&mut input, top, &expect).map_err(io::Error::other)?
    } else {
        let mut input = HashingReader::new(input);
        let tree = archive::inspect_tar_gz(&mut input, top, &expect).map_err(io::Error::other)?;
        if input.finish().map_err(io::Error::other)? != digest {
            return Err(no());
        }
        tree
    };
    let staged = root.join("versions").join(&j.to);
    let launcher = staged.join(LAUNCHER);
    #[cfg(windows)]
    let launcher_parents = archive::pin_windows_dirs(&staged)?;
    let file = open_input(&launcher)?;
    archive::verify_tree(&staged, &tree, &[LOCAL_RUNNER_MARKER]).map_err(io::Error::other)?;
    if tree.files.get(LAUNCHER)
        != Some(
            &HashingReader::new(&file)
                .finish()
                .map_err(io::Error::other)?,
        )
    {
        return Err(no());
    }
    Ok(CheckedFile {
        path: launcher,
        file,
        #[cfg(windows)]
        _parents: launcher_parents,
    })
}

pub(super) fn check_staged_current(kind: &InstallKind, j: &Journal) -> io::Result<()> {
    if let Some(root) = kind.root()
        && (j.phase != "staged" || !script_state(root, &j.from))
    {
        return Err(journal_uncertain());
    }
    Ok(())
}

// ---- the installers' lock ----

pub struct Lock {
    root: PathBuf,
    handle: fs::File,
    legacy: Option<metro_release::installer_lock::LegacyLock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageError {
    pub code: &'static str,
    pub message: String,
}

fn stage_err(code: &'static str) -> StageError {
    StageError {
        code,
        message: super::message_for(code).to_string(),
    }
}

impl Lock {
    /// The same kernel lock the manual installers use. Never remove another
    /// holder's lock, or use wall-clock age to guess that it is free.
    pub fn take(root: &Path) -> Result<Lock, StageError> {
        let mut lock = Self::take_kernel(root)?;
        lock.take_compat()?;
        Ok(lock)
    }

    fn take_kernel(root: &Path) -> Result<Lock, StageError> {
        if !plain_dir(root) {
            return Err(stage_err("io"));
        }
        #[cfg(unix)]
        let handle = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(root)
        };
        #[cfg(windows)]
        let handle = {
            use std::os::windows::fs::OpenOptionsExt;
            let parent = root.parent().ok_or_else(|| stage_err("io"))?;
            let name = root.file_name().ok_or_else(|| stage_err("io"))?;
            let path = parent.join(format!(".{}.install.lock", name.to_string_lossy()));
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .custom_flags(0x0020_0000)
                .share_mode(3)
                .open(path)
        };
        let handle = handle.map_err(|_| stage_err("io"))?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if !handle
                .metadata()
                .is_ok_and(|m| m.is_file() && m.file_attributes() & 0x400 == 0)
            {
                return Err(stage_err("io"));
            }
        }
        match handle.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Err(stage_err("installer_busy")),
            Err(fs::TryLockError::Error(_)) => return Err(stage_err("io")),
        }
        let lock = Self {
            root: root.to_path_buf(),
            handle,
            legacy: None,
        };
        lock.check(root).map_err(|_| stage_err("installer_busy"))?;
        Ok(lock)
    }

    fn take_compat(&mut self) -> Result<(), StageError> {
        self.legacy = Some(
            metro_release::installer_lock::LegacyLock::take(&self.root).map_err(|e| {
                stage_err(if e.kind() == io::ErrorKind::WouldBlock {
                    "installer_busy"
                } else {
                    "io"
                })
            })?,
        );
        Ok(())
    }

    pub fn check(&self, root: &Path) -> io::Result<()> {
        if self.root != root || !plain_dir(root) || self.legacy.as_ref().is_some_and(|l| !l.owned())
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held = self.handle.metadata()?;
            let named = fs::symlink_metadata(root)?;
            // SAFETY: geteuid has no preconditions.
            if (held.dev(), held.ino()) != (named.dev(), named.ino())
                || held.uid() != unsafe { libc::geteuid() }
                || held.mode() & 0o022 != 0
            {
                return Err(io::ErrorKind::InvalidData.into());
            }
        }
        #[cfg(windows)]
        self.handle.metadata()?;
        Ok(())
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Unlock this open file description only. A concurrently starting
        // process can briefly inherit the descriptor before its exec closes
        // it, and must not accidentally extend the watchdog's lock lifetime.
        drop(self.legacy.take());
        let _ = self.handle.unlock();
    }
}

fn plain_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if m.file_attributes() & 0x400 != 0 {
                return false;
            }
        }
        m.is_dir() && !m.file_type().is_symlink()
    })
}

pub fn lock_root<'a>(kind: &'a InstallKind, update_dir: &'a Path) -> &'a Path {
    kind.root().unwrap_or(update_dir)
}

pub fn script_state(root: &Path, version: &str) -> bool {
    fs::symlink_metadata(root.join(MARKER)).is_ok_and(|m| m.is_file())
        && read_pointer(root).as_deref() == Some(version)
        && version_present(root, version)
}

fn version_present(root: &Path, version: &str) -> bool {
    let versions = root.join("versions");
    let dir = versions.join(version);
    plain_dir(&versions) && plain_dir(&dir) && open_input(&dir.join(LAUNCHER)).is_ok()
}

pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks existence.
        pid != 0 && pid <= i32::MAX as u32 && unsafe { libc::kill(pid as i32, 0) } == 0
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        if pid == 0 {
            return false;
        }
        // SAFETY: a query-only handle, closed below.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        pid == std::process::id()
    }
}

// ---- stage ----

/// Extract (script installs) or keep (MSI) the verified download, and
/// journal `staged`. Returns the journal id.
pub fn stage(
    kind: &InstallKind,
    update_dir: &Path,
    install_dir: &Path,
    own: &Version,
    release: &VerifiedRelease,
    download: &Path,
    ldd: &Path,
) -> Result<String, StageError> {
    let target = &release.version;
    if target <= own {
        return Err(stage_err("not_latest"));
    }
    let path = journal_path(kind, update_dir).ok_or_else(|| stage_err("not_in_app"))?;
    let locked_root = lock_root(kind, update_dir);
    let lock = Lock::take(locked_root)?;
    if !matches!(kind, InstallKind::LinuxInstaller { root }
        if metro_release::installer_lock::migrated(root, MARKER))
    {
        return Err(stage_err("not_in_app"));
    }
    let from = own.to_string();
    let existing = load_journal(&path).map_err(|_| stage_err(JOURNAL_UNCERTAIN))?;
    if kind.root().is_some_and(|root| {
        !script_state(root, &from) || install_dir != root.join("versions").join(&from)
    }) {
        return Err(stage_err(if existing.is_some() {
            JOURNAL_UNCERTAIN
        } else {
            "installer_busy"
        }));
    }
    if let Some(j) = existing {
        if !finished_for(&j, &from) {
            return Err(stage_err("installer_busy"));
        }
        // Cleanup may not have run if the trial was still launching when
        // this instance started. Don't require another restart to stage.
        retire_finished(&path, update_dir, &j).map_err(|_| stage_err("io"))?;
    }
    let id = new_id();
    let journal = Journal {
        version: 1,
        id: id.clone(),
        from,
        to: target.to_string(),
        phase: JournalPhase::Staged,
        outcome: None,
        detail: None,
        local_db_snapshot: false,
        backup_manifest: None,
        watchdog_pid: None,
        source_pid: None,
        reported: false,
        started_at: now_ms(),
    };
    let assets = stage_assets(update_dir, &id);
    private_mkdir(&assets).map_err(|_| stage_err("io"))?;
    let result = (|| {
        let name = asset_name(kind, release)?;
        let digest = release.sha256_of(&name).map_err(archive_err)?;
        let package = assets.join(package_name(kind));
        let mut input = open_input(download).map_err(|_| stage_err("io"))?;
        let file =
            archive::private_copy_verified(&mut input, &digest, &package).map_err(archive_err)?;
        write_atomic(&assets.join("SHA256SUMS"), &release.evidence.manifest)
            .map_err(|_| stage_err("io"))?;
        write_atomic(
            &assets.join("SHA256SUMS.sigstore.json"),
            &release.evidence.bundle,
        )
        .map_err(|_| stage_err("io"))?;
        match kind {
            InstallKind::LinuxInstaller { root } | InstallKind::WindowsScript { root } => {
                let stage = root.join(format!(".stage.{id}"));
                private_mkdir(&stage).map_err(|_| stage_err("io"))?;
                let result = (|| {
                    let top = metro_release::manifest::archive_top(&name)
                        .ok_or_else(|| stage_err("archive"))?;
                    let expect = expect(kind, target);
                    let tree = if name.ends_with(".zip") {
                        archive::extract_zip(&mut { file }, top, &stage, &expect)
                            .map_err(archive_err)?
                    } else {
                        archive::extract_tar_gz_verified(file, &digest, top, &stage, &expect)
                            .map_err(archive_err)?
                    };
                    if matches!(kind, InstallKind::LinuxInstaller { .. }) {
                        let missing = missing_libraries(ldd, &stage);
                        if !missing.is_empty() {
                            return Err(StageError {
                                code: "missing_libraries",
                                message: format!(
                                    "This version needs system libraries that aren't installed: {}. Install GTK 4, WebKitGTK 6.0 and the GStreamer base plugins with your package manager (for example: sudo apt-get install --no-install-recommends libgtk-4-1 libwebkitgtk-6.0-4 gstreamer1.0-plugins-base), then try again.",
                                    missing.join(", ")
                                ),
                            });
                        }
                    }
                    // Keep this computer's local-runner choice.
                    if install_dir.join(LOCAL_RUNNER_MARKER).is_file() {
                        fs::write(stage.join(LOCAL_RUNNER_MARKER), b"")
                            .map_err(|_| stage_err("io"))?;
                    }
                    let dest = root.join("versions").join(target.to_string());
                    lock.check(root).map_err(|_| stage_err("installer_busy"))?;
                    if !script_state(root, &journal.from) {
                        return Err(stage_err("installer_busy"));
                    }
                    let moved = match fs::symlink_metadata(&dest) {
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {
                            fs::rename(&stage, &dest).map_err(|_| stage_err("io"))?;
                            true
                        }
                        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => {
                            archive::verify_tree(&dest, &tree, &[LOCAL_RUNNER_MARKER])
                                .map_err(archive_err)?;
                            false
                        }
                        _ => return Err(stage_err("installer_busy")),
                    };
                    let published = metro_release::sync_parent(&dest)
                        .and_then(|()| write_journal(&path, &journal));
                    if published.is_err() {
                        // If the journal became visible before a sync error,
                        // keep both pieces for retry rather than orphaning it.
                        if moved && matches!(load_journal(&path), Ok(None)) {
                            let _ = fs::remove_dir_all(&dest);
                        }
                        return Err(stage_err("io"));
                    }
                    Ok(())
                })();
                let _ = fs::remove_dir_all(&stage);
                result
            }
            InstallKind::WindowsMsi { .. } => {
                drop(file);
                lock.check(locked_root)
                    .map_err(|_| stage_err("installer_busy"))?;
                write_journal(&path, &journal).map_err(|_| stage_err("io"))
            }
            _ => Err(stage_err("not_in_app")),
        }
    })();
    if result.is_err() && matches!(load_journal(&path), Ok(None)) {
        let _ = fs::remove_dir_all(assets);
    }
    result.map(|()| id)
}

/// `ldd` on the launcher, server and UI; the sonames it can't find.
pub fn missing_libraries(ldd: &Path, app: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    for f in ["Metro", "metro-server", "ui/Metro"] {
        let Ok(out) = std::process::Command::new(ldd)
            .arg(app.join(f))
            .stdin(std::process::Stdio::null())
            .output()
        else {
            continue;
        };
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if line.contains("=> not found")
                && let Some(name) = line.split_whitespace().next()
                && name.len() < 128
                && !missing.iter().any(|m| m == name)
            {
                missing.push(name.to_string());
            }
        }
    }
    missing.sort();
    missing
}

pub fn mark_trial_ok(update_dir: &Path, id: &str) -> io::Result<()> {
    if !valid_id(id) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    write_atomic(&update_dir.join(format!("trial-{id}.ok")), b"ok")
}

pub fn trial_ok(update_dir: &Path, id: &str) -> bool {
    valid_id(id)
        && read_evidence_file(&update_dir.join(format!("trial-{id}.ok")), 2)
            .is_ok_and(|raw| raw == b"ok")
}

// ---- local database snapshot ----

/// The bundled server's database files (`local/db/metro.sqlite3*`).
fn db_files(desktop_root: &Path) -> Vec<PathBuf> {
    let db = desktop_root.join("local").join("db");
    ["metro.sqlite3", "metro.sqlite3-wal", "metro.sqlite3-shm"]
        .iter()
        .map(|n| db.join(n))
        .collect()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    id: String,
    from: String,
    to: String,
    sha256: [Option<[u8; 32]>; 3],
}

fn backup_paths(update_dir: &Path, j: &Journal) -> io::Result<(PathBuf, PathBuf)> {
    if j.version != 2 || !valid_id(&j.id) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok((
        update_dir.join(format!(".snapshot-{}.tmp", j.id)),
        update_dir.join(format!("db-snapshot-{}", j.id)),
    ))
}

pub fn begin_backup(root: &Path, j: &mut Journal, lock: &Lock) -> io::Result<()> {
    lock.check(root)?;
    ensure_script_transition(root, j, &["staged"])?;
    j.version = 2;
    j.phase = JournalPhase::BackingUp;
    write_journal(&root.join(JOURNAL), j)
}

/// Copy the stopped database into this journal's private attempt directory.
/// Even an absent database has a published record; absence is not an error.
pub fn snapshot_db(
    desktop_root: &Path,
    update_dir: &Path,
    j: &Journal,
) -> io::Result<(bool, String)> {
    if j.phase != JournalPhase::BackingUp {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let (tmp, snap) = backup_paths(update_dir, j)?;
    if !matches!(fs::symlink_metadata(&snap), Err(e) if e.kind() == io::ErrorKind::NotFound) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a previous DB snapshot is kept",
        ));
    }
    let files = db_files(desktop_root);
    let mut inputs = Vec::new();
    for path in &files {
        inputs.push(match open_input(path) {
            Ok(f) => Some(f),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        });
    }
    if inputs[0].is_none() && inputs.iter().any(Option::is_some) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "database sidecar without its main file",
        ));
    }
    private_mkdir(&tmp)?;
    let mut sha256 = [None; 3];
    for (index, (path, input)) in files.iter().zip(inputs).enumerate() {
        if let Some(input) = input {
            sha256[index] = Some(copy_synced(
                input,
                &tmp.join(path.file_name().unwrap_or_default()),
            )?);
        }
    }
    let manifest = Snapshot {
        id: j.id.clone(),
        from: j.from.clone(),
        to: j.to.clone(),
        sha256,
    };
    let encoded = serde_json::to_vec(&manifest).map_err(io::Error::other)?;
    let manifest_hash = metro_release::hex(&metro_release::sha256(&encoded));
    write_atomic(&tmp.join("snapshot.json"), &encoded)?;
    metro_release::rename_durable(&tmp, &snap)?;
    let published = read_snapshot(&snap, j, &manifest_hash)?;
    for (file, want) in files.iter().zip(published.sha256) {
        let path = snap.join(file.file_name().unwrap_or_default());
        match (open_input(&path), want) {
            (Ok(input), Some(digest)) => {
                if hash_copy(input, io::sink())? != digest {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "published backup does not verify",
                    ));
                }
            }
            (Err(e), None) if e.kind() == io::ErrorKind::NotFound => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "published backup does not verify",
                ));
            }
        }
    }
    Ok((published.sha256[0].is_some(), manifest_hash))
}

fn read_snapshot(snap: &Path, j: &Journal, expected: &str) -> io::Result<Snapshot> {
    if !plain_dir(snap) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let raw = read_evidence_file(&snap.join("snapshot.json"), 4096)?;
    if metro_release::hex(&metro_release::sha256(&raw)) != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the snapshot manifest changed",
        ));
    }
    let record: Snapshot = serde_json::from_slice(&raw).map_err(io::Error::other)?;
    if record.id != j.id
        || record.from != j.from
        || record.to != j.to
        || (record.sha256[0].is_none() && record.sha256.iter().any(Option::is_some))
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(record)
}

fn remove_owned_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) if plain_dir(path) => fs::remove_dir_all(path)?,
        Ok(_) => return Err(io::ErrorKind::InvalidData.into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    metro_release::sync_parent(path)
}

fn clean_backup(update_dir: &Path, j: &Journal) -> io::Result<()> {
    if j.version == 2 {
        let (tmp, published) = backup_paths(update_dir, j)?;
        remove_owned_dir(&tmp)?;
        remove_owned_dir(&published)?;
    }
    Ok(())
}

pub fn private_mkdir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    #[cfg(windows)]
    {
        fs::create_dir(path)?;
        if let Err(e) = crate::desktop::store::protect_private_dir(path) {
            let _ = fs::remove_dir(path);
            return Err(io::Error::other(e));
        }
    }
    Ok(())
}

fn copy_synced(input: fs::File, dest: &Path) -> io::Result<[u8; 32]> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(dest)?;
    let digest = hash_copy(input, &mut output)?;
    output.sync_all()?;
    Ok(digest)
}

fn hash_copy(mut input: impl Read, mut output: impl io::Write) -> io::Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            return Ok(hash.finalize().into());
        }
        hash.update(&buf[..n]);
        output.write_all(&buf[..n])?;
    }
}

/// Put the snapshot back byte for byte (removing a newer WAL/SHM).
fn restore_db(desktop_root: &Path, update_dir: &Path, j: &Journal) -> io::Result<()> {
    let (_, snap) = backup_paths(update_dir, j)?;
    let record = read_snapshot(
        &snap,
        j,
        j.backup_manifest
            .as_deref()
            .ok_or(io::ErrorKind::InvalidData)?,
    )?;
    if record.sha256[0].is_some() != j.local_db_snapshot {
        return Err(io::ErrorKind::InvalidData.into());
    }
    // Never delete the only database if the old install had no DB.
    if !j.local_db_snapshot {
        return Ok(());
    }
    let files = db_files(desktop_root);
    let dir = files[0].parent().ok_or(io::ErrorKind::InvalidData)?;
    if !plain_dir(dir) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let tmp = dir.join(format!(".restore-{}.tmp", j.id));
    remove_owned_dir(&tmp)?;
    private_mkdir(&tmp)?;
    let result = (|| {
        // Finish every copy before changing any live filename. The caller
        // must keep recovery pending and must not open the DB until all
        // renames/removals and the directory sync succeed.
        for (path, expected) in files.iter().zip(record.sha256) {
            if let Some(expected) = expected {
                let name = path.file_name().ok_or(io::ErrorKind::InvalidData)?;
                if copy_synced(open_input(&snap.join(name))?, &tmp.join(name))? != expected {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the backup changed",
                    ));
                }
            }
        }
        for (path, expected) in files.iter().zip(record.sha256).rev() {
            if expected.is_some() {
                metro_release::rename_durable(
                    &tmp.join(path.file_name().ok_or(io::ErrorKind::InvalidData)?),
                    path,
                )?;
            } else {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        metro_release::sync_parent(&files[0])
    })();
    result?;
    remove_owned_dir(&tmp)
}

// ---- commit and rollback (script installs) ----

/// Move the pointer (and the menu entry / shortcut) to `j.to`. Idempotent.
pub fn commit(root: &Path, j: &mut Journal, lock: &Lock) -> io::Result<()> {
    let path = root.join(JOURNAL);
    lock.check(root)?;
    ensure_script_transition(root, j, &["trial", "committing"])?;
    j.phase = JournalPhase::Committing;
    write_journal(&path, j)?;
    set_pointer(root, &j.to)?;
    move_launch_entry(root, &j.from);
    j.phase = JournalPhase::Committed;
    j.outcome = Some("updated".into());
    write_journal(&path, j)
}

/// Undo a staged or tried update: restore the DB snapshot, remove the new
/// version, keep the pointer on `from`, journal `failed`.
pub fn rollback(
    p: &super::apply::TrialPaths,
    j: &mut Journal,
    lock: &Lock,
    instance: &crate::desktop::store::InstanceLock,
    outcome: &str,
    detail: Option<String>,
) -> io::Result<()> {
    let super::apply::TrialPaths {
        root,
        desktop_root,
        update_dir,
    } = p;
    super::apply::RootGate::check(p, j, instance)?;
    let path = root.join(JOURNAL);
    lock.check(root)?;
    ensure_script_transition(root, j, &["trial", "committing", "rolling_back"])?;
    // The watchdog continuously holds the instance lock. An orphaned
    // bundled server may outlive the launcher in its own process group;
    // the inherited workspace lease, not the launcher's PID, excludes it.
    let _writer = crate::desktop_local::update_writer_exclusion(desktop_root)?;
    j.phase = JournalPhase::RollingBack;
    j.detail = detail;
    write_journal(&path, j)?;
    if j.version == 2 {
        restore_db(desktop_root, update_dir, j)?;
    } else if j.local_db_snapshot {
        return Err(io::Error::other("legacy backup needs manual recovery"));
    }
    // Move the pointer only after the DB is safe. Keep the old executable
    // and the snapshot until the terminal state is durable.
    if read_pointer(root).as_deref() != Some(j.from.as_str()) {
        set_pointer(root, &j.from)?;
    }
    j.phase = JournalPhase::Failed;
    j.outcome = Some(outcome.into());
    write_journal(&path, j)?;
    let _ = fs::remove_dir_all(root.join("versions").join(&j.to));
    Ok(())
}

pub fn ensure_script_transition(root: &Path, j: &Journal, phases: &[&str]) -> io::Result<()> {
    if !j.valid(false)
        || !phases.contains(&j.phase.as_str())
        || load_journal(&root.join(JOURNAL))?.as_ref() != Some(j)
        || !version_present(root, &j.from)
        || !version_present(root, &j.to)
        || !fs::symlink_metadata(root.join(MARKER)).is_ok_and(|m| m.is_file())
        || !read_pointer(root).is_some_and(|v| {
            v == j.from || (v == j.to && matches!(j.phase.as_str(), "committing" | "rolling_back"))
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "update journal does not match this installation",
        ));
    }
    Ok(())
}

/// What a normal (non-trial) start does with an unfinished journal.
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Nothing,
    /// Cannot safely open the database or install until recovery succeeds.
    Blocked,
    /// The journal is unreadable or disagrees with the current installation.
    JournalUncertain,
}

/// Inspect the journal and current pointer without taking any locks or
/// writing on disk. A successful observation never authorizes recovery.
pub fn inspect_recovery_journal(root: &Path) -> io::Result<Option<Journal>> {
    let Some(j) = load_journal(&root.join(JOURNAL)).map_err(|_| journal_uncertain())? else {
        return Ok(None);
    };
    let current_matches = match j.phase {
        JournalPhase::Committed => script_state(root, &j.to),
        JournalPhase::Committing | JournalPhase::RollingBack => {
            script_state(root, &j.from) || script_state(root, &j.to)
        }
        _ => script_state(root, &j.from),
    };
    if !current_matches {
        return Err(journal_uncertain());
    }
    Ok(Some(j))
}

pub fn recover(root: &Path, desktop_root: &Path, update_dir: &Path, own: &Version) -> Recovery {
    recover_inner(root, desktop_root, update_dir, own, None)
}

/// Recovery of a journal tied to a workspace gate. The expected journal
/// must still match and the gate is retired under both installation locks.
pub fn recover_with_gate(
    root: &Path,
    desktop_root: &Path,
    update_dir: &Path,
    own: &Version,
    expected: &Journal,
    instance: &InstanceLock,
) -> Recovery {
    recover_inner(
        root,
        desktop_root,
        update_dir,
        own,
        Some((expected, instance)),
    )
}

fn recover_inner(
    root: &Path,
    desktop_root: &Path,
    update_dir: &Path,
    own: &Version,
    guarded: Option<(&Journal, &InstanceLock)>,
) -> Recovery {
    use super::apply::RootGate;

    let observed = match inspect_recovery_journal(root) {
        Ok(Some(j)) if guarded.is_none_or(|(expected, _)| *expected == j) => j,
        Ok(None) if guarded.is_none() => return Recovery::Nothing,
        _ => return Recovery::JournalUncertain,
    };
    let gate_matches = |j: &Journal| {
        guarded.is_none_or(|(_, instance)| {
            instance.check(desktop_root).is_ok()
                && RootGate::read(desktop_root)
                    .is_ok_and(|gate| gate.is_some_and(|gate| gate.matches(root, &j.id)))
        })
    };
    if !gate_matches(&observed) {
        return Recovery::JournalUncertain;
    }
    let Ok(mut lock) = Lock::take_kernel(root) else {
        return Recovery::Blocked;
    };
    // Don't retire an existing compatibility lock if the journal changed.
    // Old installers only respect that lock, so inspect again after taking it.
    if !inspect_recovery_journal(root).is_ok_and(|j| j.as_ref() == Some(&observed))
        || !gate_matches(&observed)
    {
        return Recovery::JournalUncertain;
    }
    if lock.take_compat().is_err() {
        return Recovery::Blocked;
    }
    let mut j = match inspect_recovery_journal(root) {
        Ok(Some(j)) if j == observed => j,
        _ => return Recovery::JournalUncertain,
    };
    if !gate_matches(&j) {
        return Recovery::JournalUncertain;
    }
    let path = root.join(JOURNAL);
    let own = own.to_string();
    let result = match j.phase.as_str() {
        "backing_up" if own == j.from => {
            // No trial can be launched before Trial is published. A crash
            // on either side of snapshot publication leaves the live DB
            // authoritative, so retire only this attempt and retry staging.
            let reset = (|| {
                lock.check(root)?;
                ensure_script_transition(root, &j, &["backing_up"])?;
                clean_backup(update_dir, &j)?;
                j.phase = JournalPhase::Staged;
                write_journal(&path, &j)
            })();
            if reset.is_ok() {
                Recovery::Nothing
            } else {
                Recovery::Blocked
            }
        }
        "backing_up" => Recovery::Blocked,
        // Restart cannot establish uninterrupted ownership. In the gap
        // an old launcher could have opened a writer that never inherited
        // its lease. Only the original watchdog can safely restore.
        "committing" | "trial" | "rolling_back" => Recovery::Blocked,
        _ => Recovery::Nothing,
    };
    if result == Recovery::Nothing
        && let Some((_, instance)) = guarded
        && (lock.check(root).is_err()
            || RootGate::retire(desktop_root, root, &j.id, instance).is_err())
    {
        return Recovery::Blocked;
    }
    result
}

/// Linux: rewrite the menu entry when it names the old version's launcher.
/// Windows: rewrite our own Start-menu shortcut and the uninstall entry.
fn move_launch_entry(root: &Path, from: &str) {
    #[cfg(not(windows))]
    {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
        let Some(entry) = data.map(|d| d.join("applications/dev.metro.desktop.desktop")) else {
            return;
        };
        if let Err(e) = retarget_linux_entry(&entry, root, from)
            && e.kind() != io::ErrorKind::NotFound
        {
            eprintln!("metro-desktop: the applications menu entry was not changed: {e}");
        }
    }
    #[cfg(windows)]
    {
        // Windows updates are manual. A legacy recovery only moves the
        // pointer, never runs code from either version to rewrite shortcuts.
        let _ = (root, from);
    }
}

/// Match only the exact Exec form generated by the released menu script.
/// Point it at `current` so later commits need not touch the menu at all.
#[cfg(not(windows))]
pub fn retarget_linux_entry(entry: &Path, root: &Path, from: &str) -> io::Result<()> {
    let exec = |path: &Path| -> io::Result<String> {
        let path = path
            .to_str()
            .filter(|path| !path.chars().any(char::is_control))
            .ok_or(io::ErrorKind::InvalidInput)?;
        let mut line = String::from("Exec=/usr/bin/env \"");
        for ch in path.chars() {
            match ch {
                '%' => line.push_str("%%"),
                '"' | '$' | '`' => {
                    line.push_str("\\\\");
                    line.push(ch);
                }
                '\\' => line.push_str("\\\\\\\\"),
                _ => line.push(ch),
            }
        }
        line.push('"');
        Ok(line)
    };
    let bytes = read_evidence_file(entry, 64 * 1024)?;
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let old = exec(&root.join("versions").join(from).join(LAUNCHER))?;
    let current = exec(&root.join("current").join(LAUNCHER))?;
    if text.starts_with("[Desktop Entry]\n")
        && text
            .lines()
            .filter(|line| line.starts_with("Exec="))
            .eq([old.as_str()])
    {
        let replacement: String = text
            .split_inclusive('\n')
            .map(|line| {
                if line.strip_suffix('\n').unwrap_or(line) == old {
                    format!("{current}\n")
                } else {
                    line.to_string()
                }
            })
            .collect();
        write_atomic(entry, replacement.as_bytes())?;
    }
    Ok(())
}

#[cfg(windows)]
pub mod msi {
    use metro_release::Version;
    use std::path::PathBuf;

    /// `HKCU\Software\Metro\Desktop MSI`: `InstallDir`, when `Version` is ours.
    pub fn registered_dir(own: &Version) -> Option<PathBuf> {
        let dir = read("InstallDir")?;
        (read("Version")?.trim() == own.to_string()).then(|| PathBuf::from(dir))
    }

    fn read(name: &str) -> Option<String> {
        use windows_sys::Win32::System::Registry::{
            HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW,
        };
        let w = |s: &str| {
            s.encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let (key, value) = (w("Software\\Metro\\Desktop MSI"), w(name));
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: valid NUL-terminated strings and a sized buffer.
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if rc != 0 {
            return None;
        }
        let n = (len as usize / 2).saturating_sub(1).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}
