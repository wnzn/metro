//! The launcher's part of an in-app MSI update (see
//! `metro_connector::desktop::update::msi` for the journal and decisions).
//!
//! - Restart now ([`apply`]): with the UI closed and local work stopped,
//!   revalidate the staged MSI, back up the database, start a copy of this
//!   launcher from the private `update/helper-<id>` folder with
//!   `--update-helper <id>`, and exit.
//! - [`run_helper`]: wait for the app to let go of its locks, verify the
//!   exact MSI bytes again through a handle that stays open, run
//!   `msiexec /i <msi> /passive` per-user with the remembered folder and
//!   choices, record the result from facts, and start Metro again. The
//!   helper shows no dialogs: the next start explains whatever happened.
//! - [`preflight`] and [`recover_at_start`]: a normal start decides an
//!   unfinished update from facts, or stops before opening the workspace.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use metro_connector::desktop::store::{self, InstanceLock};
use metro_connector::desktop::update::install::{self, InstallKind};
use metro_connector::desktop::update::msi::{
    self, Decision, Facts, MsiJournal, Phase, Proc, SystemFacts,
};
use metro_connector::desktop::update::{ApplyRequest, InstallFailure, Updater};

use crate::update::After;

const OWN: &str = env!("CARGO_PKG_VERSION");
const LOCK_WAIT: Duration = Duration::from_secs(120);
const BUSY_WAIT: Duration = Duration::from_secs(60);
const HELPER_EXIT_WAIT: Duration = Duration::from_secs(15);

fn updating() -> String {
    "Metro is being updated. Start Metro again when the installer has finished.".into()
}

fn restart_windows() -> String {
    "Restart Windows to finish updating Metro, then start Metro again.".into()
}

fn uncertain() -> String {
    crate::update::recovery_message(
        "update_recovery_required",
        "Metro can't confirm how the Windows Installer update ended.",
    )
}

fn unreadable() -> String {
    crate::update::recovery_message(
        install::JOURNAL_UNCERTAIN,
        "Metro could not verify the update journal against this installation.",
    )
}

// ---- Restart now (the old app) ----

