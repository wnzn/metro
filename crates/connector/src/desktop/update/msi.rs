//! In-app updates of the per-user Windows MSI install.
//!
//! The journal is `<desktop_root>/update/msi-update.json` (version 3). The
//! legacy `journal.json` of the withdrawn helper is never read or consumed
//! here; [`super::install::gate_legacy_msi`] still blocks on it.
//!
//! Phases, and who writes them (every write under the update dir's
//! installer lock):
//!
//! 1. `staged` (download task): the verified MSI, `SHA256SUMS` and its
//!    bundle in the private `stage-<id>`.
//! 2. `backing_up` → `handoff` (old app, on Restart now; it also holds the
//!    instance lock): the stopped database is copied, then a copy of the
//!    launcher in `helper-<id>` is started with `--update-helper <id>` and
//!    the app exits.
//! 3. `installing` (helper, holding the instance lock and installer lock):
//!    written before `msiexec` starts, then again with msiexec's identity.
//! 4. `installed` / `failed` (helper, from msiexec's exit code checked
//!    against facts), then the helper starts Metro again.
//! 5. `committed` (new version, at its first rendered start).
//!
//! The next start decides an unfinished journal from facts it can check
//! ([`Facts`]): whether Windows Installer is running an install, the
//! installed product version for Metro's UpgradeCode, the registered
//! folder and version, and `RELEASE.json` plus `Metro.exe` in that folder.
//! A recorded process counts only as "maybe still running", never as proof
//! that it ended. Anything that doesn't add up stops before the workspace
//! opens and keeps the journal and the backup ([`Decision::Uncertain`]).
//!
//! The backup is never restored automatically on this path: if Windows
//! Installer fails, no newer version ran, so the live database stays
//! authoritative; once the new version is installed there's no automatic
//! downgrade. A committed update drops the backup.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use metro_release::archive::{self, HashingReader};
use metro_release::{ReleaseVerifier, VerifiedRelease, Version};
use serde::{Deserialize, Serialize};

use super::install::{self, InstallKind, Lock, StageError};
use super::{InstallFailure, LastUpdate, now_ms};
use crate::desktop::store::InstanceLock;

pub const JOURNAL: &str = "msi-update.json";
/// The released product's UpgradeCode (`packaging/windows-msi/build_msi.py`).
pub const UPGRADE_CODE: &str = "{29BBE899-7D6A-49A7-B60E-F9644544D683}";
/// Where the MSI remembers its folder, version and choices (HKCU).
pub const REG_KEY: &str = r"Software\Metro\Desktop MSI";
pub const PACKAGE: &str = "package.msi";
pub const LOG: &str = "msiexec.log";
#[cfg(windows)]
pub const LAUNCHER: &str = "Metro.exe";
#[cfg(not(windows))]
pub const LAUNCHER: &str = "Metro";

/// A process, identified by its id and creation time (a PID alone can be
/// reused).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proc {
    pub pid: u32,
    /// Creation time (Windows FILETIME), 0 when it couldn't be read.
    pub created: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Staged,
    BackingUp,
    Handoff,
    Installing,
    Installed,
    Committed,
    Failed,
}

/// Why an attempt failed (`failure` in the journal).
pub const FAILURES: &[&str] = &[
    "installer_cancelled",
    "installer_busy",
    "installer_failed",
    "helper_not_started",
    "not_verified",
    "installed_changed",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MsiJournal {
    pub version: u32,
    pub id: String,
    pub from: String,
    pub to: String,
    pub phase: Phase,
    /// The registered INSTALLDIR when the update was staged.
    pub install_dir: PathBuf,
    /// SHA-256 (hex) of the signed `SHA256SUMS` and of the MSI.
    pub manifest_sha256: String,
    pub msi_sha256: String,
    #[serde(default)]
    pub local_db_snapshot: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_manifest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub helper: Option<Proc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msiexec: Option<Proc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub reboot_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub started_at: i64,
}

fn hex32(s: &str) -> bool {
    metro_release::parse_hex32(s).is_some()
}

impl MsiJournal {
    pub fn valid(&self) -> bool {
        let none_yet = self.helper.is_none()
            && self.msiexec.is_none()
            && self.exit_code.is_none()
            && !self.reboot_required
            && self.outcome.is_none()
            && self.failure.is_none();
        let backup = self.backup_manifest.as_deref().is_some_and(hex32);
        let phase_ok = match self.phase {
            Phase::Staged | Phase::BackingUp => {
                none_yet && self.backup_manifest.is_none() && !self.local_db_snapshot
            }
            Phase::Handoff => none_yet && backup,
            Phase::Installing => {
                backup
                    && self.helper.is_some()
                    && self.outcome.is_none()
                    && self.failure.is_none()
                    && !self.reboot_required
            }
            Phase::Installed => {
                backup && self.helper.is_some() && self.outcome.is_none() && self.failure.is_none()
            }
            Phase::Committed => {
                self.outcome.as_deref() == Some("updated") && self.failure.is_none()
            }
            Phase::Failed => {
                self.outcome.as_deref() == Some("install_failed")
                    && self
                        .failure
                        .as_deref()
                        .is_some_and(|f| FAILURES.contains(&f))
                    && !self.reboot_required
            }
        };
        self.version == 3
            && install::valid_id(&self.id)
            && matches!((Version::parse(&self.from), Version::parse(&self.to)), (Some(a), Some(b)) if a < b)
            && self.install_dir.is_absolute()
            && hex32(&self.manifest_sha256)
            && hex32(&self.msi_sha256)
            && self.backup_manifest.as_deref().is_none_or(hex32)
            && self.helper.is_none_or(|p| p.pid != 0)
            && self.msiexec.is_none_or(|p| p.pid != 0)
            && self.detail.as_deref().is_none_or(|d| d.len() <= 200)
            && phase_ok
    }
}

pub fn journal_path(update_dir: &Path) -> PathBuf {
    update_dir.join(JOURNAL)
}

pub fn load(update_dir: &Path) -> io::Result<Option<MsiJournal>> {
    let f = match install::open_state(&journal_path(update_dir)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut raw = Vec::new();
    f.take(64 * 1024 + 1).read_to_end(&mut raw)?;
    if raw.len() > 64 * 1024 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let j: MsiJournal = serde_json::from_slice(&raw).map_err(io::Error::other)?;
    if !j.valid() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(Some(j))
}

pub fn save(update_dir: &Path, j: &MsiJournal) -> io::Result<()> {
    if !j.valid() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    install::write_atomic(
        &journal_path(update_dir),
        &serde_json::to_vec(j).map_err(io::Error::other)?,
    )
}

/// Write `j` only if the journal on disk is still `expected`.
fn transition(update_dir: &Path, expected: &MsiJournal, j: &MsiJournal) -> io::Result<()> {
    if load(update_dir)?.as_ref() != Some(expected) {
        return Err(install::journal_uncertain());
    }
    save(update_dir, j)
}

pub fn stage_dir(update_dir: &Path, id: &str) -> PathBuf {
    debug_assert!(install::valid_id(id));
    update_dir.join(format!("stage-{id}"))
}

pub fn helper_dir(update_dir: &Path, id: &str) -> PathBuf {
    debug_assert!(install::valid_id(id));
    update_dir.join(format!("helper-{id}"))
}

// ---- Windows Installer's version mapping ----

/// The ProductVersion `build_msi.py` gives a release: `MAJOR.MINOR.(PATCH*100+99)`.
pub fn product_version(v: &Version) -> Option<String> {
    (v.major <= 255 && v.minor <= 255 && v.patch <= 654)
        .then(|| format!("{}.{}.{}", v.major, v.minor, v.patch * 100 + 99))
}

// ---- facts ----

/// What the MSI remembered in `HKCU\Software\Metro\Desktop MSI`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registered {
    pub dir: PathBuf,
    pub version: String,
    pub local_runner: Option<bool>,
    pub start_menu: Option<bool>,
}

/// Facts the next start and the helper can check. `None` means "couldn't
/// tell", which never counts as an answer.
pub trait Facts {
    /// Windows Installer is running (or has suspended) an installation.
    fn installer_busy(&self) -> Option<bool>;
    /// ProductVersion of each per-user product with Metro's UpgradeCode.
    fn installed_versions(&self) -> Option<Vec<String>>;
    fn registered(&self) -> Option<Registered>;
    /// May still be running (or can't be ruled out).
    fn running(&self, p: &Proc) -> bool;
    /// An `msiexec.exe` that `parent` started is still running (covers a
    /// helper that died before it could record msiexec's identity).
    fn msiexec_child_running(&self, parent: &Proc) -> Option<bool>;
    /// The version `dir` holds: `RELEASE.json`'s, when `Metro.exe` matches
    /// its listed SHA-256.
    fn files_version(&self, dir: &Path) -> Option<String> {
        files_version(dir)
    }
}

/// `RELEASE.json` in an installed folder, checked against `Metro.exe`.
pub fn files_version(dir: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct Manifest {
        version: String,
        files: std::collections::BTreeMap<String, String>,
    }
    let mut raw = Vec::new();
    install::open_input(&dir.join("RELEASE.json"))
        .ok()?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() > 1024 * 1024 {
        return None;
    }
    let m: Manifest = serde_json::from_slice(&raw).ok()?;
    Version::parse(&m.version)?;
    let want = metro_release::parse_hex32(m.files.get("Metro.exe")?)?;
    let exe = install::open_input(&dir.join("Metro.exe")).ok()?;
    (HashingReader::new(exe).finish().ok()? == want).then_some(m.version)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('/', "\\");
        s.trim_end_matches('\\').to_lowercase()
    };
    norm(a) == norm(b)
}

