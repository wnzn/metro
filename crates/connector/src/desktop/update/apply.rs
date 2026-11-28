//! Restarting into a staged update: the trial start, the watchdog and its
//! decision. The launcher (`crates/desktop`) calls these after it closed the
//! UI and stopped the bridge and every worker. The existing instance lock
//! stays held by the watchdog and is handed to the trial without a gap.
//!
//! 1. Snapshot the local database (the bundled server is stopped).
//! 2. Journal `trial`, start `versions/<V>/Metro --update-trial <id>` in its
//!    own process group (Windows: detached, its own group, out of any job).
//!    Linux hands the trial the locked instance-lock descriptor; Windows
//!    locks can't change process, so the launcher holds the handoff gate
//!    and the instance lock until the trial takes it after the commit
//!    (`store::Handoff`).
//! 3. Watch: the new UI's first render writes `trial-<id>.ok` through the
//!    bridge (`POST /v1/update/started`) → **commit** (pointer, menu entry);
//!    the watchdog exits and the new version keeps running.
//!    The trial exits non-zero, crashes, or 120 s pass → kill it, restore
//!    the snapshot, remove the new version, journal `failed`; the old
//!    version starts again. The trial exits 0 without `.ok` (closed at
//!    once) → nothing is committed; the old version stays current.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::install::{self, Journal};
use crate::desktop::store::InstanceLock;

pub const TRIAL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, PartialEq, Eq)]
pub enum TrialOutcome {
    Committed,
    NotConfirmed,
    RolledBack(String),
    NeedsRecovery,
}

/// Paths the watchdog needs.
pub struct TrialPaths {
    pub root: PathBuf,
    pub desktop_root: PathBuf,
    pub update_dir: PathBuf,
}

const ROOT_GATE: &str = "desktop-update.json";

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RootGate {
    root: PathBuf,
    id: String,
}

impl RootGate {
    pub fn read(desktop_root: &Path) -> std::io::Result<Option<Self>> {
        super::read_json(&desktop_root.join(ROOT_GATE), 16 * 1024)
    }

    pub fn matches(&self, root: &Path, id: &str) -> bool {
        install::valid_id(id)
            && id == self.id
            && root.canonicalize().is_ok_and(|path| path == self.root)
    }

