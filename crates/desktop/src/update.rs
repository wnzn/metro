//! The launcher's part of an in-app update (see
//! `metro_connector::desktop::update` for the check, download and stage).
//!
//! - `--update-trial <id>`: this start tries a staged update; the bridge's
//!   `POST /v1/update/started` confirms it.
//! - `--update-helper <id>` (Windows MSI): a copy of the launcher outside the
//!   install folder that waits for the old app to exit, runs
//!   `msiexec /i <verified MSI> /passive /norestart`, then starts and watches
//!   the new version. There is no automatic downgrade after a successful
//!   MSI install (the MSI blocks downgrades); the UI says so up front.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use metro_connector::desktop::update::apply::{self, TrialOutcome, TrialPaths};
use metro_connector::desktop::update::install::{self, InstallKind, JournalPhase};
use metro_connector::desktop::update::{ApplyRequest, Updater};

/// How this process was started.
#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Trial(String),
    Helper(String),
}

fn valid_id(id: &str) -> bool {
    install::valid_id(id)
}

/// Parse the launcher's arguments (none, or one update mode).
pub fn mode() -> Result<Mode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => Ok(Mode::Normal),
        [flag, id] if flag == "--update-trial" && valid_id(id) => Ok(Mode::Trial(id.clone())),
        [flag, id] if flag == "--update-helper" && valid_id(id) => Ok(Mode::Helper(id.clone())),
        // Desktop environments may pass file or URL arguments; ignore them.
        _ => Ok(Mode::Normal),
    }
}

/// What the launcher does after an apply.
pub enum After {
    /// This process ends (the new version runs, or the helper took over).
    Exit(u8),
    /// The update was rolled back: start the old version again in-process.
    Relaunch,
}

/// Restart into the staged update. Called with the UI closed, the bridge
/// and every worker stopped; `lock` (the instance lock) is dropped here
/// before the new version starts.
pub async fn apply<L>(
    updater: &Arc<Updater>,
    req: ApplyRequest,
    desktop_root: &Path,
    lock: L,
) -> After {
    let update_dir = updater.dir().to_path_buf();
    let kind = updater.kind().clone();
    let record_failure = |detail: &str| {
        updater.record_outcome(metro_connector::desktop::update::LastUpdate {
            from: updater.own().to_string(),
            to: req.version.to_string(),
            outcome: "install_failed".into(),
            at: metro_connector::desktop::update::now_ms(),
            detail: Some(detail.into()),
        });
    };
    let install_lock = match install::Lock::take(install::lock_root(&kind, &update_dir)) {
        Ok(lock) => lock,
        Err(_) => {
            record_failure("The installer is busy. Close the other installer and try again.");
            return After::Relaunch;
        }
    };
    let (mut j, verified) = match updater.verify_apply(&req, &install_lock) {
        Ok(stage) => stage,
        Err(e) => {
            eprintln!("metro-desktop: the staged update wasn't used: {e}");
            record_failure(
                "The staged update could not be verified. Check again before restarting.",
            );
            return After::Relaunch;
        }
    };
    match kind {
        InstallKind::LinuxInstaller { root } | InstallKind::WindowsScript { root } => {
            let paths = TrialPaths {
                root: root.clone(),
                desktop_root: desktop_root.to_path_buf(),
                update_dir,
            };
            if let Err(e) = apply::begin_trial(&paths, &mut j, &install_lock) {
                eprintln!("metro-desktop: preparing the update failed: {}", e.kind());
                record_failure(
                    "The local workspace could not be backed up. The update was not started.",
                );
                return After::Relaunch;
            }
            drop(lock);
            let mut child = match apply::spawn_checked_trial(verified, &j.id) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "metro-desktop: starting Metro {} failed: {}",
                        j.to,
                        e.kind()
                    );
                    if install::rollback(
                        &root,
                        desktop_root,
                        &paths.update_dir,
                        &mut j,
                        &install_lock,
                        "rolled_back",
                        Some("didn't start".into()),
                    )
                    .is_ok()
                    {
                        return After::Relaunch;
                    }
                    crate::fatal(
                        "Metro could not restore the local workspace yet. The backup was kept; start Metro again to retry.",
                    );
                    return After::Exit(1);
                }
            };
            match apply::watch_trial(
                &paths,
                &mut j,
                &install_lock,
                &mut child,
                apply::TRIAL_TIMEOUT,
            )
            .await
            {
                TrialOutcome::Committed => {
                    eprintln!("metro-desktop: updated to {}", j.to);
                    After::Exit(0)
                }
                TrialOutcome::NotConfirmed => After::Exit(0),
                TrialOutcome::NeedsRecovery => {
                    crate::fatal(
                        "Metro did not finish the update. The backup and update record were kept; restart Metro to recover.",
                    );
                    After::Exit(1)
                }
                TrialOutcome::RolledBack(detail) => {
                    eprintln!("metro-desktop: Metro {} {detail}; back on {}", j.to, j.from);
                    After::Relaunch
                }
            }
        }
        InstallKind::WindowsMsi { .. } => {
            let jpath = update_dir.join(install::MSI_JOURNAL);
            let prepared = std::env::current_exe()
                .and_then(|exe| install::prepare_msi_helper(&update_dir, &j, &exe, &install_lock));
            let helper = match prepared {
                Ok(helper) => helper,
                Err(_) => {
                    record_failure("The update helper could not be verified.");
                    return After::Relaunch;
                }
            };
            j.local_db_snapshot = match install::snapshot_db(desktop_root, &update_dir) {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    record_failure(
                        "The local workspace could not be backed up. The update was not started.",
                    );
                    return After::Relaunch;
                }
            };
            j.phase = JournalPhase::Installing;
            j.watchdog_pid = Some(std::process::id());
            j.source_pid = Some(std::process::id());
            if install::write_journal(&jpath, &j).is_err() {
                record_failure("The update record could not be saved. The update was not started.");
                return After::Relaunch;
            }
            let mut cmd = std::process::Command::new(&helper.path);
            cmd.arg("--update-helper")
                .arg(&j.id)
                .stdin(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
            }
            let started = cmd.spawn();
            drop(helper);
            drop(verified);
            match started {
                Ok(mut child) => {
                    j.watchdog_pid = Some(child.id());
                    if install::write_journal(&jpath, &j).is_err() {
                        let _ = child.kill();
                        let _ = child.wait();
                        record_failure("The update helper could not take over.");
                        return After::Relaunch;
                    }
                    drop(lock);
                    // The helper takes the same installer lock before it reads
                    // the journal or touches the installer.
                    After::Exit(0)
                }
                Err(_) => {
                    j.phase = JournalPhase::Failed;
                    j.outcome = Some("install_failed".into());
                    j.detail = Some("the update helper didn't start".into());
                    if install::write_journal(&jpath, &j).is_err() {
                        return After::Exit(1);
                    }
                    After::Relaunch
                }
            }
        }
        _ => After::Relaunch,
    }
}

