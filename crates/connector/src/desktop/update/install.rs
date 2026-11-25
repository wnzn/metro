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

use super::{LastUpdate, now_ms};

pub const MARKER: &str = ".metro-desktop-install";
pub const JOURNAL: &str = ".update.json";
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
            "staged" | "trial" | "committing" | "rolling_back" | "installing" => {
                self.outcome.is_none()
            }
            _ => false,
        };
        self.version == 1
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
    let desktop_root = update_dir.parent()?;
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
    };
    let mut result = None;
    // The MSI journal, whichever way this version was installed (the person
    // may reinstall the previous version from the ZIP).
    let msi_path = update_dir.join(MSI_JOURNAL);
    if let Ok(_lock) = Lock::take(update_dir)
        && let Some(mut j) =
            read_journal(&msi_path).filter(|j| matches!(j.phase.as_str(), "committed" | "failed"))
        && (own_s == j.from || own_s == j.to)
    {
        let keep = j.outcome.as_deref() == Some("install_failed") && j.local_db_snapshot;
        if keep && own_s == j.from {
            // The previous version is back: so is the data from before the update.
            if restore_db(desktop_root, update_dir).is_ok() {
                discard_msi(update_dir, &j);
                result = Some(last(
                    &j,
                    Some("Your data from before the update was restored.".to_string()),
                ));
            }
        } else if keep {
            // Keep the snapshot until the previous version starts again.
            if !j.reported {
                j.reported = true;
                let _ = write_journal(&msi_path, &j);
                result = Some(last(&j, None));
            }
        } else {
            discard_msi(update_dir, &j);
            result = Some(last(&j, None));
        }
    }
    if let Some(root) = kind.root()
        && let Ok(_lock) = Lock::take(root)
        && script_state(root, &own_s)
    {
        let path = root.join(JOURNAL);
        if let Some(j) =
            read_journal(&path).filter(|j| matches!(j.phase.as_str(), "committed" | "failed"))
            && ((j.phase == "committed" && own_s == j.to)
                || (j.phase == "failed" && own_s == j.from))
        {
            if fs::remove_file(&path).is_ok() {
                let _ = metro_release::sync_parent(&path);
                if j.local_db_snapshot {
                    let _ = fs::remove_dir_all(update_dir.join("db-snapshot"));
                }
                let _ = fs::remove_dir_all(stage_assets(update_dir, &j.id));
            }
            result = result.or(Some(last(&j, None)));
        }
    }
    result
}

/// Remove an MSI update's journal, snapshot, download and helper copies.
fn discard_msi(update_dir: &Path, j: &Journal) {
    let journal = update_dir.join(MSI_JOURNAL);
    if fs::remove_file(&journal).is_ok() {
        let _ = metro_release::sync_parent(&journal);
        if j.local_db_snapshot {
            let _ = fs::remove_dir_all(update_dir.join("db-snapshot"));
        }
        let _ = fs::remove_dir_all(stage_assets(update_dir, &j.id));
        let _ = fs::remove_dir_all(update_dir.join(format!("helper-{}", j.id))); // legacy layout
    }
}

/// Drop a staged update that is no longer offered (design 3.5: "otherwise
/// it is deleted"): its version folder (never the current one) or MSI
/// download, and its journal.
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
        InstallKind::LinuxInstaller { root } | InstallKind::WindowsScript { root } => {
            if script_state(root, &j.from) {
                let dest = root.join("versions").join(&j.to);
                if plain_dir(&dest) {
                    let _ = fs::remove_dir_all(dest);
                }
                let _ = fs::remove_dir_all(stage_assets(update_dir, &j.id));
                let _ = fs::remove_file(&path);
            }
        }
        InstallKind::WindowsMsi { .. } => discard_msi(update_dir, &j),
        _ => {}
    }
}

/// A trial whose watchdog is still alive.
pub fn trial_in_progress(root: &Path) -> bool {
    read_journal(&root.join(JOURNAL))
        .is_some_and(|j| j.phase == "trial" && j.watchdog_pid.is_some_and(pid_alive))
}

