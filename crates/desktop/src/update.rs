//! The launcher's part of a script-install trial (Linux and Windows script
//! installs). MSI updates are manual; this build never spawns an MSI helper.
//!
//! Windows script installs also start through a stable entry point,
//! `<root>\Metro.exe` ([`entry_main`]), and a normal start of a version that
//! is no longer current starts the current one instead ([`forward_stale`]).

use std::path::Path;
use std::sync::Arc;

use metro_connector::desktop::store::InstanceLock;
use metro_connector::desktop::update::apply::{self, RootGate, TrialOutcome, TrialPaths};
use metro_connector::desktop::update::install::{self, InstallKind};
use metro_connector::desktop::update::{ApplyRequest, InstallFailure, Updater, own_version};

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Trial(String),
    /// [msi lane] `--update-helper <id>`: see `crate::msi`.
    Helper(String),
    LegacyHelper,
}

pub fn mode() -> Mode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag, id] if flag == "--update-helper" && install::valid_id(id) => {
            Mode::Helper(id.clone())
        }
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

pub(crate) fn recovery_message(code: &str, reason: &str) -> String {
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
    if cfg!(windows) {
        message.push_str(
            "\nFor read-only diagnosis, run `install-metro-windows.ps1 -Action Status` using the verified official \
             installer and the same install location. Do not run Install, Update or Uninstall to recover.",
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
    // [msi lane] dispatch hook
    if crate::msi::is_msi(updater) {
        return crate::msi::apply(updater, req, desktop_root, instance_lock).await;
    }
    let root = match updater.kind().clone() {
        InstallKind::LinuxInstaller { root } | InstallKind::WindowsScript { root } => root,
        InstallKind::WindowsMsi { .. } | InstallKind::Portable | InstallKind::Unsupported(_) => {
            return After::Relaunch;
        }
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
    // Windows: nobody else may take the instance lock until the trial has it.
    #[cfg(windows)]
    let handoff = match instance_lock.begin_handoff(desktop_root) {
        Ok(h) => h,
        Err(_) => {
            record_failure(InstallFailure::InstallerBusy);
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
            #[cfg(windows)]
            {
                drop(install_lock);
                if !instance_lock.hand_over(
                    desktop_root,
                    handoff,
                    std::time::Duration::from_secs(15),
                ) {
                    eprintln!(
                        "metro-desktop: Metro {} did not take over the workspace lock",
                        j.to
                    );
                }
            }
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
            if !trial && !own_version().is_some_and(|v| install::script_state(root, &v.to_string()))
            {
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
    let Some(own) = own_version() else {
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
    #[cfg(any(target_os = "linux", windows))]
    {
        let parent = parent_pid()?;
        let own = own_version()?;
        let kind = install::detect(
            install_dir,
            &own,
            metro_connector::desktop::update::channel(),
            Some(id),
        );
        let (InstallKind::LinuxInstaller { ref root } | InstallKind::WindowsScript { ref root }) =
            kind
        else {
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
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (id, install_dir, desktop_root);
        None
    }
}

#[cfg(target_os = "linux")]
fn parent_pid() -> Option<u32> {
    // SAFETY: getppid has no preconditions.
    Some(unsafe { libc::getppid() } as u32)
}

#[cfg(windows)]
fn parent_pid() -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    let me = std::process::id();
    // SAFETY: a snapshot handle closed below; the entry is sized as required.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        let mut ok = Process32FirstW(snap, &mut entry) != 0;
        while ok {
            if entry.th32ProcessID == me {
                found = Some(entry.th32ParentProcessID);
                break;
            }
            ok = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        found.filter(|&p| p != 0 && install::pid_alive(p))
    }
}

/// Start `exe` with no arguments, outside any job this process is in.
#[cfg(windows)]
fn start_detached(exe: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let mut cmd = std::process::Command::new(exe);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    if cmd.spawn().is_ok() {
        return Ok(());
    }
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    cmd.spawn().map(drop)
}

/// The canonical path of this program, without a verbatim prefix.
#[cfg(windows)]
fn own_exe() -> Option<std::path::PathBuf> {
    let exe = std::fs::canonicalize(std::env::current_exe().ok()?).ok()?;
    let s = exe.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => Some(rest.into()),
        _ => Some(exe),
    }
}

/// `<root>\Metro.exe` of a Windows script install: start the current
/// version and exit. Arguments are never passed on.
#[cfg(windows)]
pub fn entry_main() -> Option<std::process::ExitCode> {
    let root = install::entry::root_of(&own_exe()?)?;
    let started = install::entry::target(&root).and_then(|exe| start_detached(&exe));
    Some(match started {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => {
            crate::fatal(
                "Metro can't start: this installation is incomplete. Install Metro again with the Windows installer script.",
            );
            std::process::ExitCode::from(1)
        }
    })
}

/// A normal start of a version that isn't current in a managed Windows
/// install starts the current version instead.
#[cfg(windows)]
pub fn forward_stale(install_dir: &Path) -> Option<std::process::ExitCode> {
    let target = install::entry::forward_target(install_dir, &own_version()?)?;
    Some(match start_detached(&target) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => std::process::ExitCode::from(1),
    })
}

/// After a normal start of the current version of a managed Windows install:
/// keep the Installed-apps version in step (a commit's own write can fail).
pub fn after_start(install_dir: &Path) {
    #[cfg(windows)]
    if let (Some(root), Some(own)) = (managed_root(install_dir), own_version()) {
        let own = own.to_string();
        if install::script_state(root, &own)
            && install::managed(&InstallKind::WindowsScript {
                root: root.to_path_buf(),
            })
        {
            let _ = install::installed_apps::sync(root, &own);
        }
    }
    #[cfg(not(windows))]
    let _ = install_dir;
}