/// Every fact says `version` is the installed one in `dir`.
fn installed_as(f: &dyn Facts, version: &str, dir: &Path) -> Option<bool> {
    let pv = product_version(&Version::parse(version)?)?;
    let products = f.installed_versions()?;
    let reg = f.registered()?;
    Some(
        products == [pv]
            && reg.version == version
            && same_dir(&reg.dir, dir)
            && f.files_version(dir).as_deref() == Some(version),
    )
}

/// Windows Installer and the registry say `to`, but the files are still
/// `from`: replacement waits for a Windows restart.
fn reboot_pending(f: &dyn Facts, j: &MsiJournal) -> bool {
    let Some(pv) = Version::parse(&j.to).and_then(|v| product_version(&v)) else {
        return false;
    };
    f.installed_versions().is_some_and(|p| p == [pv])
        && f.registered()
            .is_some_and(|r| r.version == j.to && same_dir(&r.dir, &j.install_dir))
        && f.files_version(&j.install_dir).as_deref() == Some(j.from.as_str())
}

// ---- the next start's decision ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing unfinished for this version: start normally.
    Proceed,
    /// Write this journal (under the locks), then start normally.
    Write(Box<MsiJournal>),
    /// The update is still being installed: exit quietly, change nothing.
    Updating,
    /// Windows Installer finished but files are replaced at restart.
    RestartWindows,
    /// Facts don't add up: stop, keep the journal and backup.
    Uncertain,
}

fn failed(j: &MsiJournal, failure: &str, detail: &str) -> MsiJournal {
    let mut n = j.clone();
    n.phase = Phase::Failed;
    n.outcome = Some("install_failed".into());
    n.failure = Some(failure.into());
    n.detail = Some(detail.into());
    n.reboot_required = false;
    n
}

/// Decide an unfinished MSI update at a normal start of `own`. `locked`:
/// the caller holds the instance lock and the update dir's installer lock
/// (without them only `Proceed`, `Updating`, `RestartWindows` and
/// `Uncertain` are answers; any `Write` means "take the locks and ask
/// again").
pub fn decide(j: Option<&MsiJournal>, own: &str, f: &dyn Facts) -> Decision {
    let Some(j) = j else {
        return Decision::Proceed;
    };
    let is_from = own == j.from;
    let is_to = own == j.to;
    match j.phase {
        Phase::Staged => Decision::Proceed,
        // Nothing newer ran before the backup was published: the live
        // database stays authoritative. Retire this attempt.
        Phase::BackingUp if is_from => Decision::Write(Box::new(failed(
            j,
            "helper_not_started",
            "the update stopped while saving the backup",
        ))),
        // The helper writes `installing` under the same locks before it
        // starts msiexec, so under those locks `handoff` means msiexec
        // never started.
        Phase::Handoff if is_from => Decision::Write(Box::new(failed(
            j,
            "helper_not_started",
            "the update helper didn't start the installer",
        ))),
        Phase::Installing => {
            if j.helper.is_some_and(|p| f.running(&p)) || j.msiexec.is_some_and(|p| f.running(&p)) {
                return Decision::Updating;
            }
            if j.msiexec.is_none()
                && let Some(helper) = j.helper
            {
                match f.msiexec_child_running(&helper) {
                    Some(false) => {}
                    Some(true) => return Decision::Updating,
                    None => return Decision::Uncertain,
                }
            }
            match f.installer_busy() {
                Some(false) => {}
                Some(true) => return Decision::Updating,
                None => return Decision::Uncertain,
            }
            if installed_as(f, &j.to, &j.install_dir) == Some(true) {
                if !is_to {
                    return Decision::Uncertain;
                }
                let mut n = j.clone();
                n.phase = Phase::Installed;
                return Decision::Write(Box::new(n));
            }
            if reboot_pending(f, j) {
                return Decision::RestartWindows;
            }
            if installed_as(f, &j.from, &j.install_dir) == Some(true) && is_from {
                return Decision::Write(Box::new(failed(
                    j,
                    "installer_failed",
                    "Windows Installer didn't finish the update",
                )));
            }
            Decision::Uncertain
        }
        Phase::Installed => {
            if is_to && installed_as(f, &j.to, &j.install_dir) == Some(true) {
                Decision::Proceed
            } else if f.installer_busy() == Some(false) && reboot_pending(f, j) {
                Decision::RestartWindows
            } else {
                Decision::Uncertain
            }
        }
        Phase::Committed if is_to => Decision::Proceed,
        Phase::Failed if is_from => Decision::Proceed,
        _ => Decision::Uncertain,
    }
}

// ---- after msiexec returned ----

/// Windows Installer's exit codes that the helper tells apart.
pub fn failure_for(code: Option<i32>) -> (&'static str, String) {
    match code {
        Some(1602) => ("installer_cancelled", "the installer was cancelled".into()),
        Some(1618) => (
            "installer_busy",
            "another installation was in progress".into(),
        ),
        Some(c) => (
            "installer_failed",
            format!("the installer stopped (code {c})"),
        ),
        None => ("installer_failed", "the installer didn't run".into()),
    }
}

