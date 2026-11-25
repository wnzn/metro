//! The launcher's part of a script-install trial. Windows updates are
//! manual; this build never spawns an MSI helper.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use metro_connector::desktop::update::apply::{self, TrialOutcome, TrialPaths};
use metro_connector::desktop::update::install::{self, InstallKind};
use metro_connector::desktop::update::{ApplyRequest, LastUpdate, Updater, now_ms};

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Trial(String),
    LegacyHelper,
}

pub fn mode() -> Mode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, ..] if flag == "--update-helper" => Mode::LegacyHelper,
        [flag, id] if flag == "--update-trial" && install::valid_id(id) => Mode::Trial(id.clone()),
        _ => Mode::Normal,
    }
}

pub enum After {
    Exit(u8),
    Relaunch,
}

pub async fn apply<L>(
    updater: &Arc<Updater>,
    req: ApplyRequest,
    desktop_root: &Path,
    instance_lock: L,
) -> After {
    let InstallKind::LinuxInstaller { root } = updater.kind().clone() else {
        return After::Relaunch;
    };
    let paths = TrialPaths {
        root,
        desktop_root: desktop_root.to_path_buf(),
        update_dir: updater.dir().to_path_buf(),
    };
    let record_failure = |detail: &str| {
        updater.record_outcome(LastUpdate {
            from: updater.own().to_string(),
            to: req.version.to_string(),
            outcome: "install_failed".into(),
            at: now_ms(),
            detail: Some(detail.into()),
        });
    };
    let install_lock = match install::Lock::take(&paths.root) {
        Ok(lock) => lock,
        Err(_) => {
            record_failure("The installer is busy. Close the other installer and try again.");
            return After::Relaunch;
        }
    };
    let (mut j, verified) = match updater.verify_apply_with_refresh(&req, &install_lock).await {
        Ok(stage) => stage,
        Err(e) => {
            eprintln!("metro-desktop: the staged update wasn't used: {e}");
            record_failure(
                "The staged update could not be verified. Check again before restarting.",
            );
            return After::Relaunch;
        }
    };
    if let Err(e) = apply::begin_trial(&paths, &mut j, &install_lock) {
        eprintln!("metro-desktop: preparing the update failed: {}", e.kind());
        record_failure("The local workspace could not be backed up. The update was not started.");
        return After::Relaunch;
    }
    drop(instance_lock);
    let mut child = match apply::spawn_checked_trial(verified, &j.id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "metro-desktop: starting Metro {} failed: {}",
                j.to,
                e.kind()
            );
            if install::rollback(
                &paths.root,
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

pub async fn wait_for_trial(install_dir: &Path) {
    let root = install_dir
        .parent()
        .and_then(Path::parent)
        .filter(|root| root.join(install::MARKER).is_file());
    let started = std::time::Instant::now();
    while root.is_some_and(install::trial_in_progress)
        && started.elapsed() < apply::TRIAL_TIMEOUT + Duration::from_secs(30)
    {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

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

pub fn gate_msi_at_start(desktop_root: &Path) -> Result<(), String> {
    install::gate_legacy_msi(&desktop_root.join("update")).map_err(|_| {
        "Metro can't confirm that the previous Windows Installer transaction finished. The update record and workspace backup were kept. Resolve the installer and backup manually before starting Metro.".into()
    })
}

pub fn recover_at_start(install_dir: &Path, desktop_root: &Path) -> Result<(), String> {
    let Some(own) = metro_release::Version::parse(env!("CARGO_PKG_VERSION")) else {
        return Ok(());
    };
    let Some(root) = install_dir.parent().and_then(|v| v.parent()) else {
        return Ok(());
    };
    if !root.join(install::MARKER).is_file() {
        return Ok(());
    }
    match install::recover(root, desktop_root, &desktop_root.join("update"), &own) {
        install::Recovery::Nothing => {}
        install::Recovery::Blocked => {
            return Err("The previous update could not be recovered yet. The workspace backup was kept. Close any installer and restart Metro to retry.".into());
        }
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
    let InstallKind::LinuxInstaller { ref root } = kind else {
        return false;
    };
    let Some(path) = install::journal_path(&kind, &desktop_root.join("update")) else {
        return false;
    };
    install::read_journal(&path).is_some_and(|j| {
        j.phase == "trial"
            && j.to == own.to_string()
            && j.id == id
            && j.watchdog_pid.is_some_and(install::pid_alive)
            && install::script_state(root, &j.from)
    })
}