    fn publish(p: &TrialPaths, j: &Journal, instance: &InstanceLock) -> std::io::Result<()> {
        instance.check(&p.desktop_root)?;
        if Self::read(&p.desktop_root)?.is_some() {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        let gate = Self {
            root: p.root.canonicalize()?,
            id: j.id.clone(),
        };
        install::write_atomic(
            &p.desktop_root.join(ROOT_GATE),
            &serde_json::to_vec(&gate).map_err(std::io::Error::other)?,
        )
    }

    pub fn retire(
        desktop_root: &Path,
        root: &Path,
        id: &str,
        instance: &InstanceLock,
    ) -> std::io::Result<()> {
        instance.check(desktop_root)?;
        if !Self::read(desktop_root)?.is_some_and(|gate| gate.matches(root, id)) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let path = desktop_root.join(ROOT_GATE);
        std::fs::remove_file(&path)?;
        metro_release::sync_parent(&path)
    }

    pub(crate) fn check(
        p: &TrialPaths,
        j: &Journal,
        instance: &InstanceLock,
    ) -> std::io::Result<()> {
        instance.check(&p.desktop_root)?;
        if !Self::read(&p.desktop_root)?.is_some_and(|gate| gate.matches(&p.root, &j.id)) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        Ok(())
    }
}

/// Snapshot the DB and journal `trial` (before the trial starts).
pub fn begin_trial(
    p: &TrialPaths,
    j: &mut Journal,
    lock: &install::Lock,
    instance: &InstanceLock,
) -> std::io::Result<()> {
    lock.check(&p.root)?;
    install::ensure_script_transition(&p.root, j, &["staged"])?;
    RootGate::publish(p, j, instance)?;
    let _writer = crate::desktop_local::update_writer_exclusion(&p.desktop_root)?;
    install::begin_backup(&p.root, j, lock)?;
    let (has_db, manifest) = install::snapshot_db(&p.desktop_root, &p.update_dir, j)?;
    j.local_db_snapshot = has_db;
    j.backup_manifest = Some(manifest);
    j.phase = install::JournalPhase::Trial;
    j.watchdog_pid = Some(std::process::id());
    install::write_journal(&p.root.join(install::JOURNAL), j)
}

/// Start the new launcher for its trial.
pub fn spawn_trial(exe: &Path, id: &str) -> std::io::Result<tokio::process::Child> {
    spawn(exe, exe, id, std::process::Stdio::null())
}

/// On Linux, execute the exact verified inode; on Windows the verified
/// executable's handle excludes changes and deletion through spawn.
pub fn spawn_checked_trial(
    exe: install::CheckedFile,
    id: &str,
    instance: &InstanceLock,
) -> std::io::Result<tokio::process::Child> {
    #[cfg(target_os = "linux")]
    let path = {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/proc/self/fd/{}", exe.file.as_raw_fd()))
    };
    #[cfg(target_os = "linux")]
    return spawn(&path, &exe.path, id, instance.trial_input()?);
    // The verified handle (read only, shared for reading) and the pinned
    // folders are held until the process exists; the trial takes over the
    // instance lock through the handoff gate (`store::Handoff`).
    #[cfg(windows)]
    {
        let _ = instance;
        let child = spawn(&exe.path, &exe.path, id, std::process::Stdio::null());
        drop(exe);
        child
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (exe, id, instance);
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

fn spawn(
    exe: &Path,
    installed: &Path,
    id: &str,
    input: std::process::Stdio,
) -> std::io::Result<tokio::process::Child> {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("--update-trial")
        .arg(id)
        .stdin(input)
        .kill_on_drop(false);
    if let Some(dir) = installed.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if let Ok(c) = cmd.spawn() {
            return Ok(c);
        }
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
}

async fn kill(child: &mut tokio::process::Child) -> bool {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // SAFETY: signalling the trial's own process group.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        if matches!(
            tokio::time::timeout(Duration::from_secs(5), child.wait()).await,
            Ok(Ok(_))
        ) {
            return true;
        }
        // SAFETY: as above.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
    child.wait().await.is_ok()
}

/// Watch the trial and decide (script installs).
pub async fn watch_trial(
    p: &TrialPaths,
    j: &mut Journal,
    lock: &install::Lock,
    instance: &InstanceLock,
    child: &mut tokio::process::Child,
    timeout: Duration,
) -> TrialOutcome {
    let started = tokio::time::Instant::now();
    loop {
        if install::trial_ok(&p.update_dir, &j.id) {
            return match RootGate::check(p, j, instance)
                .and_then(|()| install::commit(&p.root, j, lock))
            {
                Ok(()) => TrialOutcome::Committed,
                Err(e) => {
                    // The new version runs and is healthy; a failed pointer
                    // write is finished (or undone) by the next start.
                    eprintln!("metro-desktop: committing the update failed: {}", e.kind());
                    TrialOutcome::NeedsRecovery
                }
            };
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if install::trial_ok(&p.update_dir, &j.id) {
                    continue;
                }
                if status.success() {
                    // Closed before its first render: nothing is committed,
                    // and whatever the new server wrote is undone too.
                    let restored = install::rollback(p, j, lock, instance, "not_confirmed", None);
                    return if restored.is_ok() {
                        TrialOutcome::NotConfirmed
                    } else {
                        TrialOutcome::NeedsRecovery
                    };
                }
                let detail = format!(
                    "exited with {}",
                    status.code().map_or("a signal".into(), |c| c.to_string())
                );
                let restored =
                    install::rollback(p, j, lock, instance, "rolled_back", Some(detail.clone()));
                return if restored.is_ok() {
                    TrialOutcome::RolledBack(detail)
                } else {
                    TrialOutcome::NeedsRecovery
                };
            }
            Ok(None) => {}
            Err(_) => {}
        }
        if started.elapsed() > timeout {
            if !kill(child).await {
                return TrialOutcome::NeedsRecovery;
            }
            let detail = "didn't start in time".to_string();
            let restored =
                install::rollback(p, j, lock, instance, "rolled_back", Some(detail.clone()));
            return if restored.is_ok() {
                TrialOutcome::RolledBack(detail)
            } else {
                TrialOutcome::NeedsRecovery
            };
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The new launcher of a script install.
pub fn trial_exe(root: &Path, version: &str) -> PathBuf {
    root.join("versions").join(version).join(install::LAUNCHER)
}
