//! The in-app MSI update end to end, with Windows Installer replaced by
//! scripted facts: check, download, stage, Restart now (revalidation,
//! backup, helper copy), the helper's claim, exact-bytes verification and
//! the outcome of msiexec, then the next start of either version. Against
//! a loopback fixture GitHub with the test verifier (no network).
#![cfg(unix)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use metro_connector::desktop::store;
use metro_connector::desktop::update::install::{self, InstallKind};
use metro_connector::desktop::update::msi::{self, Facts, Phase, Proc, Registered};
use metro_connector::desktop::update::{Busy, Updater, UpdaterConfig};
use metro_release::fetch::Source;
use metro_release::testing::{FixtureRelease, TestVerifier};
use metro_release::{Channel, Version};
use serde_json::Value;

const OLD: &str = "0.9.0";
const NEW: &str = "0.9.1";
const MSI_BYTES: &[u8] = b"a Windows Installer package for 0.9.1";

fn msi_name() -> String {
    format!("Metro-{NEW}-windows-x64.unsigned-preview.msi")
}

type Files = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

fn serve(files: BTreeMap<String, Vec<u8>>) -> (String, Files) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let files = Arc::new(Mutex::new(files));
    let shared = files.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let name = path
                .strip_prefix("/wnzn/metro/releases/latest/download/")
                .or_else(|| path.strip_prefix(&format!("/wnzn/metro/releases/download/v{NEW}/")));
            let body = name.and_then(|n| shared.lock().unwrap().get(n).cloned());
            let (status, body) = match body {
                Some(b) => (200, b),
                None => (404, b"no".to_vec()),
            };
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    (base, files)
}

struct Env {
    _tmp: tempfile::TempDir,
    desktop_root: PathBuf,
    update_dir: PathBuf,
    install_dir: PathBuf,
    base: String,
    files: Files,
}

fn private(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
}

fn installed_files(dir: &Path, version: &str) {
    fs::create_dir_all(dir).unwrap();
    let exe = format!("Metro.exe {version}");
    fs::write(dir.join("Metro.exe"), &exe).unwrap();
    let m = serde_json::json!({"kind": "desktop", "target": "windows-x64", "version": version,
        "files": {"Metro.exe": metro_release::hex(&metro_release::sha256(exe.as_bytes()))}});
    fs::write(dir.join("RELEASE.json"), m.to_string()).unwrap();
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let desktop_root = tmp.path().join("desktop");
    private(&desktop_root);
    let update_dir = desktop_root.join("update");
    private(&update_dir);
    let db = desktop_root.join("local").join("db");
    private(&desktop_root.join("local"));
    private(&db);
    fs::write(db.join("metro.sqlite3"), b"database before the update").unwrap();
    let install_dir = tmp.path().join("Programs").join("Metro Desktop");
    installed_files(&install_dir, OLD);
    let release = FixtureRelease::new(NEW, BTreeMap::from([(msi_name(), MSI_BYTES.to_vec())]));
    let (base, files) = serve(release.files);
    Env {
        _tmp: tmp,
        desktop_root,
        update_dir,
        install_dir,
        base,
        files,
    }
}

fn updater(e: &Env, own: &str) -> Arc<Updater> {
    Updater::new(UpdaterConfig {
        dir: e.update_dir.clone(),
        install_dir: e.install_dir.clone(),
        own: Version::parse(own).unwrap(),
        channel: Channel::Release,
        kind: InstallKind::WindowsMsi {
            dir: e.install_dir.clone(),
        },
        source: Source::loopback_fixture(&e.base).unwrap(),
        verifier: Arc::new(TestVerifier),
        trial: None,
        ldd: PathBuf::from("ldd"),
        trust: None,
    })
}

/// Windows Installer as scripted: the product and registry say `version`.
struct Scripted {
    version: RefCell<String>,
    dir: PathBuf,
    busy: Option<bool>,
}

impl Facts for Scripted {
    fn installer_busy(&self) -> Option<bool> {
        self.busy
    }
    fn installed_versions(&self) -> Option<Vec<String>> {
        Some(vec![
            msi::product_version(&Version::parse(&self.version.borrow()).unwrap()).unwrap(),
        ])
    }
    fn registered(&self) -> Option<Registered> {
        Some(Registered {
            dir: self.dir.clone(),
            version: self.version.borrow().clone(),
            local_runner: Some(true),
            start_menu: Some(true),
        })
    }
    fn running(&self, _: &Proc) -> bool {
        false
    }
    fn msiexec_child_running(&self, _: &Proc) -> Option<bool> {
        Some(false)
    }
}