/// The journal after msiexec exited with `code`, or `None` when the facts
/// don't match the code (the journal then stays `installing`).
pub fn after_msiexec(j: &MsiJournal, code: Option<i32>, f: &dyn Facts) -> Option<MsiJournal> {
    // 1618: Windows Installer refused to start ours because another
    // installation runs, so a busy installer is that other one.
    let busy_ok = code == Some(1618) && f.installer_busy().is_some();
    if j.phase != Phase::Installing || !(busy_ok || f.installer_busy() == Some(false)) {
        return None;
    }
    let mut n = j.clone();
    n.exit_code = code;
    match code {
        Some(0 | 3010 | 1641) => {
            if installed_as(f, &j.to, &j.install_dir) == Some(true) {
                n.phase = Phase::Installed;
                Some(n)
            } else if code != Some(0) && reboot_pending(f, j) {
                n.phase = Phase::Installed;
                n.reboot_required = true;
                Some(n)
            } else {
                None
            }
        }
        _ => {
            if installed_as(f, &j.from, &j.install_dir) == Some(true) {
                let (failure, detail) = failure_for(code);
                let mut n = failed(&n, failure, &detail);
                n.exit_code = code;
                Some(n)
            } else {
                None
            }
        }
    }
}

/// `msiexec` arguments, written exactly (msiexec parses its own command
/// line): the remembered folder and choices, per-user, passive.
pub fn msiexec_command_line(msi: &Path, log: &Path, reg: &Registered) -> io::Result<String> {
    let quoted = |p: &Path| -> io::Result<String> {
        let s = p.to_str().ok_or(io::ErrorKind::InvalidInput)?;
        if s.is_empty() || s.contains('"') || s.chars().any(char::is_control) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(format!("\"{s}\""))
    };
    let dir = reg
        .dir
        .to_str()
        .ok_or(io::ErrorKind::InvalidInput)?
        .trim_end_matches('\\');
    let mut line = format!(
        "/i {} /passive /norestart REBOOT=ReallySuppress INSTALLDIR={}",
        quoted(msi)?,
        quoted(Path::new(dir))?
    );
    if let Some(on) = reg.local_runner {
        line.push_str(if on {
            " LOCALRUNNER=1"
        } else {
            " LOCALRUNNER=0"
        });
    }
    if let Some(on) = reg.start_menu {
        line.push_str(if on {
            " STARTMENUSHORTCUT=1"
        } else {
            " STARTMENUSHORTCUT=0"
        });
    }
    line.push_str(" /l*v ");
    line.push_str(&quoted(log)?);
    Ok(line)
}

// ---- staging (download task) ----

fn stage_error(code: &'static str) -> StageError {
    StageError {
        code,
        message: super::message_for(code).to_string(),
    }
}

/// Clean up everything of a finished attempt (journal last).
fn retire(update_dir: &Path, j: &MsiJournal) -> io::Result<()> {
    install::clean_backup(
        update_dir,
        &backup_journal(j, install::JournalPhase::Failed),
    )?;
    install::remove_owned_dir(&stage_dir(update_dir, &j.id))?;
    let path = journal_path(update_dir);
    fs::remove_file(&path)?;
    metro_release::sync_parent(&path)?;
    sweep_helpers(update_dir);
    Ok(())
}

/// Leftover helper copies (best effort: one may still be exiting).
fn sweep_helpers(update_dir: &Path) {
    let Ok(entries) = fs::read_dir(update_dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_prefix("helper-")) else {
            continue;
        };
        if install::valid_id(id) {
            let _ = install::remove_owned_dir(&e.path());
        }
    }
}

/// The DB snapshot helpers in [`install`] take a script journal; this is
/// the same attempt in that shape (only id, from, to, phase and backup
/// fields are read).
fn backup_journal(j: &MsiJournal, phase: install::JournalPhase) -> install::Journal {
    install::Journal {
        version: 2,
        id: j.id.clone(),
        from: j.from.clone(),
        to: j.to.clone(),
        phase,
        outcome: None,
        detail: None,
        local_db_snapshot: j.local_db_snapshot,
        backup_manifest: j.backup_manifest.clone(),
        watchdog_pid: None,
        source_pid: None,
        reported: false,
        started_at: j.started_at,
    }
}

fn finished_for(j: &MsiJournal, own: &str) -> bool {
    (j.phase == Phase::Committed && own == j.to) || (j.phase == Phase::Failed && own == j.from)
}

/// Keep the verified MSI and its signed evidence; journal `staged`.
pub(super) fn stage(
    install_dir: &Path,
    update_dir: &Path,
    own: &Version,
    release: &VerifiedRelease,
    download: &Path,
) -> Result<String, StageError> {
    let target = release.version;
    if target <= *own {
        return Err(stage_error("not_latest"));
    }
    let lock = Lock::take(update_dir)?;
    let from = own.to_string();
    match load(update_dir) {
        Ok(None) => {}
        Ok(Some(j)) if finished_for(&j, &from) => {
            retire(update_dir, &j).map_err(|_| stage_error("io"))?;
        }
        Ok(Some(j)) if j.phase == Phase::Staged && j.from == from => {
            // A superseded stage of this version.
            install::remove_owned_dir(&stage_dir(update_dir, &j.id))
                .and_then(|()| fs::remove_file(journal_path(update_dir)))
                .map_err(|_| stage_error("io"))?;
        }
        Ok(Some(_)) => return Err(stage_error("installer_busy")),
        Err(_) => return Err(stage_error(install::JOURNAL_UNCERTAIN)),
    }
    let digest = release
        .sha256_of(&release.msi)
        .map_err(|_| stage_error("inconsistent"))?;
    let id = install::new_id();
    let journal = MsiJournal {
        version: 3,
        id: id.clone(),
        from,
        to: target.to_string(),
        phase: Phase::Staged,
        install_dir: install_dir.to_path_buf(),
        manifest_sha256: metro_release::hex(&metro_release::sha256(&release.evidence.manifest)),
        msi_sha256: metro_release::hex(&digest),
        local_db_snapshot: false,
        backup_manifest: None,
        helper: None,
        msiexec: None,
        exit_code: None,
        reboot_required: false,
        outcome: None,
        failure: None,
        detail: None,
        started_at: now_ms(),
    };
    if !journal.valid() {
        return Err(stage_error("io"));
    }
    let assets = stage_dir(update_dir, &id);
    install::private_mkdir(&assets).map_err(|_| stage_error("io"))?;
    let result = (|| {
        let mut input = install::open_input(download).map_err(|_| stage_error("io"))?;
        let copy = archive::private_copy_verified(&mut input, &digest, &assets.join(PACKAGE))
            .map_err(|_| stage_error("not_verified"))?;
        drop(copy);
        install::write_atomic(&assets.join("SHA256SUMS"), &release.evidence.manifest)
            .map_err(|_| stage_error("io"))?;
        install::write_atomic(
            &assets.join("SHA256SUMS.sigstore.json"),
            &release.evidence.bundle,
        )
        .map_err(|_| stage_error("io"))?;
        lock.check(update_dir)
            .map_err(|_| stage_error("installer_busy"))?;
        save(update_dir, &journal).map_err(|_| stage_error("io"))
    })();
    if result.is_err() && matches!(load(update_dir), Ok(None)) {
        let _ = fs::remove_dir_all(&assets);
    }
    result.map(|()| id)
}

