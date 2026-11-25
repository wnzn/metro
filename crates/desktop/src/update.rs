//! The launcher's part of a script-install trial. Windows updates are
//! manual; this build never spawns an MSI helper.

use std::path::Path;
use std::sync::Arc;

use metro_connector::desktop::store::InstanceLock;
use metro_connector::desktop::update::apply::{self, RootGate, TrialOutcome, TrialPaths};
use metro_connector::desktop::update::install::{self, InstallKind};
use metro_connector::desktop::update::{ApplyRequest, InstallFailure, Updater};

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

fn recovery_message(code: &str, reason: &str) -> String {
    let mut message = format!(
        "[{code}] {reason}\n\
         Stop Metro, its bundled server and any Metro installer. Keep the installation and workspace data, \
         including the update records, live database and backup. Do not remove locks or records, \
         change the current version pointer, or overwrite either database copy."
    );
    if cfg!(target_os = "linux") {
        message.push_str(
            "\nFor read-only diagnosis, run `sh install-metro-linux.sh status` using the verified official \
             installer and the same install location. Do not run install, update or uninstall to recover.",
        );
    }
    message.push_str(
        "\nContact the Metro maintainers at https://github.com/wnzn/metro/issues. Share this diagnostic, \
         not private databases or credentials. Resume only after the installation and workspace \
         have been verified together.",
    );
    message
}

fn uncertain_journal() -> String {
    recovery_message(
        install::JOURNAL_UNCERTAIN,
        "Metro could not verify the update journal against this installation.",
    )
}

fn blocked_recovery() -> String {
    recovery_message(
        "update_recovery_required",
        "The shared workspace has an unfinished update. Metro cannot verify safe recovery yet.",
    )
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
    let record_failure = |reason| updater.record_install_failure(req.version, reason);
    let install_lock = match install::Lock::take(&paths.root) {
        Ok(lock) => lock,
        Err(_) => {
            record_failure(InstallFailure::InstallerBusy);
            return After::Relaunch;
        }
    };
    let (mut j, verified) = match updater.verify_apply_with_refresh(&req, &install_lock).await {
        Ok(stage) => stage,
        Err(e)
            if e.get_ref()
                .is_some_and(|cause| cause.is::<install::JournalUncertain>()) =>
        {
            crate::fatal(&uncertain_journal());
            return After::Exit(1);
        }
        Err(e) => {
            eprintln!("metro-desktop: the staged update wasn't used: {e}");
            if !record_failure(InstallFailure::StageVerificationFailed) {
                crate::fatal(
                    "Metro could not save its update status. The staged update was not used. Use Metro's official installer to update manually.",
                );
                return After::Exit(1);
            }
            return After::Relaunch;
        }
    };
    if let Err(e) = apply::begin_trial(&paths, &mut j, &install_lock, &instance_lock) {
        eprintln!("metro-desktop: preparing the update failed: {}", e.kind());
        record_failure(InstallFailure::BackupFailed);
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
            crate::fatal(&blocked_recovery());
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
        recovery_message(
            "update_recovery_required",
            "Metro can't confirm that the previous Windows Installer transaction finished.",
        )
    })
}

fn managed_root(install_dir: &Path) -> Option<&Path> {
    install_dir
        .parent()
        .and_then(Path::parent)
        .filter(|root| root.join(install::MARKER).is_file())
}

/// Read-only early rejection, including before monitoring or instance-lock
/// creation. A pass is not authorization: startup rechecks under its locks.
pub fn preflight_at_start(
    install_dir: &Path,
    desktop_root: &Path,
    trial: bool,
) -> Result<(), String> {
    gate_msi_at_start(desktop_root)?;
    let gate = RootGate::read(desktop_root).map_err(|_| uncertain_journal())?;
    if let Some(root) = managed_root(install_dir) {
        let journal = install::inspect_recovery_journal(root).map_err(|_| uncertain_journal())?;
        if let Some(gate) = gate {
            if !journal.is_some_and(|j| gate.matches(root, &j.id)) {
                return Err(uncertain_journal());
            }
            if !trial && !install::script_state(root, env!("CARGO_PKG_VERSION")) {
                return Err(uncertain_journal());
            }
        }
    } else if gate.is_some() {
        return Err(blocked_recovery());
    }
    Ok(())
}

pub fn recover_at_start(
    install_dir: &Path,
    desktop_root: &Path,
    lock: Option<&InstanceLock>,
) -> Result<(), String> {
    let gate = RootGate::read(desktop_root).map_err(|_| uncertain_journal())?;
    let Some(lock) = lock else {
        return if gate.is_some() {
            Err(blocked_recovery())
        } else {
            Ok(())
        };
    };
    let Some(root) = managed_root(install_dir) else {
        return if gate.is_some() {
            Err(blocked_recovery())
        } else {
            Ok(())
        };
    };
    let Some(own) = metro_release::Version::parse(env!("CARGO_PKG_VERSION")) else {
        return Err(blocked_recovery());
    };
    let update_dir = desktop_root.join("update");
    let recovery = if let Some(gate) = &gate {
        let j = install::inspect_recovery_journal(root)
            .map_err(|_| uncertain_journal())?
            .filter(|j| gate.matches(root, &j.id))
            .ok_or_else(uncertain_journal)?;
        if !install::script_state(root, &own.to_string()) {
            return Err(uncertain_journal());
        }
        if !matches!(
            j.phase.as_str(),
            "staged" | "backing_up" | "committed" | "failed"
        ) {
            return Err(blocked_recovery());
        }
        install::recover_with_gate(root, desktop_root, &update_dir, &own, &j, lock)
    } else {
        install::recover(root, desktop_root, &update_dir, &own)
    };
    match recovery {
        install::Recovery::Nothing => Ok(()),
        install::Recovery::Blocked => Err(blocked_recovery()),
        install::Recovery::JournalUncertain => Err(uncertain_journal()),
    }
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
