//! The Windows script install's in-app update, on the state machine shared
//! with Linux: a ZIP staged into `versions/<V>`, verified again for the
//! trial, committed or rolled back; the stable entry point; the migration
//! gate. Runs on Linux with fakes (the loopback fixture and test verifier).
//! Native Windows behaviour (Installed apps, shortcut, lock handoff) is
//! covered by packaging/windows_update_e2e.py on a Windows runner.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use metro_connector::desktop::update::apply::{self, TrialOutcome, TrialPaths};
use metro_connector::desktop::update::install::{self, InstallKind, entry};
use metro_connector::desktop::update::{Busy, Updater, UpdaterConfig};
use metro_release::fetch::Source;
use metro_release::testing::{FixtureRelease, TestVerifier, release_json, zip};
use metro_release::{Channel, Version};
use serde_json::Value;

const OLD: &str = "0.9.0";
const NEW: &str = "0.9.1";

fn serve(files: BTreeMap<String, Vec<u8>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
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
            let (status, body) = match name.and_then(|n| files.get(n)) {
                Some(b) => (200, b.clone()),
                None => (404, b"no".to_vec()),
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.write_all(&body);
        }
    });
    base
}

fn exec_file(path: &Path, body: &[u8]) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

// Each case in its own process: a sibling's fork can keep a lock or a
// just-written executable open until exec.
macro_rules! isolate {
    ($case:ident) => {
        const CHILD: &str = "METRO_TEST_WINDOWS_SCRIPT_ISOLATED";
        if std::env::var(CHILD).as_deref() != Ok(stringify!($case)) {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", stringify!($case)])
                .env(CHILD, stringify!($case))
                .output()
                .unwrap();
            assert!(
                out.status.success() && String::from_utf8_lossy(&out.stdout).contains("1 passed;"),
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
    };
}

struct Install {
    dir: tempfile::TempDir,
    root: PathBuf,
    desktop_root: PathBuf,
    install_dir: PathBuf,
    ldd: PathBuf,
}

impl Install {
    fn update_dir(&self) -> PathBuf {
        self.desktop_root.join("update")
    }

    fn kind(&self) -> InstallKind {
        InstallKind::WindowsScript {
            root: self.root.clone(),
        }
    }

    fn db(&self) -> Vec<u8> {
        fs::read(self.desktop_root.join("local/db/metro.sqlite3")).unwrap()
    }

    fn paths(&self) -> TrialPaths {
        TrialPaths {
            root: self.root.clone(),
            desktop_root: self.desktop_root.clone(),
            update_dir: self.update_dir(),
        }
    }
}

/// A script install of OLD in the Windows layout (plus the platform
/// pointer the shared code reads here), migrated unless `migrated` is false.
fn install(migrated: bool) -> Install {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Metro");
    let install_dir = root.join("versions").join(OLD);
    exec_file(&install_dir.join(install::LAUNCHER), b"#!/bin/sh\n");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(install_dir.join(install::LOCAL_RUNNER_MARKER), b"").unwrap();
    let marker: &[u8] = if migrated {
        metro_release::installer_lock::MIGRATION
    } else {
        b"Metro desktop per-user install (managed by install-metro-windows).\r\n"
    };
    fs::write(root.join(install::MARKER), marker).unwrap();
    install::set_pointer(&root, OLD).unwrap();
    fs::write(root.join(install::TEXT_POINTER), OLD).unwrap();
    let desktop_root = dir.path().join("desktop");
    fs::create_dir_all(desktop_root.join("local/db")).unwrap();
    metro_connector::desktop::store::prepare_private_dir(&desktop_root.join("update")).unwrap();
    fs::write(desktop_root.join("local/db/metro.sqlite3"), b"original").unwrap();
    // Windows stages never ask ldd; this one would refuse everything.
    let ldd = dir.path().join("fake-ldd");
    exec_file(
        &ldd,
        b"#!/bin/sh\necho \"\tlibwebkitgtk-6.0.so.4 => not found\"\n",
    );
    Install {
        dir,
        root,
        desktop_root,
        install_dir,
        ldd,
    }
}

/// `Metro-NEW-windows-x64.zip` whose launcher runs `launcher` as a script.
fn release(launcher: &str) -> FixtureRelease {
    let launcher = launcher.as_bytes();
    let payload: Vec<(&str, &[u8], u32)> = vec![
        (install::LAUNCHER, launcher, 0o755),
        ("metro-server", b"server", 0o755),
        ("ui/Metro", b"ui", 0o755),
    ];
    let rj = release_json("desktop", "windows-x64", NEW, &payload);
    let mut files: Vec<(&str, &[u8])> = vec![("RELEASE.json", &rj)];
    files.extend(payload.iter().map(|(n, d, _)| (*n, *d)));
    let mut o = BTreeMap::new();
    o.insert(
        format!("Metro-{NEW}-windows-x64.zip"),
        zip(&format!("Metro-{NEW}-windows-x64"), &files),
    );
    FixtureRelease::new(NEW, o)
}

fn updater(i: &Install, base: &str) -> Arc<Updater> {
    Updater::new(UpdaterConfig {
        dir: i.update_dir(),
        install_dir: i.install_dir.clone(),
        own: Version::parse(OLD).unwrap(),
        channel: Channel::Release,
        kind: i.kind(),
        source: Source::loopback_fixture(base).unwrap(),
        verifier: Arc::new(TestVerifier),
        trial: None,
        ldd: i.ldd.clone(),
        trust: None,
    })
}

async fn until_settled(u: &Updater) -> Value {
    for _ in 0..400 {
        let s = u.state(Busy::default());
        if !matches!(
            s["phase"].as_str(),
            Some("downloading" | "verifying" | "staging")
        ) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never settled");
}

/// Check, download, stage and request the restart; then what the launcher
/// does before the trial: take the lock and verify the stage again.
async fn staged_and_verified(
    i: &Install,
    launcher: &str,
) -> (
    install::Lock,
    install::Journal,
    metro_connector::desktop::store::InstanceLock,
) {
    let base = serve(release(launcher).files);
    let u = updater(i, &base);
    let s = u.state(Busy::default());
    assert_eq!(s["current"]["install"], "windows-script");
    assert_eq!(s["can_update_in_app"], true, "{s}");
    u.check(true).await.unwrap();
    u.start_download(NEW).unwrap();
    let s = until_settled(&u).await;
    assert_eq!(s["phase"], "ready", "{s}");
    let staged = i.root.join("versions").join(NEW);
    assert!(staged.join("ui/Metro").is_file());
    assert!(
        staged.join(install::LOCAL_RUNNER_MARKER).is_file(),
        "the local-runner choice is kept"
    );
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
    let mut rx = u.apply_requests();
    u.request_apply(NEW, true, Busy::default()).unwrap();
    let req = rx.borrow_and_update().clone().unwrap();
    let lock = install::Lock::take(&i.root).unwrap();
    let (mut j, file) = u.verify_apply(&req, &lock).unwrap();
    assert_eq!(file.path, staged.join(install::LAUNCHER));
    drop(file);
    let instance = metro_connector::desktop::store::try_lock_instance(&i.desktop_root)
        .unwrap()
        .unwrap();
    apply::begin_trial(&i.paths(), &mut j, &lock, &instance).unwrap();
    assert!(j.local_db_snapshot);
    // ZIP entries carry no Unix mode; the real launcher is a Windows .exe.
    exec_file(
        &staged.join(install::LAUNCHER),
        &fs::read(staged.join(install::LAUNCHER)).unwrap(),
    );
    (lock, j, instance)
}

fn trial_script(i: &Install, then: &str) -> String {
    format!(
        "#!/bin/sh\nprintf migrated > '{}'\n{then}\n",
        i.desktop_root.join("local/db/metro.sqlite3").display()
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zip_update_is_staged_tried_and_committed() {
    isolate!(zip_update_is_staged_tried_and_committed);
    let i = install(true);
    // The trial confirms its first render like the UI does.
    let ok_dir = i.update_dir();
    let launcher = trial_script(
        &i,
        &format!(
            "printf ok > '{}/trial-'\"$2\"'.ok'\nexec sleep 5",
            ok_dir.display()
        ),
    );
    let (lock, mut j, instance) = staged_and_verified(&i, &launcher).await;
    let exe = apply::trial_exe(&i.root, NEW);
    let mut child = apply::spawn_trial(&exe, &j.id).unwrap();
    let out = apply::watch_trial(
        &i.paths(),
        &mut j,
        &lock,
        &instance,
        &mut child,
        Duration::from_secs(20),
    )
    .await;
    let _ = child.kill().await;
    assert_eq!(out, TrialOutcome::Committed);
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(NEW));
    assert_eq!(i.db(), b"migrated", "a committed update keeps the new data");
    drop(lock);
    let done =
        install::take_finished(&i.kind(), &i.update_dir(), &Version::parse(NEW).unwrap()).unwrap();
    assert_eq!(done.outcome, "updated");
    assert!(
        i.root.join("versions").join(OLD).is_dir(),
        "known-good kept"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broken_zip_update_is_rolled_back_and_the_database_restored() {
    isolate!(broken_zip_update_is_rolled_back_and_the_database_restored);
    let i = install(true);
    let (lock, mut j, instance) = staged_and_verified(&i, &trial_script(&i, "exit 3")).await;
    let exe = apply::trial_exe(&i.root, NEW);
    let mut child = apply::spawn_trial(&exe, &j.id).unwrap();
    let out = apply::watch_trial(
        &i.paths(),
        &mut j,
        &lock,
        &instance,
        &mut child,
        Duration::from_secs(20),
    )
    .await;
    assert_eq!(out, TrialOutcome::RolledBack("exited with 3".into()));
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
    assert_eq!(i.db(), b"original", "restored byte for byte");
    assert!(!i.root.join("versions").join(NEW).exists());
    drop(lock);
    let done =
        install::take_finished(&i.kind(), &i.update_dir(), &Version::parse(OLD).unwrap()).unwrap();
    assert_eq!(done.outcome, "rolled_back");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unmigrated_script_install_needs_the_installer_once() {
    isolate!(an_unmigrated_script_install_needs_the_installer_once);
    let i = install(false);
    let base = serve(release("#!/bin/sh\n").files);
    let u = updater(&i, &base);
    let s = u.state(Busy::default());
    assert_eq!(
        (
            s["can_check"].as_bool(),
            s["can_update_in_app"].as_bool(),
            s["reason"].as_str()
        ),
        (Some(true), Some(false), Some("manual_migration_required"))
    );
    u.check(true).await.unwrap();
    assert_eq!(u.state(Busy::default())["result"], "available");
    assert_eq!(u.start_download(NEW).unwrap_err().code, "not_in_app");
    assert!(!i.root.join("versions").join(NEW).exists());
    // The stage itself refuses too, under the lock.
    let rel = release("#!/bin/sh\n");
    let dl = i.dir.path().join("download.zip");
    fs::write(&dl, &rel.files[&format!("Metro-{NEW}-windows-x64.zip")]).unwrap();
    let verified = metro_release::release::verify_manifest(
        &TestVerifier,
        &rel.files["SHA256SUMS"],
        &rel.files["SHA256SUMS.sigstore.json"],
    )
    .unwrap();
    let err = install::stage(
        &i.kind(),
        &i.update_dir(),
        &i.install_dir,
        &Version::parse(OLD).unwrap(),
        &verified,
        &dl,
        &i.ldd,
    )
    .unwrap_err();
    assert_eq!(err.code, "not_in_app");
    assert!(!i.root.join(install::JOURNAL).exists());
}

#[test]
fn entry_point_starts_only_the_current_version() {
    let i = install(true);
    let exe = entry::path(&i.root);
    assert_eq!(entry::root_of(&exe), Some(i.root.clone()));
    assert_eq!(
        entry::target(&i.root).unwrap(),
        i.install_dir.join(install::LAUNCHER)
    );
    // Only a launcher directly in a marked root with versions and a pointer.
    assert_eq!(entry::root_of(&i.install_dir.join(install::LAUNCHER)), None);
    assert_eq!(entry::root_of(&i.root.join("Other")), None);

    // Refresh copies the named version's launcher in with one rename.
    assert!(!entry::present(&i.root));
    entry::refresh(&i.root, OLD).unwrap();
    assert_eq!(fs::read(&exe).unwrap(), b"#!/bin/sh\n");
    assert!(entry::refresh(&i.root, NEW).is_err(), "not installed");
    assert!(entry::refresh(&i.root, "../x").is_err());
    assert!(
        fs::read_dir(&i.root)
            .unwrap()
            .all(|e| { !e.unwrap().file_name().to_string_lossy().ends_with(".tmp") })
    );

    // A start of a stale version forwards to the current one, never the
    // reverse, and only in a migrated install.
    let new_dir = i.root.join("versions").join(NEW);
    exec_file(&new_dir.join(install::LAUNCHER), b"#!/bin/sh\nnew\n");
    let (old, new) = (Version::parse(OLD).unwrap(), Version::parse(NEW).unwrap());
    assert_eq!(entry::forward_target(&i.install_dir, &old), None);
    assert_eq!(
        entry::forward_target(&new_dir, &new),
        Some(i.install_dir.join(install::LAUNCHER))
    );
    fs::write(i.root.join(install::TEXT_POINTER), format!("{NEW}\r\n")).unwrap();
    assert_eq!(
        entry::target(&i.root).unwrap(),
        new_dir.join(install::LAUNCHER)
    );
    assert_eq!(
        entry::forward_target(&i.install_dir, &old),
        Some(new_dir.join(install::LAUNCHER))
    );
    assert_eq!(entry::forward_target(&new_dir, &new), None);
    fs::write(i.root.join(install::MARKER), b"managed\n").unwrap();
    assert_eq!(entry::forward_target(&i.install_dir, &old), None);
    assert!(entry::target(&i.root).is_ok(), "the entry works unmigrated");

    // A damaged pointer or a version folder that is a link: refused.
    for bad in ["", "0.9", "../0.9.1", "0.9.1/../../x"] {
        fs::write(i.root.join(install::TEXT_POINTER), bad).unwrap();
        assert!(entry::target(&i.root).is_err(), "{bad:?}");
    }
    fs::write(i.root.join(install::TEXT_POINTER), "0.9.2").unwrap();
    std::os::unix::fs::symlink(&new_dir, i.root.join("versions/0.9.2")).unwrap();
    assert!(entry::target(&i.root).is_err(), "linked version folder");
    fs::remove_file(i.root.join(install::MARKER)).unwrap();
    fs::write(i.root.join(install::TEXT_POINTER), NEW).unwrap();
    assert!(entry::target(&i.root).is_err(), "unmarked folder");
}

#[test]
fn text_pointer_is_one_version() {
    let i = install(true);
    for (raw, want) in [
        ("0.9.0", Some(OLD)),
        ("0.9.0\r\n", Some(OLD)),
        (" 0.9.0 ", Some(OLD)),
        ("v0.9.0", None),
        ("0.9.0\n0.9.1", None),
        ("", None),
    ] {
        fs::write(i.root.join(install::TEXT_POINTER), raw).unwrap();
        assert_eq!(
            install::read_text_pointer(&i.root).as_deref(),
            want,
            "{raw:?}"
        );
    }
    fs::write(i.root.join(install::TEXT_POINTER), "1".repeat(200)).unwrap();
    assert_eq!(install::read_text_pointer(&i.root), None, "oversized");
}