pub(super) fn staged(update_dir: &Path) -> Option<(Version, String)> {
    let j = load(update_dir).ok().flatten()?;
    (j.phase == Phase::Staged
        && install::open_state(&stage_dir(update_dir, &j.id).join(PACKAGE)).is_ok())
    .then(|| Version::parse(&j.to).map(|v| (v, j.id)))
    .flatten()
}

pub(super) fn discard_staged(update_dir: &Path) {
    let Ok(_lock) = Lock::take(update_dir) else {
        return;
    };
    if let Ok(Some(j)) = load(update_dir)
        && j.phase == Phase::Staged
        && install::remove_owned_dir(&stage_dir(update_dir, &j.id)).is_ok()
    {
        let _ = fs::remove_file(journal_path(update_dir));
    }
}

fn failure_code(failure: Option<&str>) -> InstallFailure {
    match failure {
        Some("installer_busy") => InstallFailure::InstallerBusy,
        Some("installer_cancelled") => InstallFailure::InstallerCancelled,
        Some("not_verified") => InstallFailure::StageVerificationFailed,
        _ => InstallFailure::InstallerFailed,
    }
}

/// A journal that ended since the last start becomes `last_update`; its
/// backup, stage and journal are removed. Anything else is left alone.
pub(super) fn take_finished(update_dir: &Path, own: &Version) -> Option<LastUpdate> {
    let own = own.to_string();
    let _lock = Lock::take(update_dir).ok()?;
    let j = load(update_dir).ok().flatten()?;
    if !finished_for(&j, &own) {
        return None;
    }
    retire(update_dir, &j).ok()?;
    Some(last_update(&j))
}

fn last_update(j: &MsiJournal) -> LastUpdate {
    LastUpdate {
        from: j.from.clone(),
        to: j.to.clone(),
        outcome: j.outcome.clone().unwrap_or_else(|| "updated".into()),
        at: now_ms(),
        detail: j.detail.clone(),
        failure_code: (j.phase == Phase::Failed).then(|| failure_code(j.failure.as_deref())),
    }
}

/// `POST /v1/update/started` in the new version: commit and drop the
/// backup.
pub(super) fn started(update_dir: &Path, own: &Version) -> Option<LastUpdate> {
    let _lock = Lock::take(update_dir).ok()?;
    let j = load(update_dir).ok().flatten()?;
    if j.phase != Phase::Installed || j.to != own.to_string() || j.reboot_required {
        return None;
    }
    let mut n = j.clone();
    n.phase = Phase::Committed;
    n.outcome = Some("updated".into());
    transition(update_dir, &j, &n).ok()?;
    retire(update_dir, &n).ok()?;
    Some(last_update(&n))
}

// ---- verification of the exact bytes ----

/// The staged MSI, open so it can't be changed, renamed or deleted (on
/// Windows) while it is used, with its folders pinned.
pub struct CheckedMsi {
    pub path: PathBuf,
    pub file: fs::File,
    #[cfg(windows)]
    _parents: Vec<fs::File>,
}

fn read_capped(path: &Path, cap: usize) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    install::open_input(path)?
        .take((cap + 1) as u64)
        .read_to_end(&mut data)?;
    if data.len() > cap {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(data)
}

/// Verify the signed evidence and the MSI's bytes through the handle that
/// stays open, and return it. `offer`: the manifest the updater offers now.
pub fn verify_package(
    update_dir: &Path,
    j: &MsiJournal,
    verifier: &dyn ReleaseVerifier,
    offer: Option<&[u8]>,
) -> io::Result<CheckedMsi> {
    let no = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the staged installer no longer verifies",
        )
    };
    let assets = stage_dir(update_dir, &j.id);
    if !install::plain_dir(&assets) {
        return Err(no());
    }
    let manifest = read_capped(&assets.join("SHA256SUMS"), metro_release::MAX_MANIFEST)?;
    if metro_release::hex(&metro_release::sha256(&manifest)) != j.manifest_sha256
        || offer.is_some_and(|m| m != manifest)
    {
        return Err(no());
    }
    let bundle = read_capped(
        &assets.join("SHA256SUMS.sigstore.json"),
        metro_release::MAX_BUNDLE,
    )?;
    let release = metro_release::release::verify_manifest(verifier, &manifest, &bundle)
        .map_err(io::Error::other)?;
    let digest = release.sha256_of(&release.msi).map_err(io::Error::other)?;
    if release.version.to_string() != j.to || metro_release::hex(&digest) != j.msi_sha256 {
        return Err(no());
    }
    let path = assets.join(PACKAGE);
    #[cfg(windows)]
    let parents = archive::pin_windows_dirs(&assets)?;
    let file = install::open_input(&path)?;
    if HashingReader::new(&file)
        .finish()
        .map_err(io::Error::other)?
        != digest
    {
        return Err(no());
    }
    Ok(CheckedMsi {
        path,
        file,
        #[cfg(windows)]
        _parents: parents,
    })
}

impl super::Updater {
    /// Revalidate the staged MSI after the launcher stopped local work and
    /// took the installer lock (the MSI counterpart of `verify_apply`).
    pub fn verify_msi_apply(
        &self,
        req: &super::ApplyRequest,
        lock: &Lock,
    ) -> io::Result<(MsiJournal, CheckedMsi)> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the update is no longer staged and offered",
            )
        };
        lock.check(&self.cfg.dir)?;
        let j = load(&self.cfg.dir)
            .map_err(|_| install::journal_uncertain())?
            .filter(|j| {
                j.phase == Phase::Staged
                    && j.id == req.journal_id
                    && j.from == self.cfg.own.to_string()
                    && j.to == req.version.to_string()
            })
            .ok_or_else(install::journal_uncertain)?;
        let InstallKind::WindowsMsi { dir } = &self.cfg.kind else {
            return Err(invalid());
        };
        if !same_dir(dir, &j.install_dir) || !self.can_update_in_app() {
            return Err(invalid());
        }
        let manifest = {
            let inner = self.lock();
            if inner.phase != super::Phase::Applying
                || inner.status_error
                || !super::offered(&inner.status, self.cfg.own, req.version)
                || inner.staged.as_ref() != Some(&(req.version, req.journal_id.clone()))
            {
                return Err(invalid());
            }
            inner
                .status
                .evidence
                .as_ref()
                .ok_or_else(invalid)?
                .manifest
                .clone()
        };
        let msi = verify_package(
            &self.cfg.dir,
            &j,
            self.cfg.verifier.as_ref(),
            Some(&manifest),
        )?;
        Ok((j, msi))
    }

    /// Record the outcome of the MSI handoff in `last_update` right away
    /// (a later start would show the same from the journal).
    pub fn record_msi_outcome(&self, last: LastUpdate) -> bool {
        let mut inner = self.lock();
        inner.status.last_update = Some(last);
        inner.phase = super::Phase::Idle;
        inner.staged = None;
        self.save_status(&mut inner);
        !inner.status_error
    }

    pub(super) fn msi_started(&self) -> bool {
        match started(&self.cfg.dir, &self.cfg.own) {
            Some(last) => {
                let mut inner = self.lock();
                inner.status.last_update = Some(last);
                self.save_status(&mut inner);
                true
            }
            None => false,
        }
    }
}

