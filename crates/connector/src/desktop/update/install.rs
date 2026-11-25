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
use std::io;
use std::path::{Path, PathBuf};

use metro_release::archive::{Expect, extract_tar_gz};
use metro_release::{Channel, Version};
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
        let raw = fs::read_to_string(root.join("current.txt")).ok()?;
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
    if cfg!(windows) {
        write_atomic(&root.join("current.txt"), version.as_bytes())
    } else {
        #[cfg(unix)]
        {
            let tmp = root.join(".current.new");
            let _ = fs::remove_file(&tmp);
            std::os::unix::fs::symlink(Path::new("versions").join(version), &tmp)?;
            fs::rename(&tmp, root.join("current"))?;
            if let Ok(d) = fs::File::open(root) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }
}

/// Write a file atomically (temp, fsync, rename).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

// ---- the journal ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    pub version: u32,
    pub id: String,
    pub from: String,
    pub to: String,
    /// `staged`, `trial`, `committing`, `committed`, `failed`.
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default)]
    pub local_db_snapshot: bool,
    /// MSI: the verified MSI and the helper copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msi: Option<PathBuf>,
    pub started_at: i64,
}

pub fn read_journal(path: &Path) -> Option<Journal> {
    let raw = fs::read(path).ok()?;
    if raw.len() > 64 * 1024 {
        return None;
    }
    serde_json::from_slice(&raw).ok()
}

pub fn write_journal(path: &Path, j: &Journal) -> io::Result<()> {
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

fn new_id() -> String {
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
    let path = journal_path(kind, update_dir)?;
    let j = read_journal(&path)?;
    if !matches!(j.phase.as_str(), "committed" | "failed") {
        return None;
    }
    let _ = fs::remove_file(&path);
    let _ = fs::remove_dir_all(update_dir.join("db-snapshot"));
    if let Some(root) = kind.root() {
        cleanup_versions(root, own);
    }
    Some(LastUpdate {
        from: j.from,
        to: j.to,
        outcome: j.outcome.unwrap_or_else(|| {
            if j.phase == "committed" {
                "updated".into()
            } else {
                "rolled_back".into()
            }
        }),
        at: now_ms(),
        detail: j.detail,
    })
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
            root.join("versions").join(&j.to).join(LAUNCHER).is_file()
        }
        InstallKind::WindowsMsi { .. } => j.msi.as_ref().is_some_and(|m| m.is_file()),
        _ => false,
    };
    present.then_some((v, j.id))
}