pub fn msi_in_progress(update_dir: &Path) -> bool {
    read_journal(&update_dir.join(MSI_JOURNAL)).is_some_and(|j| {
        matches!(j.phase.as_str(), "installing" | "trial") && j.watchdog_pid.is_some_and(pid_alive)
    })
}

#[cfg(windows)]
pub fn msi_registered_dir(own: &Version) -> Option<PathBuf> {
    msi::registered_dir(own)
}

/// Recover a helper that stopped in the middle of an MSI update. Never
/// automatically downgrade; restore data only when the old version is back.
pub fn recover_msi(desktop_root: &Path, update_dir: &Path, own: &Version) -> io::Result<()> {
    let path = update_dir.join(MSI_JOURNAL);
    if load_journal(&path)?.is_none() {
        return Ok(());
    }
    let _lock = Lock::take(update_dir).map_err(|e| io::Error::other(e.message))?;
    let Some(mut j) = load_journal(&path)? else {
        return Ok(());
    };
    let version = own.to_string();
    if version != j.from && version != j.to {
        return Ok(()); // unrelated version; never consume its snapshot
    }
    if matches!(j.phase.as_str(), "installing" | "trial") {
        if j.phase == "trial" && version == j.to && trial_ok(update_dir, &j.id) {
            j.phase = JournalPhase::Committed;
            j.outcome = Some("updated".into());
        } else {
            j.phase = JournalPhase::Failed;
            j.outcome = Some("install_failed".into());
            j.detail = Some("The installer was interrupted.".into());
        }
        write_journal(&path, &j)?;
    }
    if j.phase == "failed" && j.local_db_snapshot && j.from == version {
        restore_db(desktop_root, update_dir)?;
        j.local_db_snapshot = false;
        j.detail = Some("Your data from before the update was restored.".into());
        write_journal(&path, &j)?;
        let _ = fs::remove_dir_all(update_dir.join("db-snapshot"));
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
    if load_journal(&path)?.as_ref() != Some(j) {
        return Err(no());
    }
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

/// Make and verify a fresh private helper from this installation's own
/// launcher. It is not taken from unauthenticated journal fields.
pub fn prepare_msi_helper(
    update_dir: &Path,
    j: &Journal,
    source: &Path,
    lock: &Lock,
) -> io::Result<CheckedFile> {
    lock.check(update_dir)?;
    if j.phase != "staged" || load_journal(&update_dir.join(MSI_JOURNAL))?.as_ref() != Some(j) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let assets = stage_assets(update_dir, &j.id);
    if !plain_dir(&assets) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let input = open_input(source)?;
    let mut read = HashingReader::new(input);
    let dir = assets.join(format!("helper-{}", new_id()));
    private_mkdir(&dir)?;
    #[cfg(windows)]
    let parents = archive::pin_windows_dirs(&dir)?;
    let path = dir.join("metro-update-helper.exe");
    let mut output = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    io::copy(
        &mut (&mut read).take(metro_release::MAX_ARCHIVE + 1),
        &mut output,
    )?;
    output.sync_all()?;
    drop(output);
    let file = open_input(&path)?;
    if HashingReader::new(&file)
        .finish()
        .map_err(io::Error::other)?
        != read.finish().map_err(io::Error::other)?
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(CheckedFile {
        path,
        file,
        #[cfg(windows)]
        _parents: parents,
    })
}

// ---- the installers' lock ----

pub struct Lock {
    root: PathBuf,
    handle: fs::File,
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
        if !matches!(fs::symlink_metadata(root.join(LOCK)), Err(e) if e.kind() == io::ErrorKind::NotFound)
        {
            return Err(stage_err("installer_busy"));
        }
        let lock = Self {
            root: root.to_path_buf(),
            handle,
        };
        lock.check(root).map_err(|_| stage_err("installer_busy"))?;
        Ok(lock)
    }

    pub fn check(&self, root: &Path) -> io::Result<()> {
        if self.root != root || !plain_dir(root) {
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
    let from = own.to_string();
    if kind.root().is_some_and(|root| {
        !script_state(root, &from) || install_dir != root.join("versions").join(&from)
    }) {
        return Err(stage_err("installer_busy"));
    }
    if load_journal(&path)
        .map_err(|_| stage_err("installer_busy"))?
        .is_some()
    {
        return Err(stage_err("installer_busy"));
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

/// Copy the local database aside (the server must be stopped).
pub fn snapshot_db(desktop_root: &Path, update_dir: &Path) -> io::Result<bool> {
    let snap = update_dir.join("db-snapshot");
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
    if inputs[0].is_none() {
        if inputs.iter().any(Option::is_some) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "database sidecar without its main file",
            ));
        }
        return Ok(false);
    }
    let tmp = update_dir.join(format!(".snapshot-{}", new_id()));
    private_mkdir(&tmp)?;
    let result = (|| {
        let mut present = [false; 3];
        for (index, (path, input)) in files.iter().zip(inputs).enumerate() {
            if let Some(input) = input {
                copy_synced(input, &tmp.join(path.file_name().unwrap_or_default()))?;
                present[index] = true;
            }
        }
        write_atomic(
            &tmp.join("snapshot.json"),
            &serde_json::to_vec(&present).map_err(io::Error::other)?,
        )?;
        metro_release::rename_durable(&tmp, &snap)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }
    result?;
    Ok(true)
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

fn copy_synced(mut input: fs::File, dest: &Path) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(dest)?;
    io::copy(&mut input, &mut output)?;
    output.sync_all()
}

/// Put the snapshot back byte for byte (removing a newer WAL/SHM).
pub fn restore_db(desktop_root: &Path, update_dir: &Path) -> io::Result<()> {
    let snap = update_dir.join("db-snapshot");
    if !plain_dir(&snap) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut raw = Vec::new();
    open_input(&snap.join("snapshot.json"))?
        .take(128)
        .read_to_end(&mut raw)?;
    let present: [bool; 3] = serde_json::from_slice(&raw).map_err(io::Error::other)?;
    if !present[0] {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let files = db_files(desktop_root);
    let dir = files[0].parent().ok_or(io::ErrorKind::InvalidData)?;
    if !plain_dir(dir) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let tmp = dir.join(format!(".restore-{}", new_id()));
    private_mkdir(&tmp)?;
    let result = (|| {
        // Finish every copy before changing any live filename. The caller
        // must keep recovery pending and must not open the DB until all
        // renames/removals and the directory sync succeed.
        for (path, present) in files.iter().zip(present) {
            if present {
                let name = path.file_name().ok_or(io::ErrorKind::InvalidData)?;
                copy_synced(open_input(&snap.join(name))?, &tmp.join(name))?;
            }
        }
        for (path, present) in files.iter().zip(present).rev() {
            if present {
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
    let _ = fs::remove_dir_all(&tmp);
    result
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
    move_launch_entry(root, &j.from, &j.to);
    j.phase = JournalPhase::Committed;
    j.outcome = Some("updated".into());
    write_journal(&path, j)
}

/// Undo a staged or tried update: restore the DB snapshot, remove the new
/// version, keep the pointer on `from`, journal `failed`.
pub fn rollback(
    root: &Path,
    desktop_root: &Path,
    update_dir: &Path,
    j: &mut Journal,
    lock: &Lock,
    outcome: &str,
    detail: Option<String>,
) -> io::Result<()> {
    let path = root.join(JOURNAL);
    lock.check(root)?;
    ensure_script_transition(root, j, &["trial", "committing", "rolling_back"])?;
    j.phase = JournalPhase::RollingBack;
    j.detail = detail;
    write_journal(&path, j)?;
    if j.local_db_snapshot {
        restore_db(desktop_root, update_dir)?;
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
    /// Finished a commit the previous run started.
    Committed,
    /// Rolled back an interrupted update.
    RolledBack,
    /// Cannot safely open the database or install until recovery succeeds.
    Blocked,
}

pub fn recover(root: &Path, desktop_root: &Path, update_dir: &Path, own: &Version) -> Recovery {
    let path = root.join(JOURNAL);
    if matches!(load_journal(&path), Ok(None)) {
        return Recovery::Nothing;
    }
    let Ok(lock) = Lock::take(root) else {
        return Recovery::Blocked;
    };
    let mut j = match load_journal(&path) {
        Ok(Some(j)) => j,
        Ok(None) => return Recovery::Nothing,
        Err(_) => return Recovery::Blocked,
    };
    let own = own.to_string();
    match j.phase.as_str() {
        "committing" if own == j.to => {
            if commit(root, &mut j, &lock).is_ok() {
                Recovery::Committed
            } else {
                Recovery::Blocked
            }
        }
        "committing" | "trial" | "rolling_back" if own == j.from || own == j.to => {
            match rollback(
                root,
                desktop_root,
                update_dir,
                &mut j,
                &lock,
                "rolled_back",
                Some("interrupted".into()),
            ) {
                Ok(()) => Recovery::RolledBack,
                Err(e) => {
                    eprintln!("metro-desktop: update recovery blocked: {e}");
                    Recovery::Blocked
                }
            }
        }
        "committing" | "trial" | "rolling_back" => Recovery::Blocked,
        _ => Recovery::Nothing,
    }
}

/// Linux: rewrite the menu entry when it names the old version's launcher.
/// Windows: rewrite our own Start-menu shortcut and the uninstall entry.
fn move_launch_entry(root: &Path, from: &str, to: &str) {
    #[cfg(not(windows))]
    {
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
        let Some(entry) = data.map(|d| d.join("applications/dev.metro.desktop.desktop")) else {
            return;
        };
        let Ok(text) = fs::read_to_string(&entry) else {
            return;
        };
        let old = root.join("versions").join(from).join(LAUNCHER);
        if !text.contains(&*old.to_string_lossy()) {
            return;
        }
        let script = root.join("versions").join(to).join("install-desktop");
        if script.is_file() {
            let _ = std::process::Command::new(&script)
                .arg("--install")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .status();
        }
    }
    #[cfg(windows)]
    {
        let _ = from;
        windows_entry::update(root, to);
    }
}

#[cfg(windows)]
mod windows_entry {
    use std::path::Path;

    /// Rewrite `Metro.lnk` (only when it points into this install) and the
    /// HKCU uninstall entry's version, through PowerShell with every value
    /// passed in the environment (never on a command line).
    pub fn update(root: &Path, to: &str) {
        let target = root.join("versions").join(to).join("Metro.exe");
        let script = r#"
$ErrorActionPreference = 'Stop'
$lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'Metro.lnk'
if (Test-Path -LiteralPath $lnk) {
  $sh = New-Object -ComObject WScript.Shell
  $s = $sh.CreateShortcut($lnk)
  if ($s.TargetPath.StartsWith($env:METRO_UPDATE_ROOT + '\versions\', [StringComparison]::OrdinalIgnoreCase)) {
    $s.TargetPath = $env:METRO_UPDATE_TARGET
    $s.WorkingDirectory = Split-Path -Parent $env:METRO_UPDATE_TARGET
    $s.IconLocation = $env:METRO_UPDATE_TARGET + ',0'
    $s.Save()
  }
}
$key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\dev.metro.desktop'
if (Test-Path -LiteralPath $key) {
  Set-ItemProperty -LiteralPath $key -Name DisplayVersion -Value $env:METRO_UPDATE_VERSION
  Set-ItemProperty -LiteralPath $key -Name DisplayIcon -Value $env:METRO_UPDATE_TARGET
}
"#;
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let ps = Path::new(&system).join("System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        let _ = std::process::Command::new(ps)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                script,
            ])
            .env("METRO_UPDATE_ROOT", root)
            .env("METRO_UPDATE_TARGET", &target)
            .env("METRO_UPDATE_VERSION", to)
            .status();
    }
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
