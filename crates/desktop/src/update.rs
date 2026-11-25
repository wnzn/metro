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
use metro_connector::desktop::update::install::{self, InstallKind};
use metro_connector::desktop::update::{ApplyRequest, Updater};

/// How this process was started.
#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Trial(String),
    Helper(String),
}

fn valid_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|c| c.is_ascii_hexdigit())
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
    match updater.kind().clone() {
        InstallKind::LinuxInstaller { root } | InstallKind::WindowsScript { root } => {
            let Some(mut j) = install::read_journal(&root.join(install::JOURNAL))
                .filter(|j| j.id == req.journal_id && j.phase == "staged")
            else {
                eprintln!("metro-desktop: the staged update is gone; staying on this version");
                return After::Relaunch;
            };
            let paths = TrialPaths {
                root: root.clone(),
                desktop_root: desktop_root.to_path_buf(),
                update_dir,
            };
            if let Err(e) = apply::begin_trial(&paths, &mut j) {
                eprintln!("metro-desktop: preparing the update failed: {}", e.kind());
                let _ = install::rollback(
                    &root,
                    desktop_root,
                    &paths.update_dir,
                    &mut j,
                    "rolled_back",
                    Some("couldn't prepare".into()),
                );
                return After::Relaunch;
            }
            drop(lock);
            let exe = apply::trial_exe(&root, &j.to);
            let mut child = match apply::spawn_trial(&exe, &j.id) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "metro-desktop: starting Metro {} failed: {}",
                        j.to,
                        e.kind()
                    );
                    let _ = install::rollback(
                        &root,
                        desktop_root,
                        &paths.update_dir,
                        &mut j,
                        "rolled_back",
                        Some("didn't start".into()),
                    );
                    return After::Relaunch;
                }
            };
            match apply::watch_trial(&paths, &mut j, &mut child, apply::TRIAL_TIMEOUT).await {
                TrialOutcome::Committed => {
                    eprintln!("metro-desktop: updated to {}", j.to);
                    After::Exit(0)
                }
                TrialOutcome::NotConfirmed => After::Exit(0),
                TrialOutcome::RolledBack(detail) => {
                    eprintln!("metro-desktop: Metro {} {detail}; back on {}", j.to, j.from);
                    After::Relaunch
                }
            }
        }
        InstallKind::WindowsMsi { .. } => {
            let jpath = update_dir.join(install::MSI_JOURNAL);
            let Some(mut j) = install::read_journal(&jpath)
                .filter(|j| j.id == req.journal_id && j.phase == "staged")
            else {
                return After::Relaunch;
            };
            j.local_db_snapshot = install::snapshot_db(desktop_root, &update_dir).unwrap_or(false);
            j.phase = "installing".into();
            if install::write_journal(&jpath, &j).is_err() {
                return After::Relaunch;
            }
            drop(lock);
            let helper = update_dir
                .join(format!("helper-{}", j.id))
                .join("metro-update-helper.exe");
            let mut cmd = std::process::Command::new(&helper);
            cmd.arg("--update-helper")
                .arg(&j.id)
                .stdin(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0000_0008 | 0x0000_0200); // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
            }
            match cmd.spawn() {
                Ok(_) => After::Exit(0),
                Err(_) => {
                    j.phase = "failed".into();
                    j.outcome = Some("install_failed".into());
                    j.detail = Some("the update helper didn't start".into());
                    let _ = install::write_journal(&jpath, &j);
                    After::Relaunch
                }
            }
        }
        _ => After::Relaunch,
    }
}

/// At a normal start of a script install: finish or undo an interrupted
/// update before anything else runs.
pub fn recover_at_start(install_dir: &Path, desktop_root: &Path) {
    let Some(own) = metro_release::Version::parse(env!("CARGO_PKG_VERSION")) else {
        return;
    };
    let Some(root) = install_dir.parent().and_then(|v| v.parent()) else {
        return;
    };
    if !root.join(install::MARKER).is_file() {
        return;
    }
    let update_dir = desktop_root.join("update");
    match install::recover(root, desktop_root, &update_dir, &own) {
        install::Recovery::Nothing => {}
        r => eprintln!("metro-desktop: finished an interrupted update ({r:?})"),
    }
}

// ---- Windows MSI helper ----

/// `--update-helper <id>`: see the module docs.
pub fn run_helper(id: &str) -> u8 {
    let Ok(desktop_root) = metro_connector::desktop::store::desktop_root() else {
        return 2;
    };
    let update_dir = desktop_root.join("update");
    let jpath = update_dir.join(install::MSI_JOURNAL);
    let Some(mut j) =
        install::read_journal(&jpath).filter(|j| j.id == id && j.phase == "installing")
    else {
        return 2;
    };
    let Some(msi) = j.msi.clone() else { return 2 };
    // Wait for the old app to let go of its instance lock (it is exiting).
    for _ in 0..240 {
        match metro_connector::desktop::store::try_lock_instance(&desktop_root) {
            Ok(Some(lock)) => {
                drop(lock);
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    }
    let log = update_dir.join(format!("msi-{}.log", j.to));
    let code = run_msiexec(&msi, &log);
    let finish = |j: &mut install::Journal, phase: &str, outcome: &str, detail: Option<String>| {
        j.phase = phase.into();
        j.outcome = Some(outcome.into());
        j.detail = detail;
        let _ = install::write_journal(&jpath, j);
    };
    match code {
        Some(0) | Some(3010) => {}
        other => {
            // The MSI's own transaction rolled the install back.
            let detail = format!(
                "the installer stopped (code {}); details saved",
                other.map_or("?".into(), |c| c.to_string())
            );
            finish(&mut j, "failed", "install_failed", Some(detail));
            start_installed(None);
            return 0;
        }
    }
    j.phase = "trial".into();
    let _ = install::write_journal(&jpath, &j);
    let Some(exe) = start_installed(Some(&j.id)) else {
        finish(
            &mut j,
            "failed",
            "install_failed",
            Some("the new version didn't start".into()),
        );
        crate::fatal(&no_rollback_text(&j.to, &j.from));
        return 1;
    };
    let mut child = exe;
    let started = std::time::Instant::now();
    loop {
        if install::trial_ok(&update_dir, &j.id) {
            finish(&mut j, "committed", "updated", None);
            return 0;
        }
        if let Ok(Some(status)) = child.try_wait()
            && !install::trial_ok(&update_dir, &j.id)
        {
            if status.success() {
                finish(&mut j, "failed", "not_confirmed", None);
                return 0;
            }
            break;
        }
        if started.elapsed() > apply::TRIAL_TIMEOUT {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    // Installed but not healthy: no automatic downgrade. Data from before
    // the update stays saved in the snapshot.
    finish(
        &mut j,
        "failed",
        "install_failed",
        Some("installed but didn't start".into()),
    );
    crate::fatal(&no_rollback_text(&j.to, &j.from));
    1
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