/// Remove every `versions/*` except `keep`, and leftover `.stage.*`/`.old.*`.
pub fn cleanup_versions(root: &Path, keep: &Version) {
    let keep = keep.to_string();
    let pointer = read_pointer(root);
    if let Ok(entries) = fs::read_dir(root.join("versions")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name != keep && Some(&name) != pointer.as_ref() {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    if let Ok(entries) = fs::read_dir(root) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(".stage.") || name.starts_with(".old.") {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
}

// ---- the installers' lock ----

pub struct Lock(PathBuf);

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
    /// `mkdir <root>/.lock` with an owner file; a lock this app left behind
    /// (dead pid, > 10 minutes) is taken over, any other means busy.
    pub fn take(root: &Path) -> Result<Lock, StageError> {
        let path = root.join(LOCK);
        for _ in 0..2 {
            match fs::create_dir(&path) {
                Ok(()) => {
                    let owner = serde_json::json!({"pid": std::process::id(), "started_at": now_ms(), "by": "metro-desktop"});
                    let _ = fs::write(path.join("owner"), owner.to_string());
                    return Ok(Lock(path));
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let stale = fs::read(path.join("owner"))
                        .ok()
                        .and_then(|r| serde_json::from_slice::<serde_json::Value>(&r).ok())
                        .is_some_and(|v| {
                            v["by"] == "metro-desktop"
                                && now_ms() - v["started_at"].as_i64().unwrap_or(i64::MAX)
                                    > 10 * 60 * 1000
                                && !pid_alive(v["pid"].as_u64().unwrap_or(0) as u32)
                        });
                    if !stale {
                        return Err(stage_err("installer_busy"));
                    }
                    let _ = fs::remove_dir_all(&path);
                }
                Err(_) => return Err(stage_err("io")),
            }
        }
        Err(stage_err("installer_busy"))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks existence.
        pid != 0 && pid <= i32::MAX as u32 && unsafe { libc::kill(pid as i32, 0) } == 0
    }
    #[cfg(not(unix))]
    {
        pid != 0 && pid == std::process::id()
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
    target: &Version,
    download: &Path,
    ldd: &Path,
) -> Result<String, StageError> {
    let id = new_id();
    let journal = Journal {
        version: 1,
        id: id.clone(),
        from: own.to_string(),
        to: target.to_string(),
        phase: "staged".into(),
        outcome: None,
        detail: None,
        local_db_snapshot: false,
        msi: None,
        started_at: now_ms(),
    };
    match kind {
        InstallKind::LinuxInstaller { root } | InstallKind::WindowsScript { root } => {
            let lock = Lock::take(root)?;
            let stage = root.join(format!(".stage.{id}"));
            fs::create_dir(&stage).map_err(|_| stage_err("io"))?;
            let result = (|| {
                let name = download
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                let top = metro_release::manifest::archive_top(name)
                    .unwrap_or_default()
                    .to_string();
                let target_s = if matches!(kind, InstallKind::WindowsScript { .. }) {
                    "windows-x64"
                } else {
                    "linux-x64"
                };
                let expect = Expect {
                    kind: "desktop",
                    target: target_s,
                    version: target,
                };
                let to_err = |e: metro_release::Error| StageError {
                    code: if e.code == metro_release::ErrorCode::NotVerified {
                        "not_verified"
                    } else {
                        "archive"
                    },
                    message: super::message_for("archive").into(),
                };
                if name.ends_with(".zip") {
                    let mut f = fs::File::open(download).map_err(|_| stage_err("io"))?;
                    metro_release::archive::extract_zip(&mut f, &top, &stage, &expect)
                        .map_err(to_err)?;
                } else {
                    let f = fs::File::open(download).map_err(|_| stage_err("io"))?;
                    extract_tar_gz(io::BufReader::new(f), &top, &stage, &expect).map_err(to_err)?;
                }
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
                    fs::write(stage.join(LOCAL_RUNNER_MARKER), b"").map_err(|_| stage_err("io"))?;
                }
                let dest = root.join("versions").join(target.to_string());
                if fs::symlink_metadata(&dest).is_ok() {
                    if read_pointer(root).as_deref() == Some(target.to_string().as_str()) {
                        return Err(stage_err("installer_busy"));
                    }
                    let old = root.join(format!(".old.{id}"));
                    fs::rename(&dest, &old).map_err(|_| stage_err("io"))?;
                    let _ = fs::remove_dir_all(&old);
                }
                fs::rename(&stage, &dest).map_err(|_| stage_err("io"))?;
                write_journal(&root.join(JOURNAL), &journal).map_err(|_| stage_err("io"))?;
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_dir_all(&stage);
            }
            drop(lock);
            result.map(|_| id)
        }
        InstallKind::WindowsMsi { .. } => {
            // Windows can't replace a running EXE: a helper copy outside the
            // install folder runs msiexec after this process exits.
            let helper = update_dir.join(format!("helper-{id}"));
            fs::create_dir_all(&helper).map_err(|_| stage_err("io"))?;
            let exe = std::env::current_exe().map_err(|_| stage_err("io"))?;
            fs::copy(&exe, helper.join("metro-update-helper.exe")).map_err(|_| stage_err("io"))?;
            let j = Journal {
                msi: Some(download.to_path_buf()),
                ..journal
            };
            write_journal(&update_dir.join(MSI_JOURNAL), &j).map_err(|_| stage_err("io"))?;
            Ok(id)
        }
        _ => Err(stage_err("not_in_app")),
    }
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
    if id.is_empty() || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    write_atomic(&update_dir.join(format!("trial-{id}.ok")), b"ok")
}

pub fn trial_ok(update_dir: &Path, id: &str) -> bool {
    update_dir.join(format!("trial-{id}.ok")).is_file()
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
    let _ = fs::remove_dir_all(&snap);
    let files = db_files(desktop_root);
    if !files[0].is_file() {
        return Ok(false);
    }
    fs::create_dir_all(&snap)?;
    for f in files.iter().filter(|f| f.is_file()) {
        fs::copy(f, snap.join(f.file_name().unwrap_or_default()))?;
    }
    if let Ok(d) = fs::File::open(&snap) {
        let _ = d.sync_all();
    }
    Ok(true)
}

/// Put the snapshot back byte for byte (removing a newer WAL/SHM).
pub fn restore_db(desktop_root: &Path, update_dir: &Path) -> io::Result<()> {
    let snap = update_dir.join("db-snapshot");
    if !snap.join("metro.sqlite3").is_file() {
        return Ok(());
    }
    for f in db_files(desktop_root) {
        let saved = snap.join(f.file_name().unwrap_or_default());
        if saved.is_file() {
            let tmp = f.with_extension("restore");
            fs::copy(&saved, &tmp)?;
            fs::rename(&tmp, &f)?;
        } else {
            let _ = fs::remove_file(&f);
        }
    }
    Ok(())
}

// ---- commit and rollback (script installs) ----

/// Move the pointer (and the menu entry / shortcut) to `j.to`. Idempotent.
pub fn commit(root: &Path, j: &mut Journal) -> io::Result<()> {
    let path = root.join(JOURNAL);
    j.phase = "committing".into();
    write_journal(&path, j)?;
    set_pointer(root, &j.to)?;
    move_launch_entry(root, &j.from, &j.to);
    j.phase = "committed".into();
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
    outcome: &str,
    detail: Option<String>,
) -> io::Result<()> {
    if read_pointer(root).as_deref() != Some(j.from.as_str()) {
        set_pointer(root, &j.from)?;
    }
    if j.local_db_snapshot {
        restore_db(desktop_root, update_dir)?;
    }
    let _ = fs::remove_dir_all(root.join("versions").join(&j.to));
    j.phase = "failed".into();
    j.outcome = Some(outcome.into());
    j.detail = detail;
    write_journal(&root.join(JOURNAL), j)
}

/// What a normal (non-trial) start does with an unfinished journal.
#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Nothing,
    /// Finished a commit the previous run started.
    Committed,
    /// Rolled back an interrupted update.
    RolledBack,
}

pub fn recover(root: &Path, desktop_root: &Path, update_dir: &Path, own: &Version) -> Recovery {
    let path = root.join(JOURNAL);
    let Some(mut j) = read_journal(&path) else {
        return Recovery::Nothing;
    };
    let own = own.to_string();
    match j.phase.as_str() {
        "committing" if own == j.to => {
            if commit(root, &mut j).is_ok() {
                Recovery::Committed
            } else {
                Recovery::Nothing
            }
        }
        "committing" | "trial" if own == j.from => {
            // The watchdog is gone (we are a fresh start of the old version).
            let _ = rollback(
                root,
                desktop_root,
                update_dir,
                &mut j,
                "rolled_back",
                Some("interrupted".into()),
            );
            Recovery::RolledBack
        }
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