// ---- the old app's handoff ----

/// Back up the stopped database and journal `handoff`.
pub fn begin_handoff(
    desktop_root: &Path,
    update_dir: &Path,
    j: &mut MsiJournal,
    lock: &Lock,
    instance: &InstanceLock,
) -> io::Result<()> {
    lock.check(update_dir)?;
    instance.check(desktop_root)?;
    if j.phase != Phase::Staged || load(update_dir)?.as_ref() != Some(j) {
        return Err(install::journal_uncertain());
    }
    let _writer = crate::desktop_local::update_writer_exclusion(desktop_root)?;
    let before = j.clone();
    j.phase = Phase::BackingUp;
    transition(update_dir, &before, j)?;
    let (has_db, manifest) = install::snapshot_db(
        desktop_root,
        update_dir,
        &backup_journal(j, install::JournalPhase::BackingUp),
    )?;
    let before = j.clone();
    j.local_db_snapshot = has_db;
    j.backup_manifest = Some(manifest);
    j.phase = Phase::Handoff;
    transition(update_dir, &before, j)
}

/// The helper: a copy of this launcher in the private `helper-<id>` folder
/// (msiexec replaces the installed one), held open while it is started.
pub struct HelperExe {
    pub path: PathBuf,
    pub file: fs::File,
    #[cfg(windows)]
    _parents: Vec<fs::File>,
}

pub fn prepare_helper(update_dir: &Path, j: &MsiJournal, exe: &Path) -> io::Result<HelperExe> {
    use std::io::Write;
    let dir = helper_dir(update_dir, &j.id);
    install::remove_owned_dir(&dir)?;
    install::private_mkdir(&dir)?;
    let path = dir.join(LAUNCHER);
    let source = install::open_input(exe)?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o700);
    }
    let mut out = options.open(&path)?;
    let mut reader = HashingReader::new(source);
    io::copy(&mut reader, &mut out)?;
    out.flush()?;
    out.sync_all()?;
    drop(out);
    let want = reader.finish().map_err(io::Error::other)?;
    #[cfg(windows)]
    let parents = archive::pin_windows_dirs(&dir)?;
    let file = install::open_input(&path)?;
    if HashingReader::new(&file)
        .finish()
        .map_err(io::Error::other)?
        != want
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the update helper copy changed",
        ));
    }
    Ok(HelperExe {
        path,
        file,
        #[cfg(windows)]
        _parents: parents,
    })
}

/// The handoff couldn't start the helper: no installer ran.
pub fn abandon_handoff(update_dir: &Path, j: &MsiJournal, lock: &Lock) -> io::Result<LastUpdate> {
    lock.check(update_dir)?;
    let n = failed(j, "helper_not_started", "the update helper didn't start");
    transition(update_dir, j, &n)?;
    retire(update_dir, &n)?;
    Ok(last_update(&n))
}

// ---- the helper ----

/// Apply a [`Decision::Write`] under the locks, if the journal is unchanged.
pub fn write_decision(
    update_dir: &Path,
    expected: &MsiJournal,
    n: &MsiJournal,
    lock: &Lock,
) -> io::Result<()> {
    lock.check(update_dir)?;
    transition(update_dir, expected, n)
}

/// The helper's claim: `handoff` for exactly this id.
pub fn claim(update_dir: &Path, id: &str, lock: &Lock) -> io::Result<MsiJournal> {
    lock.check(update_dir)?;
    load(update_dir)?
        .filter(|j| j.id == id && j.phase == Phase::Handoff)
        .ok_or_else(install::journal_uncertain)
}

/// Before msiexec: the old version is the installed one, in the journal's
/// folder (else the attempt fails without running anything).
pub fn precheck(j: &MsiJournal, f: &dyn Facts) -> Result<Registered, &'static str> {
    if installed_as(f, &j.from, &j.install_dir) != Some(true) {
        return Err("installed_changed");
    }
    f.registered().ok_or("installed_changed")
}

pub fn begin_install(
    update_dir: &Path,
    j: &mut MsiJournal,
    me: Proc,
    lock: &Lock,
) -> io::Result<()> {
    lock.check(update_dir)?;
    let before = j.clone();
    j.phase = Phase::Installing;
    j.helper = Some(me);
    transition(update_dir, &before, j)
}

pub fn record_msiexec(
    update_dir: &Path,
    j: &mut MsiJournal,
    p: Proc,
    lock: &Lock,
) -> io::Result<()> {
    lock.check(update_dir)?;
    let before = j.clone();
    j.msiexec = Some(p);
    transition(update_dir, &before, j)
}

/// Fail the claimed attempt before msiexec ran (`handoff` or `installing`
/// without msiexec).
pub fn fail_before_msiexec(
    update_dir: &Path,
    j: &MsiJournal,
    failure: &'static str,
    detail: &str,
    lock: &Lock,
) -> io::Result<()> {
    lock.check(update_dir)?;
    if j.msiexec.is_some() || !matches!(j.phase, Phase::Handoff | Phase::Installing) {
        return Err(install::journal_uncertain());
    }
    transition(update_dir, j, &failed(j, failure, detail))
}

// ---- the real facts ----

/// This machine's facts.
pub struct SystemFacts;

impl Facts for SystemFacts {
    fn installer_busy(&self) -> Option<bool> {
        sys::installer_busy()
    }
    fn installed_versions(&self) -> Option<Vec<String>> {
        sys::installed_versions()
    }
    fn registered(&self) -> Option<Registered> {
        sys::registered()
    }
    fn running(&self, p: &Proc) -> bool {
        sys::running(p)
    }
    fn msiexec_child_running(&self, parent: &Proc) -> Option<bool> {
        sys::msiexec_child_running(parent)
    }
}

impl Proc {
    pub fn current() -> Proc {
        sys::current()
    }

    /// A child started by this process.
    pub fn of(child: &std::process::Child) -> Proc {
        sys::of(child)
    }
}