async fn settle(u: &Updater) -> Value {
    for _ in 0..400 {
        let s = u.state(Busy::default());
        if !matches!(
            s["phase"].as_str(),
            Some("downloading" | "verifying" | "staging" | "checking")
        ) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the download didn't settle");
}

/// Check, download, stage and press Restart now; return the revalidated
/// journal (still `staged`).
async fn stage_and_apply(e: &Env, u: &Arc<Updater>) -> msi::MsiJournal {
    u.check(true).await.unwrap();
    let s = u.state(Busy::default());
    assert_eq!(s["can_update_in_app"], true, "{s}");
    assert!(s.get("reason").is_none(), "{s}");
    assert!(s.get("msi_note").is_none(), "{s}");
    assert_eq!(s["latest"]["version"], NEW);
    u.start_download(NEW).unwrap();
    let s = settle(u).await;
    assert_eq!(s["phase"], "ready", "{s}");
    let staged = msi::load(&e.update_dir).unwrap().unwrap();
    assert_eq!((staged.phase, staged.from.as_str()), (Phase::Staged, OLD));
    assert_eq!(
        fs::read(msi::stage_dir(&e.update_dir, &staged.id).join(msi::PACKAGE)).unwrap(),
        MSI_BYTES
    );
    u.request_apply(NEW, true, Busy::default()).unwrap();
    let req = u.apply_requests().borrow().clone().unwrap();
    let lock = install::Lock::take(&e.update_dir).unwrap();
    let (j, checked) = u.verify_msi_apply(&req, &lock).unwrap();
    assert_eq!(fs::read(&checked.path).unwrap(), MSI_BYTES);
    j
}

/// The old app's handoff: backup, helper copy; returns the `handoff` journal.
fn handoff(e: &Env, mut j: msi::MsiJournal) -> msi::MsiJournal {
    let instance = store::try_lock_instance(&e.desktop_root).unwrap().unwrap();
    let lock = install::Lock::take(&e.update_dir).unwrap();
    msi::begin_handoff(&e.desktop_root, &e.update_dir, &mut j, &lock, &instance).unwrap();
    assert_eq!(j.phase, Phase::Handoff);
    assert!(j.local_db_snapshot);
    let backup = e.update_dir.join(format!("db-snapshot-{}", j.id));
    assert_eq!(
        fs::read(backup.join("metro.sqlite3")).unwrap(),
        b"database before the update"
    );
    let exe = e.install_dir.join("Metro.exe");
    let helper = msi::prepare_helper(&e.update_dir, &j, &exe).unwrap();
    assert_eq!(fs::read(&helper.path).unwrap(), fs::read(&exe).unwrap());
    assert!(
        helper
            .path
            .starts_with(e.update_dir.join(format!("helper-{}", j.id)))
    );
    assert_eq!(msi::load(&e.update_dir).unwrap().as_ref(), Some(&j));
    j
}

/// The helper up to msiexec's exit: claim, precheck, verify, `installing`.
fn helper_runs(e: &Env, id: &str, facts: &Scripted) -> msi::MsiJournal {
    let _instance = store::try_lock_instance(&e.desktop_root).unwrap().unwrap();
    let lock = install::Lock::take(&e.update_dir).unwrap();
    let mut j = msi::claim(&e.update_dir, id, &lock).unwrap();
    let reg = msi::precheck(&j, facts).unwrap();
    let checked = msi::verify_package(&e.update_dir, &j, &TestVerifier, None).unwrap();
    let line =
        msi::msiexec_command_line(&checked.path, &e.update_dir.join(msi::LOG), &reg).unwrap();
    assert!(
        line.contains("/passive") && line.contains("LOCALRUNNER=1"),
        "{line}"
    );
    msi::begin_install(&e.update_dir, &mut j, Proc::current(), &lock).unwrap();
    msi::record_msiexec(
        &e.update_dir,
        &mut j,
        Proc {
            pid: 4242,
            created: 1,
        },
        &lock,
    )
    .unwrap();
    j
}

fn finish(e: &Env, j: &msi::MsiJournal, code: Option<i32>, facts: &Scripted) -> msi::MsiJournal {
    let lock = install::Lock::take(&e.update_dir).unwrap();
    let n = msi::after_msiexec(j, code, facts).expect("the facts match the exit code");
    msi::write_decision(&e.update_dir, j, &n, &lock).unwrap();
    n
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_install_keeps_the_old_version_shows_why_and_allows_a_retry() {
    let e = env();
    let u = updater(&e, OLD);
    let j = handoff(&e, stage_and_apply(&e, &u).await);
    drop(u);
    let facts = Scripted {
        version: RefCell::new(OLD.into()),
        dir: e.install_dir.clone(),
        busy: Some(false),
    };
    let j = helper_runs(&e, &j.id, &facts);
    let n = finish(&e, &j, Some(1602), &facts);
    assert_eq!(n.failure.as_deref(), Some("installer_cancelled"));
    // The old version starts again (msiexec rolled back).
    let u = updater(&e, OLD);
    let s = u.state(Busy::default());
    assert_eq!(s["last_update"]["outcome"], "install_failed", "{s}");
    assert_eq!(s["last_update"]["failure_code"], "installer_cancelled");
    assert!(msi::load(&e.update_dir).unwrap().is_none());
    assert!(!e.update_dir.join(format!("db-snapshot-{}", j.id)).exists());
    assert!(!msi::stage_dir(&e.update_dir, &j.id).exists());
    assert!(!msi::helper_dir(&e.update_dir, &j.id).exists());
    assert_eq!(
        fs::read(e.desktop_root.join("local/db/metro.sqlite3")).unwrap(),
        b"database before the update"
    );
    // Retry.
    u.check(true).await.unwrap();
    u.start_download(NEW).unwrap();
    assert_eq!(settle(&u).await["phase"], "ready");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finished_install_is_committed_at_the_new_versions_first_start() {
    let e = env();
    let u = updater(&e, OLD);
    let j = handoff(&e, stage_and_apply(&e, &u).await);
    drop(u);
    let facts = Scripted {
        version: RefCell::new(OLD.into()),
        dir: e.install_dir.clone(),
        busy: Some(false),
    };
    let j = helper_runs(&e, &j.id, &facts);
    // Windows Installer replaced the files.
    installed_files(&e.install_dir, NEW);
    *facts.version.borrow_mut() = NEW.into();
    let n = finish(&e, &j, Some(0), &facts);
    assert_eq!(n.phase, Phase::Installed);
    // The new version starts: the start decision lets it through.
    assert_eq!(msi::decide(Some(&n), NEW, &facts), msi::Decision::Proceed);
    let u = updater(&e, NEW);
    assert!(u.state(Busy::default())["last_update"].is_null());
    // The backup is kept until the first rendered start.
    assert!(e.update_dir.join(format!("db-snapshot-{}", j.id)).is_dir());
    assert!(u.started());
    let s = u.state(Busy::default());
    assert_eq!(s["last_update"]["outcome"], "updated", "{s}");
    assert_eq!(s["last_update"]["to"], NEW);
    assert!(msi::load(&e.update_dir).unwrap().is_none());
    assert!(!e.update_dir.join(format!("db-snapshot-{}", j.id)).exists());
    assert!(!u.started(), "only once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_helper_is_decided_at_the_next_start() {
    let e = env();
    let u = updater(&e, OLD);
    let j = handoff(&e, stage_and_apply(&e, &u).await);
    drop(u);
    let facts = Scripted {
        version: RefCell::new(OLD.into()),
        dir: e.install_dir.clone(),
        busy: Some(false),
    };
    // The helper dies during msiexec; Windows Installer is still running.
    let j = helper_runs(&e, &j.id, &facts);
    let busy = Scripted {
        busy: Some(true),
        version: RefCell::new(OLD.into()),
        dir: e.install_dir.clone(),
    };
    assert_eq!(msi::decide(Some(&j), OLD, &busy), msi::Decision::Updating);
    // It rolled back: the old version records the failure and keeps going.
    let msi::Decision::Write(n) = msi::decide(Some(&j), OLD, &facts) else {
        panic!("a decision");
    };
    let lock = install::Lock::take(&e.update_dir).unwrap();
    msi::write_decision(&e.update_dir, &j, &n, &lock).unwrap();
    drop(lock);
    let u = updater(&e, OLD);
    assert_eq!(
        u.state(Busy::default())["last_update"]["failure_code"],
        "installer_failed"
    );
    assert!(msi::load(&e.update_dir).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_installs_keep_journal_backup_and_stage() {
    let e = env();
    let u = updater(&e, OLD);
    let j = handoff(&e, stage_and_apply(&e, &u).await);
    drop(u);
    let facts = Scripted {
        version: RefCell::new(OLD.into()),
        dir: e.install_dir.clone(),
        busy: Some(false),
    };
    let j = helper_runs(&e, &j.id, &facts);
    // msiexec said 0, but nothing changed.
    assert_eq!(msi::after_msiexec(&j, Some(0), &facts), None);
    // Registry says the new version, the product the old one.
    let mixed = Scripted {
        version: RefCell::new(NEW.into()),
        dir: e.install_dir.clone(),
        busy: Some(false),
    };
    installed_files(&e.install_dir, NEW);
    struct Mixed<'a>(&'a Scripted);
    impl Facts for Mixed<'_> {
        fn installer_busy(&self) -> Option<bool> {
            Some(false)
        }
        fn installed_versions(&self) -> Option<Vec<String>> {
            Some(vec![
                msi::product_version(&Version::parse(OLD).unwrap()).unwrap(),
            ])
        }
        fn registered(&self) -> Option<Registered> {
            self.0.registered()
        }
        fn running(&self, _: &Proc) -> bool {
            false
        }
        fn msiexec_child_running(&self, _: &Proc) -> Option<bool> {
            Some(false)
        }
    }
    for own in [OLD, NEW] {
        assert_eq!(
            msi::decide(Some(&j), own, &Mixed(&mixed)),
            msi::Decision::Uncertain
        );
    }
    // A start of either version leaves everything as it was.
    for own in [OLD, NEW] {
        let _u = updater(&e, own);
        assert_eq!(msi::load(&e.update_dir).unwrap().as_ref(), Some(&j));
    }
    assert!(e.update_dir.join(format!("db-snapshot-{}", j.id)).is_dir());
    assert!(
        msi::stage_dir(&e.update_dir, &j.id)
            .join(msi::PACKAGE)
            .is_file()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_helper_verifies_the_exact_bytes_again() {
    let e = env();
    let u = updater(&e, OLD);
    let j = handoff(&e, stage_and_apply(&e, &u).await);
    drop(u);
    let package = msi::stage_dir(&e.update_dir, &j.id).join(msi::PACKAGE);
    fs::write(&package, b"swapped installer").unwrap();
    assert!(msi::verify_package(&e.update_dir, &j, &TestVerifier, None).is_err());
    fs::write(&package, MSI_BYTES).unwrap();
    assert!(msi::verify_package(&e.update_dir, &j, &TestVerifier, None).is_ok());
    // Evidence swapped for another signed manifest.
    let sums = msi::stage_dir(&e.update_dir, &j.id).join("SHA256SUMS");
    let mut other = fs::read(&sums).unwrap();
    other.extend_from_slice(b"\n");
    fs::write(&sums, other).unwrap();
    assert!(msi::verify_package(&e.update_dir, &j, &TestVerifier, None).is_err());
    let _ = &e.files;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handoff_whose_helper_never_ran_is_retired_by_the_old_version() {
    let e = env();
    let u = updater(&e, OLD);
    let j = handoff(&e, stage_and_apply(&e, &u).await);
    drop(u);
    let facts = Scripted {
        version: RefCell::new(OLD.into()),
        dir: e.install_dir.clone(),
        busy: Some(false),
    };
    let msi::Decision::Write(n) = msi::decide(Some(&j), OLD, &facts) else {
        panic!("a decision");
    };
    assert_eq!(n.failure.as_deref(), Some("helper_not_started"));
    let lock = install::Lock::take(&e.update_dir).unwrap();
    msi::write_decision(&e.update_dir, &j, &n, &lock).unwrap();
    drop(lock);
    let u = updater(&e, OLD);
    assert_eq!(
        u.state(Busy::default())["last_update"]["outcome"],
        "install_failed"
    );
    // Its claim is gone too: a late helper does nothing.
    let lock = install::Lock::take(&e.update_dir).unwrap();
    assert!(msi::claim(&e.update_dir, &j.id, &lock).is_err());
}

#[test]
fn the_legacy_msi_journal_is_still_gated_separately() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("update");
    private(&dir);
    fs::write(dir.join(install::MSI_JOURNAL), b"{}").unwrap();
    assert!(install::gate_legacy_msi(&dir).is_err());
    assert!(msi::load(&dir).unwrap().is_none());
}

/// `packaging/windows-msi/update_fixture.py release` (the native CI's
/// fixture release) verifies with the test verifier and names the MSI.
#[test]
fn the_native_tests_fixture_release_verifies() {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/windows-msi/update_fixture.py");
    let tmp = tempfile::tempdir().unwrap();
    let msi = tmp.path().join(msi_name());
    fs::write(&msi, MSI_BYTES).unwrap();
    let out = match std::process::Command::new("python3")
        .arg("-I")
        .arg(&script)
        .args(["release", "--version", NEW, "--msi"])
        .arg(&msi)
        .arg("--dest")
        .arg(tmp.path().join("release"))
        .output()
    {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => panic!("{e}"),
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = tmp.path().join("release").join(format!("v{NEW}"));
    let manifest = fs::read(dir.join("SHA256SUMS")).unwrap();
    let bundle = fs::read(dir.join("SHA256SUMS.sigstore.json")).unwrap();
    let release =
        metro_release::release::verify_manifest(&TestVerifier, &manifest, &bundle).unwrap();
    assert_eq!(release.version.to_string(), NEW);
    assert_eq!(release.msi, msi_name());
    release.check_bytes(&msi_name(), MSI_BYTES).unwrap();
}
