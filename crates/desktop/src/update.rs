//! The launcher's part of a script-install trial. Windows updates are
//! manual; this build never spawns an MSI helper.

use std::path::Path;
use std::sync::Arc;

use metro_connector::desktop::store::InstanceLock;
use metro_connector::desktop::update::apply::{self, RootGate, TrialOutcome, TrialPaths};
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
        [flag, ..] if flag == "--update-trial" => Mode::Trial(String::new()),
        _ => Mode::Normal,
    }
}

pub enum After {
    Exit(u8),
    Relaunch,
}

pub async fn apply(
    updater: &Arc<Updater>,
    req: ApplyRequest,
    desktop_root: &Path,
    instance_lock: Arc<InstanceLock>,
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
    if let Err(e) = apply::begin_trial(&paths, &mut j, &install_lock, &instance_lock) {
        eprintln!("metro-desktop: preparing the update failed: {}", e.kind());
        record_failure("The local workspace could not be backed up. The update was not started.");
        return After::Relaunch;
    }
    let retire = |id: &str| RootGate::retire(desktop_root, &paths.root, id, &instance_lock);
    let mut child = match apply::spawn_checked_trial(verified, &j.id, &instance_lock) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "metro-desktop: starting Metro {} failed: {}",
                j.to,
                e.kind()
            );
            if install::rollback(
                &paths,
                &mut j,
                &install_lock,
                &instance_lock,
                "rolled_back",
                Some("didn't start".into()),
            )
            .and_then(|()| retire(&j.id))
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
    let outcome = apply::watch_trial(
        &paths,
        &mut j,
        &install_lock,
        &instance_lock,
        &mut child,
        apply::TRIAL_TIMEOUT,
    )
    .await;
    if outcome != TrialOutcome::NeedsRecovery && retire(&j.id).is_err() {
        crate::fatal(
            "Metro could not clear the workspace update record. Start Metro again to finish cleanup.",
        );
        return After::Exit(1);
    }
    match outcome {
        TrialOutcome::Committed => {
            eprintln!("metro-desktop: updated to {}", j.to);
            After::Exit(0)
        }
        TrialOutcome::NotConfirmed => After::Exit(0),
        TrialOutcome::NeedsRecovery => {
            crate::fatal(
                "Metro did not finish the update. The backup and update records were kept. Resolve the interrupted update manually before restarting.",
            );
            After::Exit(1)
        }
        TrialOutcome::RolledBack(detail) => {
            eprintln!("metro-desktop: Metro {} {detail}; back on {}", j.to, j.from);
            After::Relaunch
        }
    }
}

pub fn gate_msi_at_start(desktop_root: &Path) -> Result<(), String> {
    install::gate_legacy_msi(&desktop_root.join("update")).map_err(|_| {
        "Metro can't confirm that the previous Windows Installer transaction finished. The update record and workspace backup were kept. Resolve the installer and backup manually before starting Metro.".into()
    })
}

pub fn recover_at_start(
    install_dir: &Path,
    desktop_root: &Path,
    lock: Option<&InstanceLock>,
) -> Result<(), String> {
    let blocked = || {
        "The shared workspace has an unfinished update. Close other Metro instances. If the interrupted update still blocks startup, preserve both copies and resolve it manually before restarting.".to_string()
    };
    let gate = RootGate::read(desktop_root).map_err(|_| blocked())?;
    let Some(lock) = lock else {
        return if gate.is_some() {
            Err(blocked())
        } else {
            Ok(())
        };
    };
    let root = install_dir
        .parent()
        .and_then(Path::parent)
        .filter(|root| root.join(install::MARKER).is_file());
    let Some(root) = root else {
        return if gate.is_some() {
            Err(blocked())
        } else {
            Ok(())
        };
    };
    let Some(own) = metro_release::Version::parse(env!("CARGO_PKG_VERSION")) else {
        return Err(blocked());
    };
    if let Some(gate) = &gate {
        let j = install::load_journal(&root.join(install::JOURNAL))
            .map_err(|_| blocked())?
            .filter(|j| gate.matches(root, &j.id))
            .ok_or_else(blocked)?;
        if !matches!(
            j.phase.as_str(),
            "staged" | "backing_up" | "committed" | "failed"
        ) || !install::script_state(root, &own.to_string())
        {
            return Err(blocked());
        }
        if install::recover(root, desktop_root, &desktop_root.join("update"), &own)
            == install::Recovery::Blocked
        {
            return Err(blocked());
        }
        return RootGate::retire(desktop_root, root, &j.id, lock).map_err(|_| blocked());
    }
    if install::recover(root, desktop_root, &desktop_root.join("update"), &own)
        == install::Recovery::Blocked
    {
        return Err(blocked());
    }
    Ok(())
}

pub fn trial_lock(id: &str, install_dir: &Path, desktop_root: &Path) -> Option<InstanceLock> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getppid has no preconditions.
        let parent = unsafe { libc::getppid() } as u32;
        let own = metro_release::Version::parse(env!("CARGO_PKG_VERSION"))?;
        let kind = install::detect(
            install_dir,
            &own,
            metro_release::Channel::current(),
            Some(id),
        );
        let InstallKind::LinuxInstaller { ref root } = kind else {
            return None;
        };
        let gate = RootGate::read(desktop_root).ok().flatten()?;
        let j = install::read_journal(&root.join(install::JOURNAL))?;
        if !gate.matches(root, id)
            || j.id != id
            || j.phase != "trial"
            || j.to != own.to_string()
            || j.watchdog_pid != Some(parent)
            || !install::script_state(root, &j.from)
        {
            return None;
        }
        metro_connector::desktop::store::InstanceLock::adopt_trial(desktop_root).ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (id, install_dir, desktop_root);
        None
    }
}