#[cfg(windows)]
mod sys {
    use super::{Proc, REG_KEY, Registered, UPGRADE_CODE};
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER,
        ERROR_NO_MORE_ITEMS, ERROR_UNKNOWN_PRODUCT, FILETIME, GetLastError, HANDLE, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::System::ApplicationInstallationAndServicing::{
        MSIINSTALLCONTEXT_USERUNMANAGED, MsiEnumRelatedProductsW, MsiGetProductInfoExW,
    };
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey,
        RegGetValueW, RegOpenKeyExW,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenMutexW, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE,
        WaitForSingleObject,
    };

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn created(h: HANDLE) -> u64 {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut c, mut e, mut k, mut u) = (zero, zero, zero, zero);
        // SAFETY: a valid process handle and four out-parameters.
        if unsafe { GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) } == 0 {
            return 0;
        }
        (u64::from(c.dwHighDateTime) << 32) | u64::from(c.dwLowDateTime)
    }

    pub fn current() -> Proc {
        // SAFETY: the pseudo handle needs no closing.
        let h = unsafe { GetCurrentProcess() };
        Proc {
            pid: std::process::id(),
            created: created(h),
        }
    }

    pub fn of(child: &std::process::Child) -> Proc {
        use std::os::windows::io::AsRawHandle;
        Proc {
            pid: child.id(),
            created: created(child.as_raw_handle() as HANDLE),
        }
    }

    pub fn running(p: &Proc) -> bool {
        // SAFETY: query-only handle, closed below.
        unsafe {
            let h = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                p.pid,
            );
            if h.is_null() {
                // No such process; anything else (access denied) can't be
                // ruled out.
                return GetLastError() != ERROR_INVALID_PARAMETER;
            }
            let alive = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
            let same = p.created == 0 || created(h) == p.created;
            CloseHandle(h);
            alive && same
        }
    }

    pub fn msiexec_child_running(parent: &Proc) -> Option<bool> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        };
        // SAFETY: a snapshot handle, closed below; entries sized as required.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut e = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut found = false;
            let mut ok = Process32FirstW(snap, &mut e) != 0;
            while ok {
                let end = e
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(e.szExeFile.len());
                let name = String::from_utf16_lossy(&e.szExeFile[..end]);
                if e.th32ParentProcessID == parent.pid
                    && name.eq_ignore_ascii_case("msiexec.exe")
                    && started_after(e.th32ProcessID, parent.created)
                {
                    found = true;
                    break;
                }
                ok = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
            Some(found)
        }
    }

    /// Created at or after `t` (or its creation time can't be read).
    fn started_after(pid: u32, t: u64) -> bool {
        // SAFETY: query-only handle, closed below.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return true;
            }
            let c = created(h);
            CloseHandle(h);
            c == 0 || c >= t
        }
    }

    pub fn installer_busy() -> Option<bool> {
        let name = w("Global\\_MSIExecute");
        // SAFETY: a NUL-terminated name; the handle is closed.
        let mutex = unsafe {
            let h = OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr());
            if h.is_null() {
                match GetLastError() {
                    ERROR_FILE_NOT_FOUND => false,
                    ERROR_ACCESS_DENIED => true,
                    _ => return None,
                }
            } else {
                CloseHandle(h);
                true
            }
        };
        // A suspended installation that Windows Installer will roll back.
        let key = w("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Installer\\InProgress");
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: a NUL-terminated name and an out-parameter, closed below.
        let in_progress = unsafe {
            match RegOpenKeyExW(HKEY_LOCAL_MACHINE, key.as_ptr(), 0, KEY_READ, &mut h) {
                0 => {
                    RegCloseKey(h);
                    true
                }
                ERROR_FILE_NOT_FOUND => false,
                _ => return None,
            }
        };
        Some(mutex || in_progress)
    }

    pub fn installed_versions() -> Option<Vec<String>> {
        let code = w(UPGRADE_CODE);
        let prop = w("VersionString");
        let mut out = Vec::new();
        for index in 0..64u32 {
            let mut product = [0u16; 39];
            // SAFETY: a NUL-terminated code and a 39-character buffer.
            let rc =
                unsafe { MsiEnumRelatedProductsW(code.as_ptr(), 0, index, product.as_mut_ptr()) };
            if rc == ERROR_NO_MORE_ITEMS {
                return Some(out);
            }
            if rc != 0 {
                return None;
            }
            let mut buf = [0u16; 64];
            let mut len = buf.len() as u32;
            // SAFETY: the product code from above, a sized buffer.
            let rc = unsafe {
                MsiGetProductInfoExW(
                    product.as_ptr(),
                    std::ptr::null(),
                    MSIINSTALLCONTEXT_USERUNMANAGED,
                    prop.as_ptr(),
                    buf.as_mut_ptr(),
                    &mut len,
                )
            };
            match rc {
                0 => out.push(String::from_utf16_lossy(
                    &buf[..(len as usize).min(buf.len())],
                )),
                // Not installed for this user (another context).
                ERROR_UNKNOWN_PRODUCT => {}
                _ => return None,
            }
        }
        None
    }

    fn read(name: &str) -> Option<String> {
        let (key, value) = (w(REG_KEY), w(name));
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

    pub fn registered() -> Option<Registered> {
        let flag = |name| match read(name).as_deref() {
            Some("1") => Some(true),
            Some("0") => Some(false),
            _ => None,
        };
        Some(Registered {
            dir: PathBuf::from(read("InstallDir")?),
            version: read("Version")?.trim().to_string(),
            local_runner: flag("LocalRunner"),
            start_menu: flag("StartMenuShortcut"),
        })
    }
}

#[cfg(not(windows))]
mod sys {
    //! Not a Windows Installer machine: nothing can be established.
    use super::{Proc, Registered};

    pub fn current() -> Proc {
        Proc {
            pid: std::process::id(),
            created: 0,
        }
    }

    pub fn of(child: &std::process::Child) -> Proc {
        Proc {
            pid: child.id(),
            created: 0,
        }
    }

    pub fn running(p: &Proc) -> bool {
        super::install::pid_alive(p.pid)
    }

    pub fn installer_busy() -> Option<bool> {
        None
    }

    pub fn msiexec_child_running(_: &Proc) -> Option<bool> {
        None
    }

    pub fn installed_versions() -> Option<Vec<String>> {
        None
    }

    pub fn registered() -> Option<Registered> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const A: &str = "1.0.0";
    const B: &str = "1.0.1";

    /// Facts for an install folder on disk plus scripted Windows Installer
    /// answers.
    struct Fake {
        busy: Option<bool>,
        products: Option<Vec<String>>,
        reg: Option<Registered>,
        running: RefCell<Vec<u32>>,
    }

    impl Facts for Fake {
        fn installer_busy(&self) -> Option<bool> {
            self.busy
        }
        fn installed_versions(&self) -> Option<Vec<String>> {
            self.products.clone()
        }
        fn registered(&self) -> Option<Registered> {
            self.reg.clone()
        }
        fn running(&self, p: &Proc) -> bool {
            self.running.borrow().contains(&p.pid)
        }
        fn msiexec_child_running(&self, parent: &Proc) -> Option<bool> {
            Some(self.running.borrow().contains(&(parent.pid + 1000)))
        }
    }

    fn pv(v: &str) -> String {
        product_version(&Version::parse(v).unwrap()).unwrap()
    }

    struct Install {
        _tmp: tempfile::TempDir,
        dir: PathBuf,
    }