pub async fn apply(
    updater: &Arc<Updater>,
    req: ApplyRequest,
    desktop_root: &Path,
    instance_lock: Arc<InstanceLock>,
) -> After {
    let update_dir = updater.dir().to_path_buf();
    let record_failure = |reason| updater.record_install_failure(req.version, reason);
    let lock = match install::Lock::take(&update_dir) {
        Ok(lock) => lock,
        Err(_) => {
            record_failure(InstallFailure::InstallerBusy);
            return After::Relaunch;
        }
    };
    let (mut j, checked) = match updater.verify_msi_apply(&req, &lock) {
        Ok(v) => v,
        Err(e)
            if e.get_ref()
                .is_some_and(|cause| cause.is::<install::JournalUncertain>()) =>
        {
            crate::fatal(&unreadable());
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
    // The helper verifies the bytes again; it doesn't rely on this handle.
    drop(checked);
    if let Err(e) = msi::begin_handoff(desktop_root, &update_dir, &mut j, &lock, &instance_lock) {
        eprintln!("metro-desktop: saving the backup failed: {}", e.kind());
        if j.phase == Phase::BackingUp {
            let _ = msi::abandon_handoff(&update_dir, &j, &lock);
        }
        record_failure(InstallFailure::BackupFailed);
        return After::Relaunch;
    }
    let started = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .and_then(|exe| msi::prepare_helper(&update_dir, &j, &exe))
        .and_then(|helper| {
            let child = spawn_detached(&helper.path, &["--update-helper", &j.id]);
            drop(helper);
            child
        });
    match started {
        Ok(_) => {
            eprintln!("metro-desktop: installing Metro {}", j.to);
            // Keep both locks until this process has ended (Windows releases
            // them with its handles), so the helper starts msiexec only once
            // nothing of this version is running.
            std::mem::forget(lock);
            std::mem::forget(instance_lock);
            After::Exit(0)
        }
        Err(e) => {
            eprintln!(
                "metro-desktop: the update helper didn't start: {}",
                e.kind()
            );
            match msi::abandon_handoff(&update_dir, &j, &lock) {
                Ok(last) => {
                    updater.record_msi_outcome(last);
                    After::Relaunch
                }
                Err(_) => {
                    crate::fatal(&uncertain());
                    After::Exit(1)
                }
            }
        }
    }
}

/// Start a program outside this launcher's process group and job, so it
/// outlives the launcher.
fn spawn_detached(exe: &Path, args: &[&str]) -> std::io::Result<std::process::Child> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if let Ok(c) = cmd.spawn() {
            return Ok(c);
        }
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()
}

// ---- the helper ----

/// Both locks, once the app has let go of them.
fn take_locks(desktop_root: &Path, update_dir: &Path) -> Option<(InstanceLock, install::Lock)> {
    let started = Instant::now();
    while started.elapsed() < LOCK_WAIT {
        if let Ok(Some(instance)) = store::try_lock_instance(desktop_root)
            && let Ok(lock) = install::Lock::take(update_dir)
        {
            return Some((instance, lock));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

fn wait_not_busy(facts: &dyn Facts) -> bool {
    let started = Instant::now();
    loop {
        match facts.installer_busy() {
            Some(false) => return true,
            _ if started.elapsed() >= BUSY_WAIT => return false,
            _ => std::thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// Start the installed Metro (the registered folder, if it is the journal's).
fn relaunch(j: &MsiJournal, facts: &dyn Facts) {
    let Some(reg) = facts.registered() else {
        return;
    };
    let same = std::fs::canonicalize(&reg.dir).ok() == std::fs::canonicalize(&j.install_dir).ok();
    if same {
        let _ = spawn_detached(&j.install_dir.join(msi::LAUNCHER), &[]);
    }
}

fn verifier(update_dir: &Path) -> Option<Arc<dyn metro_release::ReleaseVerifier>> {
    if let (_, Some((_, verifier))) = metro_connector::desktop::update::test_release() {
        return Some(verifier);
    }
    let trust =
        metro_release::trust_refresh::TrustRefresh::new(&update_dir.join("trust-cache")).ok()?;
    Some(trust.verifier.clone())
}

fn msiexec(command_line: &str) -> std::io::Result<std::process::Child> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let exe = PathBuf::from(system).join("System32").join("msiexec.exe");
        std::process::Command::new(exe)
            .raw_arg(command_line)
            .stdin(std::process::Stdio::null())
            .spawn()
    }
    #[cfg(not(windows))]
    {
        let _ = (command_line, PathBuf::new());
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// `--update-helper <id>`. Exit codes: 0 handed back to Metro, 1 failed
/// (recorded), 2 nothing to do, 3 the app didn't let go.
pub fn run_helper(id: &str) -> u8 {
    let Ok(desktop_root) = store::desktop_root() else {
        return 2;
    };
    let update_dir = desktop_root.join("update");
    let Some((instance, lock)) = take_locks(&desktop_root, &update_dir) else {
        // Nothing ran; the next start retires the handoff.
        return 3;
    };
    let Ok(mut j) = msi::claim(&update_dir, id, &lock) else {
        return 2;
    };
    let facts = SystemFacts;
    let fail = |j: &MsiJournal, failure: &'static str, detail: &str| -> u8 {
        let _ = msi::fail_before_msiexec(&update_dir, j, failure, detail, &lock);
        1
    };
    let reg = match msi::precheck(&j, &facts) {
        Ok(reg) => reg,
        Err(failure) => {
            let code = fail(&j, failure, "the installed Metro changed before the update");
            drop((lock, instance));
            relaunch(&j, &facts);
            return code;
        }
    };
    let checked = match verifier(&update_dir)
        .ok_or_else(|| std::io::Error::other("no release verifier"))
        .and_then(|v| msi::verify_package(&update_dir, &j, v.as_ref(), None))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("metro-desktop: the staged installer didn't verify: {e}");
            let code = fail(&j, "not_verified", "the downloaded installer didn't verify");
            drop((lock, instance));
            relaunch(&j, &facts);
            return code;
        }
    };
    let log = update_dir.join(msi::LOG);
    let Ok(line) = msi::msiexec_command_line(&checked.path, &log, &reg) else {
        let code = fail(
            &j,
            "installer_failed",
            "the install folder can't be passed on",
        );
        drop((lock, instance));
        relaunch(&j, &facts);
        return code;
    };
    if msi::begin_install(&update_dir, &mut j, Proc::current(), &lock).is_err() {
        // Still `handoff`: the next start retires it.
        drop((lock, instance));
        relaunch(&j, &facts);
        return 1;
    }
    wait_not_busy(&facts);
    let code = match msiexec(&line) {
        Ok(mut child) => {
            let _ = msi::record_msiexec(&update_dir, &mut j, Proc::of(&child), &lock);
            child.wait().ok().and_then(|s| s.code())
        }
        Err(e) => {
            eprintln!("metro-desktop: msiexec didn't start: {}", e.kind());
            let code = fail(&j, "installer_failed", "the installer didn't start");
            drop((checked, lock, instance));
            relaunch(&j, &facts);
            return code;
        }
    };
    drop(checked);
    if code != Some(1618) {
        wait_not_busy(&facts);
    }
    let result = match msi::after_msiexec(&j, code, &facts) {
        Some(n) if msi::write_decision(&update_dir, &j, &n, &lock).is_ok() => {
            u8::from(n.phase != Phase::Installed)
        }
        // Left `installing`: the next start decides from the facts, or stops.
        _ => 1,
    };
    drop((lock, instance));
    relaunch(&j, &facts);
    result
}

// ---- normal starts ----

pub enum Gate {
    Continue,
    /// An update is being installed; this start does nothing.
    QuietExit,
    Fatal(String),
}

/// Only the helper may still be running (no msiexec, no installer).
fn only_helper(j: &MsiJournal, facts: &dyn Facts) -> bool {
    !j.msiexec.is_some_and(|p| facts.running(&p))
        && j.helper
            .is_none_or(|h| facts.msiexec_child_running(&h) == Some(false))
        && facts.installer_busy() == Some(false)
}

/// Before the instance lock: read-only, and never shows the UI while an
/// installer may be replacing its files.
pub fn preflight(desktop_root: &Path) -> Gate {
    preflight_with(desktop_root, &SystemFacts, HELPER_EXIT_WAIT)
}

fn preflight_with(desktop_root: &Path, facts: &dyn Facts, helper_wait: Duration) -> Gate {
    let update_dir = desktop_root.join("update");
    let started = Instant::now();
    loop {
        let j = match msi::load(&update_dir) {
            Ok(j) => j,
            Err(_) => return Gate::Fatal(unreadable()),
        };
        match msi::decide(j.as_ref(), OWN, facts) {
            Decision::Proceed => return Gate::Continue,
            Decision::Write(_) => {
                let j = j.expect("a decision to write needs a journal");
                // The old app (handoff) or the helper may still hold the
                // instance lock: then it is busy updating.
                if matches!(j.phase, Phase::BackingUp | Phase::Handoff) {
                    return match store::try_lock_instance(desktop_root) {
                        Ok(Some(_)) => Gate::Continue,
                        Ok(None) => Gate::QuietExit,
                        Err(_) => Gate::Fatal(unreadable()),
                    };
                }
                return Gate::Continue;
            }
            Decision::Updating => {
                // A helper that just started Metro again is exiting.
                let j = j.expect("updating needs a journal");
                if !only_helper(&j, facts) || started.elapsed() >= helper_wait {
                    return Gate::QuietExit;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Decision::RestartWindows => return Gate::Fatal(restart_windows()),
            Decision::Uncertain => return Gate::Fatal(uncertain()),
        }
    }
}

/// Under the instance lock: settle an unfinished update or stop.
pub fn recover_at_start(desktop_root: &Path, lock: Option<&InstanceLock>) -> Result<(), String> {
    recover_with(desktop_root, lock, &SystemFacts)
}

fn recover_with(
    desktop_root: &Path,
    lock: Option<&InstanceLock>,
    facts: &dyn Facts,
) -> Result<(), String> {
    let update_dir = desktop_root.join("update");
    let observed = msi::load(&update_dir).map_err(|_| unreadable())?;
    let settled = |j: &Option<MsiJournal>| {
        j.as_ref().is_none_or(|j| {
            matches!(
                j.phase,
                Phase::Staged | Phase::Committed | Phase::Failed | Phase::Installed
            )
        })
    };
    if settled(&observed) {
        return decision_result(msi::decide(observed.as_ref(), OWN, facts));
    }
    // A secondary instance never opens a UI next to a running update.
    if lock.is_none() {
        return Err(updating());
    }
    let Ok(install_lock) = install::Lock::take(&update_dir) else {
        return Err(updating());
    };
    let j = msi::load(&update_dir).map_err(|_| unreadable())?;
    match msi::decide(j.as_ref(), OWN, facts) {
        Decision::Write(n) => {
            let j = j.expect("a decision to write needs a journal");
            msi::write_decision(&update_dir, &j, &n, &install_lock).map_err(|_| uncertain())
        }
        d => decision_result(d),
    }
}

fn decision_result(d: Decision) -> Result<(), String> {
    match d {
        Decision::Proceed => Ok(()),
        // Settled phases never need a write; others are decided under locks.
        Decision::Write(_) | Decision::Updating => Err(updating()),
        Decision::RestartWindows => Err(restart_windows()),
        Decision::Uncertain => Err(uncertain()),
    }
}

/// Whether this updater is for an MSI install.
pub fn is_msi(updater: &Updater) -> bool {
    matches!(updater.kind(), InstallKind::WindowsMsi { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use metro_connector::desktop::update::msi::Registered;
    use std::cell::Cell;

    struct Fake {
        busy: Cell<Option<bool>>,
        products: Vec<String>,
        version: String,
        dir: PathBuf,
        running: Vec<u32>,
    }

    impl Facts for Fake {
        fn installer_busy(&self) -> Option<bool> {
            self.busy.get()
        }
        fn installed_versions(&self) -> Option<Vec<String>> {
            Some(self.products.clone())
        }
        fn registered(&self) -> Option<Registered> {
            Some(Registered {
                dir: self.dir.clone(),
                version: self.version.clone(),
                local_runner: None,
                start_menu: None,
            })
        }
        fn running(&self, p: &Proc) -> bool {
            self.running.contains(&p.pid)
        }
        fn msiexec_child_running(&self, _: &Proc) -> Option<bool> {
            Some(false)
        }
    }

    fn older() -> String {
        let v = metro_release::Version::parse(OWN).unwrap();
        format!("{}.{}.{}", v.major, v.minor.saturating_sub(1), v.patch)
    }

    fn setup(phase: Phase) -> (tempfile::TempDir, PathBuf, MsiJournal) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("desktop");
        let update = root.join("update");
        std::fs::create_dir_all(&update).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for d in [&root, &update] {
                std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let dir = tmp.path().join("Metro Desktop");
        let mut j = MsiJournal {
            version: 3,
            id: "00112233445566aa".into(),
            from: older(),
            to: OWN.into(),
            phase,
            install_dir: dir,
            manifest_sha256: "a".repeat(64),
            msi_sha256: "b".repeat(64),
            local_db_snapshot: false,
            backup_manifest: Some("c".repeat(64)),
            helper: Some(Proc { pid: 5, created: 1 }),
            msiexec: None,
            exit_code: None,
            reboot_required: false,
            outcome: None,
            failure: None,
            detail: None,
            started_at: 0,
        };
        if phase == Phase::Handoff {
            j.helper = None;
        }
        msi::save(&update, &j).unwrap();
        (tmp, root, j)
    }

    fn fake(j: &MsiJournal, version: &str, running: Vec<u32>) -> Fake {
        Fake {
            busy: Cell::new(Some(false)),
            products: vec![
                msi::product_version(&metro_release::Version::parse(version).unwrap()).unwrap(),
            ],
            version: version.into(),
            dir: j.install_dir.clone(),
            running,
        }
    }

    fn files(j: &MsiJournal, version: &str) {
        let dir = &j.install_dir;
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("Metro.exe"), version).unwrap();
        let m = serde_json::json!({"version": version, "files": {"Metro.exe": metro_release::hex(&metro_release::sha256(version.as_bytes()))}});
        std::fs::write(dir.join("RELEASE.json"), m.to_string()).unwrap();
    }

    #[test]
    fn a_start_during_msiexec_exits_quietly_and_changes_nothing() {
        let (_tmp, root, mut j) = setup(Phase::Installing);
        j.msiexec = Some(Proc { pid: 6, created: 2 });
        msi::save(&root.join("update"), &j).unwrap();
        let f = fake(&j, &older(), vec![6]);
        assert!(matches!(
            preflight_with(&root, &f, Duration::from_secs(5)),
            Gate::QuietExit
        ));
        assert_eq!(msi::load(&root.join("update")).unwrap(), Some(j));
    }

    #[test]
    fn a_dead_helper_after_a_finished_install_continues_in_the_new_version() {
        let (_tmp, root, j) = setup(Phase::Installing);
        files(&j, OWN);
        let f = fake(&j, OWN, vec![]);
        assert!(matches!(
            preflight_with(&root, &f, Duration::ZERO),
            Gate::Continue
        ));
        let lock = store::try_lock_instance(&root).unwrap().unwrap();
        recover_with(&root, Some(&lock), &f).unwrap();
        let n = msi::load(&root.join("update")).unwrap().unwrap();
        assert_eq!(n.phase, Phase::Installed);
    }

    #[test]
    fn disagreeing_facts_stop_and_keep_the_journal() {
        let (_tmp, root, j) = setup(Phase::Installing);
        files(&j, &older());
        // Registry and product say the new version, files are still old,
        // and this is the new launcher: nothing adds up.
        let mut f = fake(&j, OWN, vec![]);
        f.products = vec![];
        assert!(matches!(
            preflight_with(&root, &f, Duration::ZERO),
            Gate::Fatal(_)
        ));
        let lock = store::try_lock_instance(&root).unwrap().unwrap();
        assert!(recover_with(&root, Some(&lock), &f).is_err());
        assert_eq!(msi::load(&root.join("update")).unwrap(), Some(j));
    }

    #[test]
    fn a_secondary_instance_never_runs_next_to_an_unfinished_update() {
        let (_tmp, root, j) = setup(Phase::Installing);
        files(&j, OWN);
        let f = fake(&j, OWN, vec![]);
        assert_eq!(recover_with(&root, None, &f), Err(updating()));
        assert_eq!(msi::load(&root.join("update")).unwrap(), Some(j));
    }

    #[test]
    fn an_old_app_still_in_handoff_holds_the_lock() {
        let (_tmp, root, _j) = setup(Phase::Handoff);
        let f = Fake {
            busy: Cell::new(Some(false)),
            products: vec![],
            version: String::new(),
            dir: PathBuf::new(),
            running: vec![],
        };
        // This launcher is the journal's `to`, so a handoff isn't its to
        // retire: it doesn't add up.
        assert!(matches!(
            preflight_with(&root, &f, Duration::ZERO),
            Gate::Fatal(_)
        ));
    }

    #[test]
    fn unreadable_journal_stops() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::create_dir_all(root.join("update")).unwrap();
        std::fs::write(root.join("update").join(msi::JOURNAL), b"{").unwrap();
        let f = Fake {
            busy: Cell::new(Some(false)),
            products: vec![],
            version: String::new(),
            dir: PathBuf::new(),
            running: vec![],
        };
        assert!(matches!(
            preflight_with(&root, &f, Duration::ZERO),
            Gate::Fatal(_)
        ));
        assert!(recover_with(&root, None, &f).is_err());
    }
}