/// Before taking the instance lock: while a trial of an update runs under
/// its watchdog, wait for it (it takes the lock the watchdog just let go),
/// instead of racing it for the lock.
pub async fn wait_for_trial(install_dir: &Path, desktop_root: &Path) {
    let script = install_dir
        .parent()
        .and_then(Path::parent)
        .filter(|root| root.join(install::MARKER).is_file());
    let update_dir = desktop_root.join("update");
    let started = std::time::Instant::now();
    while (script.is_some_and(install::trial_in_progress) || install::msi_in_progress(&update_dir))
        && started.elapsed() < apply::TRIAL_TIMEOUT + Duration::from_secs(30)
    {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A trial start takes the instance lock right after the watchdog let go of
/// it; retry briefly rather than become a secondary instance.
pub async fn lock_for_trial(
    desktop_root: &Path,
) -> Option<metro_connector::desktop::store::InstanceLock> {
    for _ in 0..40 {
        if let Ok(Some(lock)) = metro_connector::desktop::store::try_lock_instance(desktop_root) {
            return Some(lock);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}

/// At a normal start of a script install: finish or undo an interrupted
/// update before anything else runs.
pub fn recover_at_start(install_dir: &Path, desktop_root: &Path) -> Result<(), String> {
    let Some(own) = metro_release::Version::parse(env!("CARGO_PKG_VERSION")) else {
        return Ok(());
    };
    let update_dir = desktop_root.join("update");
    let pending = "The previous update could not be recovered yet. The workspace backup was kept. Close any installer and restart Metro to retry.";
    if install::msi_in_progress(&update_dir) {
        return Err(pending.into());
    }
    install::recover_msi(desktop_root, &update_dir, &own).map_err(|_| pending.to_string())?;
    let Some(root) = install_dir.parent().and_then(|v| v.parent()) else {
        return Ok(());
    };
    if !root.join(install::MARKER).is_file() {
        return Ok(());
    }
    match install::recover(root, desktop_root, &update_dir, &own) {
        install::Recovery::Nothing => {}
        install::Recovery::Blocked => return Err(pending.into()),
        install::Recovery::RolledBack if install::read_pointer(root) != Some(own.to_string()) => {
            return Err("Metro restored the previous installation. Start Metro from its usual shortcut again.".into());
        }
        r => eprintln!("metro-desktop: finished an interrupted update ({r:?})"),
    }
    Ok(())
}

pub fn is_expected_trial(id: &str, install_dir: &Path, desktop_root: &Path) -> bool {
    let Some(own) = metro_release::Version::parse(env!("CARGO_PKG_VERSION")) else {
        return false;
    };
    let kind = install::detect(
        install_dir,
        &own,
        metro_release::Channel::current(),
        Some(id),
    );
    let update_dir = desktop_root.join("update");
    let Some(path) = install::journal_path(&kind, &update_dir) else {
        return false;
    };
    install::read_journal(&path).is_some_and(|j| {
        j.phase == "trial"
            && j.to == own.to_string()
            && j.id == id
            && j.watchdog_pid.is_some_and(install::pid_alive)
            && kind
                .root()
                .is_none_or(|root| install::script_state(root, &j.from))
    })
}

// ---- Windows MSI helper ----

/// `--update-helper <id>`: see the module docs.
pub fn run_helper(id: &str) -> u8 {
    let Ok(desktop_root) = metro_connector::desktop::store::desktop_root() else {
        return 2;
    };
    let update_dir = desktop_root.join("update");
    let jpath = update_dir.join(install::MSI_JOURNAL);
    if !cfg!(windows) {
        return 2;
    }
    let Some(install_lock) = wait_for_lock(|| install::Lock::take(&update_dir).ok()) else {
        crate::fatal(
            "The installer remained busy. Metro did not start the update; start Metro again to recover.",
        );
        return 1;
    };
    let Some(mut j) = install::read_journal(&jpath).filter(|j| {
        j.id == id
            && j.phase == "installing"
            && j.watchdog_pid == Some(std::process::id())
            && j.from == env!("CARGO_PKG_VERSION")
    }) else {
        return 2;
    };
    let finish = |j: &mut install::Journal,
                  phase: JournalPhase,
                  outcome: &str,
                  detail: Option<String>| {
        j.phase = phase;
        j.outcome = Some(outcome.into());
        j.detail = detail;
        if install::write_journal(&jpath, j).is_err() {
            crate::fatal("Metro could not save the update outcome. The workspace backup was kept.");
            false
        } else {
            true
        }
    };
    // Wait for the old app to let go of its instance lock (it is exiting).
    let instance = wait_for_lock(|| {
        metro_connector::desktop::store::try_lock_instance(&desktop_root)
            .ok()
            .flatten()
    });
    let Some(instance) = instance else {
        finish(
            &mut j,
            JournalPhase::Failed,
            "install_failed",
            Some("The old instance did not close in time.".into()),
        );
        crate::fatal(
            "Metro did not install the update because another Metro instance is still running.",
        );
        return 1;
    };
    let Some(source_pid) = j.source_pid else {
        return 2;
    };
    if wait_for_lock(|| (!install::pid_alive(source_pid)).then_some(())).is_none() {
        finish(
            &mut j,
            JournalPhase::Failed,
            "install_failed",
            Some("The old launcher did not exit in time.".into()),
        );
        crate::fatal(
            "Metro did not install the update because the previous launcher did not exit.",
        );
        return 1;
    }
    // The MSI sat on disk since it was verified: check it again right before
    // it runs and hold its write/delete-denying handle until msiexec returns.
    let verified = || {
        let own = metro_release::Version::parse(&j.from)?;
        let dir = registered_version_dir(&own)?;
        let kind = InstallKind::WindowsMsi { dir };
        let trust =
            metro_release::trust_refresh::TrustRefresh::new(&update_dir.join("trust-cache"))
                .ok()?;
        install::verify_staged(
            &kind,
            &update_dir,
            &j,
            trust.verifier.as_ref(),
            None,
            &install_lock,
        )
        .ok()
    };
    let msi = match verified() {
        Some(msi) => msi,
        None => {
            finish(
                &mut j,
                JournalPhase::Failed,
                "install_failed",
                Some("The staged installer could not be verified.".into()),
            );
            drop(instance);
            drop(install_lock);
            start_installed(None);
            return 1;
        }
    };
    let log = update_dir.join(format!("msi-{}.log", j.to));
    let code = run_msiexec(&msi.path, &log);
    drop(msi);
    match code {
        Some(0) | Some(3010) => {}
        other => {
            // The MSI's own transaction rolled the install back.
            let detail = format!(
                "the installer stopped (code {}); details saved",
                other.map_or("?".into(), |c| c.to_string())
            );
            let saved = finish(&mut j, JournalPhase::Failed, "install_failed", Some(detail));
            drop(instance);
            drop(install_lock);
            start_installed(None);
            return u8::from(!saved);
        }
    }
    j.phase = JournalPhase::Trial;
    if install::write_journal(&jpath, &j).is_err() {
        crate::fatal("Metro could not record the trial. Restart Metro to recover.");
        return 1;
    }
    let Some(exe) = start_installed(Some(&j.id)) else {
        finish(
            &mut j,
            JournalPhase::Failed,
            "install_failed",
            Some("the new version didn't start".into()),
        );
        crate::fatal(&no_rollback_text(&j.to, &j.from));
        return 1;
    };
    let mut child = exe;
    // Normal starts remain gated by this helper's journal while the trial
    // takes the instance lock. Keep the installer lock through confirmation.
    drop(instance);
    let started = std::time::Instant::now();
    loop {
        if install::trial_ok(&update_dir, &j.id) {
            return u8::from(!finish(&mut j, JournalPhase::Committed, "updated", None));
        }
        if let Ok(Some(status)) = child.try_wait()
            && !install::trial_ok(&update_dir, &j.id)
        {
            if status.success() {
                return u8::from(!finish(
                    &mut j,
                    JournalPhase::Failed,
                    "install_failed",
                    Some("The trial closed before it confirmed the update.".into()),
                ));
            }
            break;
        }
        if started.elapsed() > apply::TRIAL_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    // Installed but not healthy: no automatic downgrade. Data from before
    // the update stays saved in the snapshot.
    finish(
        &mut j,
        JournalPhase::Failed,
        "install_failed",
        Some("installed but didn't start".into()),
    );
    crate::fatal(&no_rollback_text(&j.to, &j.from));
    1
}

fn wait_for_lock<T>(mut attempt: impl FnMut() -> Option<T>) -> Option<T> {
    (0..240).find_map(|_| {
        let lock = attempt();
        if lock.is_none() {
            std::thread::sleep(Duration::from_millis(250));
        }
        lock
    })
}

fn registered_version_dir(own: &metro_release::Version) -> Option<PathBuf> {
    #[cfg(windows)]
    return install::msi_registered_dir(own);
    #[cfg(not(windows))]
    {
        let _ = own;
        None
    }
}

fn no_rollback_text(to: &str, from: &str) -> String {
    format!(
        "Metro {to} was installed but didn't start. Your data from before the update is saved. \
         Reinstall Metro {from} from https://github.com/wnzn/metro/releases/tag/v{from}, or report the problem."
    )
}

fn run_msiexec(msi: &Path, log: &Path) -> Option<i32> {
    #[cfg(windows)]
    {
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let msiexec = PathBuf::from(system).join("System32").join("msiexec.exe");
        std::process::Command::new(msiexec)
            .arg("/i")
            .arg(msi)
            .args(["/passive", "/norestart", "REBOOT=ReallySuppress", "/l*v"])
            .arg(log)
            .status()
            .ok()?
            .code()
    }
    #[cfg(not(windows))]
    {
        let _ = (msi, log, PathBuf::new());
        None
    }
}

/// Start the installed Metro (from the MSI's registered folder).
fn start_installed(trial: Option<&str>) -> Option<std::process::Child> {
    #[cfg(windows)]
    {
        let dir = registered_install_dir()?;
        let mut cmd = std::process::Command::new(dir.join("Metro.exe"));
        if let Some(id) = trial {
            cmd.arg("--update-trial").arg(id);
        }
        cmd.current_dir(&dir).spawn().ok()
    }
    #[cfg(not(windows))]
    {
        let _ = trial;
        None
    }
}

#[cfg(windows)]
fn registered_install_dir() -> Option<PathBuf> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    let w = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (key, value) = (w("Software\\Metro\\Desktop MSI"), w("InstallDir"));
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
    (rc == 0).then(|| {
        PathBuf::from(String::from_utf16_lossy(
            &buf[..(len as usize / 2).saturating_sub(1).min(buf.len())],
        ))
    })
}