    fn install_files(dir: &Path, version: &str, exe: &[u8]) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("Metro.exe"), exe).unwrap();
        let manifest = serde_json::json!({
            "kind": "desktop", "target": "windows-x64", "version": version,
            "files": {"Metro.exe": metro_release::hex(&metro_release::sha256(exe))},
        });
        fs::write(dir.join("RELEASE.json"), manifest.to_string()).unwrap();
    }

    fn installed(version: &str) -> Install {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Metro Desktop");
        install_files(&dir, version, format!("launcher {version}").as_bytes());
        Install { _tmp: tmp, dir }
    }

    fn facts(i: &Install, product: &str, reg_version: &str) -> Fake {
        Fake {
            busy: Some(false),
            products: Some(vec![pv(product)]),
            reg: Some(Registered {
                dir: PathBuf::from(format!("{}\\", i.dir.display())),
                version: reg_version.into(),
                local_runner: Some(true),
                start_menu: Some(false),
            }),
            running: RefCell::new(Vec::new()),
        }
    }

    fn journal(i: &Install, phase: Phase) -> MsiJournal {
        let mut j = MsiJournal {
            version: 3,
            id: "0123456789abcdef".into(),
            from: A.into(),
            to: B.into(),
            phase: Phase::Staged,
            install_dir: i.dir.clone(),
            manifest_sha256: "a".repeat(64),
            msi_sha256: "b".repeat(64),
            local_db_snapshot: false,
            backup_manifest: None,
            helper: None,
            msiexec: None,
            exit_code: None,
            reboot_required: false,
            outcome: None,
            failure: None,
            detail: None,
            started_at: 1,
        };
        if phase != Phase::Staged && phase != Phase::BackingUp {
            j.local_db_snapshot = true;
            j.backup_manifest = Some("c".repeat(64));
        }
        if matches!(phase, Phase::Installing | Phase::Installed) {
            j.helper = Some(Proc {
                pid: 41,
                created: 7,
            });
            j.msiexec = Some(Proc {
                pid: 42,
                created: 8,
            });
        }
        match phase {
            Phase::Failed => {
                j.outcome = Some("install_failed".into());
                j.failure = Some("installer_failed".into());
            }
            Phase::Committed => j.outcome = Some("updated".into()),
            _ => {}
        }
        j.phase = phase;
        assert!(j.valid(), "{phase:?}");
        j
    }

    #[test]
    fn product_versions_match_build_msi() {
        // build_msi.py windows_version(): release rank 99.
        assert_eq!(pv("0.6.5"), "0.6.599");
        assert_eq!(pv("1.0.0"), "1.0.99");
        assert_eq!(pv("2.3.654"), "2.3.65499");
        assert_eq!(product_version(&Version::parse("0.6.655").unwrap()), None);
        assert_eq!(product_version(&Version::parse("256.0.0").unwrap()), None);
    }

    #[test]
    fn journal_validation() {
        let i = installed(A);
        let mut j = journal(&i, Phase::Installing);
        j.helper = None;
        assert!(!j.valid(), "installing needs the helper's identity");
        let mut j = journal(&i, Phase::Handoff);
        j.backup_manifest = None;
        assert!(!j.valid(), "handoff needs the published backup");
        let mut j = journal(&i, Phase::Staged);
        j.from = B.into();
        j.to = A.into();
        assert!(!j.valid(), "no downgrade");
        let mut j = journal(&i, Phase::Failed);
        j.failure = None;
        assert!(!j.valid(), "a failure code is required");
        j.failure = Some("installer_cancelled".into());
        assert!(j.valid());
        j.failure = Some("anything".into());
        assert!(!j.valid());
        let mut j = journal(&i, Phase::Staged);
        j.install_dir = PathBuf::from("relative");
        assert!(!j.valid());
    }

    #[test]
    fn no_journal_or_staged_starts_normally() {
        let i = installed(A);
        let f = facts(&i, A, A);
        assert_eq!(decide(None, A, &f), Decision::Proceed);
        assert_eq!(
            decide(Some(&journal(&i, Phase::Staged)), A, &f),
            Decision::Proceed
        );
    }

    #[test]
    fn a_handoff_without_installer_is_retired_by_the_old_version_only() {
        let i = installed(A);
        let f = facts(&i, A, A);
        for phase in [Phase::BackingUp, Phase::Handoff] {
            let j = journal(&i, phase);
            let Decision::Write(n) = decide(Some(&j), A, &f) else {
                panic!("{phase:?}");
            };
            assert_eq!(n.phase, Phase::Failed);
            assert_eq!(n.failure.as_deref(), Some("helper_not_started"));
            assert!(n.valid());
            assert_eq!(decide(Some(&j), B, &f), Decision::Uncertain);
        }
    }

    #[test]
    fn a_running_helper_installer_or_windows_installer_means_wait() {
        let i = installed(A);
        let j = journal(&i, Phase::Installing);
        let f = facts(&i, A, A);
        f.running.borrow_mut().push(41);
        assert_eq!(decide(Some(&j), A, &f), Decision::Updating);
        f.running.borrow_mut().clear();
        f.running.borrow_mut().push(42);
        assert_eq!(decide(Some(&j), A, &f), Decision::Updating);
        f.running.borrow_mut().clear();
        // The helper died before recording msiexec, which still runs.
        let mut unrecorded = j.clone();
        unrecorded.msiexec = None;
        f.running.borrow_mut().push(41 + 1000);
        assert_eq!(decide(Some(&unrecorded), A, &f), Decision::Updating);
        f.running.borrow_mut().clear();
        let busy = Fake {
            busy: Some(true),
            ..facts(&i, A, A)
        };
        assert_eq!(decide(Some(&j), A, &busy), Decision::Updating);
        let unknown = Fake {
            busy: None,
            ..facts(&i, A, A)
        };
        assert_eq!(decide(Some(&j), A, &unknown), Decision::Uncertain);
    }

    #[test]
    fn helper_killed_during_msiexec_is_decided_from_facts() {
        // The installer finished: the new version continues.
        let i = installed(B);
        let j = journal(&i, Phase::Installing);
        let f = facts(&i, B, B);
        let Decision::Write(n) = decide(Some(&j), B, &f) else {
            panic!()
        };
        assert_eq!(n.phase, Phase::Installed);
        // The old version can't be the one starting after that.
        assert_eq!(decide(Some(&j), A, &f), Decision::Uncertain);

        // The installer rolled back: the old version records the failure.
        let i = installed(A);
        let j = journal(&i, Phase::Installing);
        let f = facts(&i, A, A);
        let Decision::Write(n) = decide(Some(&j), A, &f) else {
            panic!()
        };
        assert_eq!(
            (n.phase, n.failure.as_deref()),
            (Phase::Failed, Some("installer_failed"))
        );
        assert!(n.valid());
    }

    #[test]
    fn facts_that_disagree_stop_safely() {
        let i = installed(A);
        let j = journal(&i, Phase::Installing);
        // Registry says B, product says A.
        assert_eq!(decide(Some(&j), A, &facts(&i, A, B)), Decision::Uncertain);
        // Product missing (removed, new one not installed).
        let none = Fake {
            products: Some(vec![]),
            ..facts(&i, A, A)
        };
        assert_eq!(decide(Some(&j), A, &none), Decision::Uncertain);
        // Two products with the UpgradeCode.
        let two = Fake {
            products: Some(vec![pv(A), pv(B)]),
            ..facts(&i, A, A)
        };
        assert_eq!(decide(Some(&j), A, &two), Decision::Uncertain);
        // MSI API unavailable.
        let api = Fake {
            products: None,
            ..facts(&i, A, A)
        };
        assert_eq!(decide(Some(&j), A, &api), Decision::Uncertain);
        // Another folder registered.
        let mut other = facts(&i, A, A);
        other.reg.as_mut().unwrap().dir = PathBuf::from("C:\\Elsewhere");
        assert_eq!(decide(Some(&j), A, &other), Decision::Uncertain);
        // A changed Metro.exe.
        fs::write(i.dir.join("Metro.exe"), b"changed").unwrap();
        assert_eq!(decide(Some(&j), A, &facts(&i, A, A)), Decision::Uncertain);
    }

    #[test]
    fn files_waiting_for_a_windows_restart() {
        let i = installed(A);
        let j = journal(&i, Phase::Installing);
        let f = facts(&i, B, B);
        assert_eq!(decide(Some(&j), A, &f), Decision::RestartWindows);
        let n = after_msiexec(&j, Some(3010), &f).unwrap();
        assert!(n.reboot_required && n.phase == Phase::Installed);
        assert_eq!(decide(Some(&n), A, &f), Decision::RestartWindows);
        // After the restart the files are B.
        install_files(&i.dir, B, b"launcher 1.0.1");
        assert_eq!(decide(Some(&n), B, &f), Decision::Proceed);
        // Exit 0 must mean the files are replaced.
        install_files(&i.dir, A, b"launcher 1.0.0");
        assert_eq!(after_msiexec(&j, Some(0), &f), None);
    }

    #[test]
    fn msiexec_codes() {
        let i = installed(A);
        let j = journal(&i, Phase::Installing);
        let old = facts(&i, A, A);
        for (code, failure) in [
            (Some(1602), "installer_cancelled"),
            (Some(1618), "installer_busy"),
            (Some(1603), "installer_failed"),
            (None, "installer_failed"),
        ] {
            let n = after_msiexec(&j, code, &old).unwrap();
            assert_eq!(n.phase, Phase::Failed);
            assert_eq!(n.failure.as_deref(), Some(failure));
            assert_eq!(n.exit_code, code);
            assert!(n.valid());
            // Then the old version starts normally and shows it.
            assert_eq!(decide(Some(&n), A, &old), Decision::Proceed);
        }
        // A failure code while B is installed doesn't add up.
        let i = installed(B);
        let j = journal(&i, Phase::Installing);
        assert_eq!(after_msiexec(&j, Some(1603), &facts(&i, B, B)), None);
        let n = after_msiexec(&j, Some(0), &facts(&i, B, B)).unwrap();
        assert_eq!((n.phase, n.reboot_required), (Phase::Installed, false));
        // Windows Installer still busy: decide later.
        let busy = Fake {
            busy: Some(true),
            ..facts(&i, B, B)
        };
        assert_eq!(after_msiexec(&j, Some(0), &busy), None);
        // ...unless it refused ours because of another installation.
        let i = installed(A);
        let j = journal(&i, Phase::Installing);
        let other = Fake {
            busy: Some(true),
            ..facts(&i, A, A)
        };
        let n = after_msiexec(&j, Some(1618), &other).unwrap();
        assert_eq!(n.failure.as_deref(), Some("installer_busy"));
    }

    #[test]
    fn installed_then_committed() {
        let i = installed(B);
        let f = facts(&i, B, B);
        let j = journal(&i, Phase::Installed);
        assert_eq!(decide(Some(&j), B, &f), Decision::Proceed);
        // The old version starting against an installed update.
        assert_eq!(decide(Some(&j), A, &f), Decision::Uncertain);
        let c = journal(&i, Phase::Committed);
        assert_eq!(decide(Some(&c), B, &f), Decision::Proceed);
        assert_eq!(decide(Some(&c), A, &f), Decision::Uncertain);
    }

    #[test]
    fn command_line() {
        let reg = Registered {
            dir: PathBuf::from("C:\\Users\\a b\\Metro Desktop\\"),
            version: A.into(),
            local_runner: Some(false),
            start_menu: Some(true),
        };
        let line = msiexec_command_line(
            Path::new(
                "C:\\Users\\a b\\AppData\\Local\\Metro\\Desktop\\update\\stage-x\\package.msi",
            ),
            Path::new("C:\\l\\msiexec.log"),
            &reg,
        )
        .unwrap();
        assert_eq!(
            line,
            "/i \"C:\\Users\\a b\\AppData\\Local\\Metro\\Desktop\\update\\stage-x\\package.msi\" \
             /passive /norestart REBOOT=ReallySuppress INSTALLDIR=\"C:\\Users\\a b\\Metro Desktop\" \
             LOCALRUNNER=0 STARTMENUSHORTCUT=1 /l*v \"C:\\l\\msiexec.log\""
        );
        let none = Registered {
            local_runner: None,
            start_menu: None,
            ..reg.clone()
        };
        let line = msiexec_command_line(Path::new("C:\\p.msi"), Path::new("C:\\l"), &none).unwrap();
        assert!(!line.contains("LOCALRUNNER") && !line.contains("STARTMENUSHORTCUT"));
        let bad = Registered {
            dir: PathBuf::from("C:\\a\"b"),
            ..reg
        };
        assert!(msiexec_command_line(Path::new("C:\\p.msi"), Path::new("C:\\l"), &bad).is_err());
    }

    #[test]
    fn precheck_needs_the_old_version_installed_where_recorded() {
        let i = installed(A);
        let j = journal(&i, Phase::Handoff);
        assert!(precheck(&j, &facts(&i, A, A)).is_ok());
        assert_eq!(precheck(&j, &facts(&i, B, B)), Err("installed_changed"));
    }

    #[test]
    fn files_version_checks_the_launcher() {
        let i = installed(A);
        assert_eq!(files_version(&i.dir).as_deref(), Some(A));
        fs::write(i.dir.join("Metro.exe"), b"other").unwrap();
        assert_eq!(files_version(&i.dir), None);
        fs::remove_file(i.dir.join("RELEASE.json")).unwrap();
        assert_eq!(files_version(&i.dir), None);
    }

    /// The real facts answer on Windows (no Metro product in a test profile).
    #[cfg(windows)]
    #[test]
    fn system_facts_answer() {
        let f = SystemFacts;
        assert!(f.installer_busy().is_some());
        assert!(f.installed_versions().is_some());
        let me = Proc::current();
        assert!(me.created != 0);
        assert!(f.running(&me));
        assert_eq!(f.msiexec_child_running(&me), Some(false));
        assert!(!f.running(&Proc {
            pid: me.pid,
            created: me.created + 1
        }));
        let child = std::process::Command::new("cmd.exe")
            .args(["/c", "exit", "3"])
            .spawn();
        if let Ok(mut child) = child {
            let p = Proc::of(&child);
            let _ = child.wait();
            assert!(!f.running(&p));
        }
    }

    /// With Metro `$METRO_MSI_TEST_INSTALLED` installed per-user from a
    /// release-identity MSI (a test machine or Wine prefix), the facts read
    /// it back: product version through the MSI API, registry, files.
    #[cfg(windows)]
    #[test]
    fn system_facts_read_an_installed_product() {
        let Ok(version) = std::env::var("METRO_MSI_TEST_INSTALLED") else {
            return;
        };
        let f = SystemFacts;
        let pv = product_version(&Version::parse(&version).unwrap()).unwrap();
        assert_eq!(f.installed_versions(), Some(vec![pv]));
        let reg = f.registered().expect("remembered values");
        assert_eq!(reg.version, version);
        assert_eq!(f.installer_busy(), Some(false));
        assert_eq!(f.files_version(&reg.dir).as_deref(), Some(version.as_str()));
    }

    #[test]
    fn failure_codes_for_last_update() {
        assert_eq!(
            failure_code(Some("installer_busy")),
            InstallFailure::InstallerBusy
        );
        assert_eq!(
            failure_code(Some("installer_cancelled")),
            InstallFailure::InstallerCancelled
        );
        assert_eq!(
            failure_code(Some("not_verified")),
            InstallFailure::StageVerificationFailed
        );
        assert_eq!(
            failure_code(Some("helper_not_started")),
            InstallFailure::InstallerFailed
        );
    }
}
