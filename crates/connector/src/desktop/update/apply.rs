//! Restarting into a staged update: the trial start, the watchdog and its
//! decision. The launcher (`crates/desktop`) calls these after it closed the
//! UI, stopped the bridge and every worker, and dropped its locks.
//!
//! 1. Snapshot the local database (the bundled server is stopped).
//! 2. Journal `trial`, start `versions/<V>/Metro --update-trial <id>` in its
//!    own process group (Windows: detached, its own group, out of any job).
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

/// Snapshot the DB and journal `trial` (before the trial starts).
pub fn begin_trial(p: &TrialPaths, j: &mut Journal, lock: &install::Lock) -> std::io::Result<()> {
    lock.check(&p.root)?;
    install::ensure_script_transition(&p.root, j, &["staged"])?;
    j.local_db_snapshot = install::snapshot_db(&p.desktop_root, &p.update_dir)?;
    j.phase = install::JournalPhase::Trial;
    j.watchdog_pid = Some(std::process::id());
    install::write_journal(&p.root.join(install::JOURNAL), j)
}

/// Start the new launcher for its trial.
pub fn spawn_trial(exe: &Path, id: &str) -> std::io::Result<tokio::process::Child> {
    spawn(exe, exe, id)
}

/// On Linux, execute the exact verified inode; on Windows the verified
/// executable's handle excludes changes and deletion through spawn.
pub fn spawn_checked_trial(
    exe: install::CheckedFile,
    id: &str,
) -> std::io::Result<tokio::process::Child> {
    #[cfg(target_os = "linux")]
    let path = {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/proc/self/fd/{}", exe.file.as_raw_fd()))
    };
    #[cfg(not(target_os = "linux"))]
    let path = exe.path.clone();
    let child = spawn(&path, &exe.path, id);
    drop(exe);
    child
}

fn spawn(exe: &Path, installed: &Path, id: &str) -> std::io::Result<tokio::process::Child> {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("--update-trial")
        .arg(id)
        .stdin(std::process::Stdio::null())
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
    child: &mut tokio::process::Child,
    timeout: Duration,
) -> TrialOutcome {
    let started = tokio::time::Instant::now();
    loop {
        if install::trial_ok(&p.update_dir, &j.id) {
            return match install::commit(&p.root, j, lock) {
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
                    let restored = install::rollback(
                        &p.root,
                        &p.desktop_root,
                        &p.update_dir,
                        j,
                        lock,
                        "not_confirmed",
                        None,
                    );
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
                let restored = install::rollback(
                    &p.root,
                    &p.desktop_root,
                    &p.update_dir,
                    j,
                    lock,
                    "rolled_back",
                    Some(detail.clone()),
                );
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
            let restored = install::rollback(
                &p.root,
                &p.desktop_root,
                &p.update_dir,
                j,
                lock,
                "rolled_back",
                Some(detail.clone()),
            );
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
