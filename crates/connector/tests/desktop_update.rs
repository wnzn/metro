//! The desktop updater: detection, check, download, stage, apply request,
//! commit, rollback, crash recovery and the trial watchdog, against a
//! loopback fixture GitHub with the test verifier (no network).
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use metro_connector::desktop::bridge;
use metro_connector::desktop::core::{Desktop, DesktopConfig};
use metro_connector::desktop::update::apply::{self, TrialOutcome, TrialPaths};
use metro_connector::desktop::update::install::{self, InstallKind, Journal, JournalPhase};
use metro_connector::desktop::update::{Busy, InstallFailure, Updater, UpdaterConfig};
use metro_release::fetch::Source;
use metro_release::testing::{FixtureRelease, TestVerifier, release_json, tar_gz};
use metro_release::{Channel, Version};
use serde_json::{Value, json};

const OLD: &str = "0.9.0";
const NEW: &str = "0.9.1";

type Files = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;
/// A canned response: path, status, headers, body.
type Canned = (String, u16, Vec<(String, String)>, Vec<u8>);

struct Fixture {
    base: String,
    hits: Arc<AtomicUsize>,
    files: Files,
}

/// Serves `latest/download/<name>`, `download/v<NEW>/<name>` and the notes API.
fn serve(files: BTreeMap<String, Vec<u8>>, extra: Vec<Canned>) -> Fixture {
    serve_version(NEW, files, extra)
}

fn serve_version(
    version: &'static str,
    files: BTreeMap<String, Vec<u8>>,
    extra: Vec<Canned>,
) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let files = Arc::new(Mutex::new(files));
    let (counter, shared) = (hits.clone(), files.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
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
            let mut response = (404u16, vec![], b"no".to_vec());
            if let Some((_, s, h, b)) = extra.iter().find(|(p, ..)| *p == path) {
                response = (*s, h.clone(), b.clone());
            } else if path == format!("/api/repos/wnzn/metro/releases/tags/v{version}") {
                let notes = json!({"body": "Fixes and **notes**", "html_url": format!("https://github.com/wnzn/metro/releases/tag/v{version}")});
                response = (200, vec![], notes.to_string().into_bytes());
            } else {
                let name = path
                    .strip_prefix("/wnzn/metro/releases/latest/download/")
                    .or_else(|| {
                        path.strip_prefix(&format!("/wnzn/metro/releases/download/v{version}/"))
                    });
                if let Some(b) = name.and_then(|n| shared.lock().unwrap().get(n).cloned()) {
                    response = (200, vec![], b);
                }
            }
            let (status, headers, body) = response;
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            let _ = stream.write_all(format!("{head}\r\n").as_bytes());
            let _ = stream.write_all(&body);
        }
    });
    Fixture { base, hits, files }
}

fn exec_file(path: &Path, body: &[u8]) {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn desktop_archive(version: &str, kind: &str) -> Vec<u8> {
    let payload: Vec<(&str, &[u8], u32)> = vec![
        ("Metro", b"#!/bin/sh\nexit 0\n", 0o755),
        ("metro-server", b"#!/bin/sh\nexit 0\n", 0o755),
        ("ui/Metro", b"#!/bin/sh\nexit 0\n", 0o755),
        ("install-desktop", b"#!/bin/sh\nexit 0\n", 0o755),
    ];
    let rj = release_json(kind, "linux-x64", version, &payload);
    let mut all = vec![("RELEASE.json", rj.as_slice(), 0o644)];
    all.extend(payload);
    tar_gz(&format!("Metro-{version}-linux-x64"), &all)
}

fn release(archive: Vec<u8>) -> FixtureRelease {
    let mut o = BTreeMap::new();
    o.insert(format!("Metro-{NEW}-linux-x64.tar.gz"), archive);
    FixtureRelease::new(NEW, o)
}

struct Install {
    _dir: tempfile::TempDir,
    root: PathBuf,
    desktop_root: PathBuf,
    install_dir: PathBuf,
    ldd: PathBuf,
}

/// A Linux script install of OLD, a desktop root with a local DB, a fake ldd.
fn install() -> Install {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("metro-desktop-app");
    let install_dir = root.join("versions").join(OLD);
    exec_file(&install_dir.join("Metro"), b"#!/bin/sh\n");
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(install_dir.join(install::LOCAL_RUNNER_MARKER), b"").unwrap();
    fs::write(
        root.join(install::MARKER),
        metro_release::installer_lock::MIGRATION,
    )
    .unwrap();
    install::set_pointer(&root, OLD).unwrap();
    let desktop_root = dir.path().join("desktop");
    fs::create_dir_all(desktop_root.join("local/db")).unwrap();
    metro_connector::desktop::store::prepare_private_dir(&desktop_root.join("update")).unwrap();
    fs::write(
        desktop_root.join("local/db/metro.sqlite3"),
        b"original database",
    )
    .unwrap();
    fs::write(
        desktop_root.join("local/db/metro.sqlite3-wal"),
        b"original wal",
    )
    .unwrap();
    let ldd = dir.path().join("fake-ldd");
    exec_file(&ldd, b"#!/bin/sh\necho \"\tlibc.so.6 => /lib/libc.so.6\"\n");
    Install {
        _dir: dir,
        root,
        desktop_root,
        install_dir,
        ldd,
    }
}

#[cfg(target_os = "linux")]
#[test]
fn desktop_updater_and_published_uninstaller_exclude_each_other() {
    use std::process::{Command, Stdio};
    let i = install();
    let published = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/test-fixtures/install-metro-linux-0.4.0.sh");
    let command = || {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg(&published)
            .arg("uninstall")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", i._dir.path())
            .env("METRO_INSTALL_DIR", &i.root);
        cmd
    };
    let lock = install::Lock::take(&i.root).unwrap();
    let refused = command().output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("an install is running"));
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
    drop(lock);

    // Stop the published script just after it took its own lock.
    exec_file(
        &i.install_dir.join("install-desktop"),
        b"#!/bin/sh\nprintf 'locked\\n' >&2\nread -r done\n",
    );
    let mut holder = command()
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut marker = String::new();
    BufReader::new(holder.stderr.take().unwrap())
        .read_line(&mut marker)
        .unwrap();
    let refused = install::Lock::take(&i.root);
    drop(holder.stdin.take());
    assert!(holder.wait().unwrap().success());
    assert_eq!(marker, "locked\n");
    assert_eq!(refused.err().unwrap().code, "installer_busy");
    assert!(!i.root.exists());
}

fn updater(i: &Install, f: &Fixture, channel: Channel) -> Arc<Updater> {
    updater_with_verifier(i, f, channel, Arc::new(TestVerifier))
}

fn updater_with_verifier(
    i: &Install,
    f: &Fixture,
    channel: Channel,
    verifier: Arc<dyn metro_release::ReleaseVerifier>,
) -> Arc<Updater> {
    let own = Version::parse(OLD).unwrap();
    Updater::new(UpdaterConfig {
        dir: i.desktop_root.join("update"),
        install_dir: i.install_dir.clone(),
        own,
        channel,
        kind: install::detect(&i.install_dir, &own, channel, None),
        source: Source::loopback_fixture(&f.base).unwrap(),
        verifier,
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

#[test]
fn detection() {
    let i = install();
    let own = Version::parse(OLD).unwrap();
    let r = Channel::Release;
    assert_eq!(
        install::detect(&i.install_dir, &own, r, None),
        InstallKind::LinuxInstaller {
            root: i.root.clone()
        }
    );
    assert_eq!(
        install::detect(&i.install_dir, &own, Channel::Source, None),
        InstallKind::Unsupported("development_build")
    );
    assert_eq!(
        install::detect(&i.install_dir, &own, Channel::Preview, None),
        InstallKind::Unsupported("preview_build")
    );
    // A moved or extracted copy is portable.
    assert_eq!(
        install::detect(&i.root, &own, r, None),
        InstallKind::Portable
    );
    // The pointer names another version: portable, unless this is the trial.
    let new_dir = i.root.join("versions").join(NEW);
    exec_file(&new_dir.join("Metro"), b"");
    let new = Version::parse(NEW).unwrap();
    assert_eq!(
        install::detect(&new_dir, &new, r, None),
        InstallKind::Portable
    );
    let j = Journal {
        version: 1,
        id: "abcdef0123456789".into(),
        from: OLD.into(),
        to: NEW.into(),
        phase: JournalPhase::Trial,
        outcome: None,
        detail: None,
        local_db_snapshot: false,
        backup_manifest: None,
        watchdog_pid: None,
        source_pid: None,
        reported: false,
        started_at: 0,
    };
    install::write_journal(&i.root.join(install::JOURNAL), &j).unwrap();
    assert_eq!(
        install::detect(&new_dir, &new, r, Some("abcdef0123456789")),
        InstallKind::LinuxInstaller {
            root: i.root.clone()
        }
    );
    assert_eq!(
        install::detect(&new_dir, &new, r, Some("0000000000000000")),
        InstallKind::Portable
    );
    // No marker: portable.
    fs::remove_file(i.root.join(install::MARKER)).unwrap();
    assert_eq!(
        install::detect(&i.install_dir, &own, r, None),
        InstallKind::Portable
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_download_stage_and_apply_request() {
    let i = install();
    let f = serve(release(desktop_archive(NEW, "desktop")).files, vec![]);
    let u = updater(&i, &f, Channel::Release);
    let s = u.state(Busy::default());
    assert_eq!(
        (s["can_check"].as_bool(), s["can_update_in_app"].as_bool()),
        (Some(true), Some(true))
    );
    assert_eq!(s["current"]["install"], "linux-installer");
    assert_eq!(s["result"], "never");
    assert_eq!(s["auto_check"], true);

    u.check(true).await.unwrap();
    let s = u.state(Busy::default());
    assert_eq!(s["result"], "available", "{s}");
    assert_eq!(s["latest"]["version"], NEW);
    assert_eq!(s["latest"]["notes"], "Fixes and **notes**");
    assert_eq!(
        s["latest"]["release_url"],
        format!("https://github.com/wnzn/metro/releases/tag/v{NEW}")
    );
    // Nothing was downloaded by the check.
    assert!(!i.root.join("versions").join(NEW).exists());

    // Only the offered version, only once.
    assert_eq!(u.start_download("0.9.2").unwrap_err().code, "not_latest");
    assert_eq!(
        u.request_apply(NEW, true, Busy::default())
            .unwrap_err()
            .code,
        "not_ready"
    );
    u.start_download(NEW).unwrap();
    let s = until_settled(&u).await;
    assert_eq!(s["phase"], "ready", "{s}");
    let staged = i.root.join("versions").join(NEW);
    assert!(staged.join("Metro").is_file() && staged.join("ui/Metro").is_file());
    assert!(
        staged.join(install::LOCAL_RUNNER_MARKER).is_file(),
        "the local-runner choice is kept"
    );
    assert_eq!(
        install::read_pointer(&i.root).as_deref(),
        Some(OLD),
        "nothing visible changed"
    );
    let j = install::read_journal(&i.root.join(install::JOURNAL)).unwrap();
    assert_eq!(
        (j.phase.as_str(), j.from.as_str(), j.to.as_str()),
        ("staged", OLD, NEW)
    );
    assert!(!i.root.join(".lock").exists());

    // Apply: busy work needs confirmation; then the launcher gets the request.
    let busy = Busy {
        serving_connections: 1,
        local_workspace: false,
    };
    assert_eq!(
        u.request_apply(NEW, false, busy).unwrap_err().code,
        "would_interrupt"
    );
    let mut rx = u.apply_requests();
    u.request_apply(NEW, true, busy).unwrap();
    let req = rx.borrow_and_update().clone().unwrap();
    assert_eq!(
        (req.version.to_string(), req.journal_id),
        (NEW.to_string(), j.id.clone())
    );
    assert_eq!(u.state(Busy::default())["phase"], "applying");

    // A restart before applying offers the staged version again.
    drop(u);
    let u = updater(&i, &f, Channel::Release);
    assert_eq!(u.state(Busy::default())["phase"], "ready");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_and_schedule_rules() {
    // Source builds never make a request.
    let i = install();
    let f = serve(release(desktop_archive(NEW, "desktop")).files, vec![]);
    let u = updater(&i, &f, Channel::Source);
    assert_eq!(u.check(true).await.unwrap_err().code, "not_for_this_build");
    let s = u.state(Busy::default());
    assert_eq!(
        (s["can_check"].as_bool(), s["reason"].as_str()),
        (Some(false), Some("development_build"))
    );
    assert_eq!(f.hits.load(Ordering::SeqCst), 0);

    // Automatic checks skip within 20 hours; manual ones don't.
    let u = updater(&i, &f, Channel::Release);
    u.check(false).await.unwrap();
    let after_first = f.hits.load(Ordering::SeqCst);
    assert!(after_first > 0);
    u.check(false).await.unwrap();
    assert_eq!(
        f.hits.load(Ordering::SeqCst),
        after_first,
        "automatic check within 20 h"
    );
    u.check(true).await.unwrap();
    assert!(f.hits.load(Ordering::SeqCst) > after_first);
    // The switch persists.
    u.set_auto_check(false).unwrap();
    let u2 = updater(&i, &f, Channel::Release);
    assert_eq!(u2.state(Busy::default())["auto_check"], false);

    // Rate limited: remembered, and even a manual check waits.
    let i = install();
    let limited = serve(
        BTreeMap::new(),
        vec![(
            "/wnzn/metro/releases/latest/download/SHA256SUMS".into(),
            429,
            vec![("Retry-After".into(), "600".into())],
            vec![],
        )],
    );
    let u = updater(&i, &limited, Channel::Release);
    u.check(true).await.unwrap();
    assert_eq!(u.state(Busy::default())["error"], "rate_limited");
    assert_eq!(u.check(true).await.unwrap_err().status, 429);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_downloads_change_nothing() {
    // A tampered archive.
    let i = install();
    let rel = release(desktop_archive(NEW, "desktop"));
    let f = serve(rel.files.clone(), vec![]);
    let u = updater(&i, &f, Channel::Release);
    u.check(true).await.unwrap();
    f.files.lock().unwrap().insert(
        format!("Metro-{NEW}-linux-x64.tar.gz"),
        desktop_archive(NEW, "desktop2"),
    );
    u.start_download(NEW).unwrap();
    let s = until_settled(&u).await;
    assert_eq!(
        (s["phase"].as_str(), s["failure"]["code"].as_str()),
        (Some("failed"), Some("not_verified")),
        "{s}"
    );
    assert!(!i.root.join("versions").join(NEW).exists());

    // A correctly signed archive of the wrong kind.
    let i = install();
    let f = serve(release(desktop_archive(NEW, "runner")).files, vec![]);
    let u = updater(&i, &f, Channel::Release);
    u.check(true).await.unwrap();
    u.start_download(NEW).unwrap();
    let s = until_settled(&u).await;
    assert_eq!(s["failure"]["code"], "archive", "{s}");
    assert!(!i.root.join("versions").join(NEW).exists());
    assert!(fs::read_dir(&i.root).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".stage.")
    }));

    // Missing system libraries: the exact list, nothing moved.
    let i = install();
    exec_file(
        &i.ldd,
        b"#!/bin/sh\necho \"\tlibwebkitgtk-6.0.so.4 => not found\"\n",
    );
    let f = serve(release(desktop_archive(NEW, "desktop")).files, vec![]);
    let u = updater(&i, &f, Channel::Release);
    u.check(true).await.unwrap();
    u.start_download(NEW).unwrap();
    let s = until_settled(&u).await;
    assert_eq!(s["failure"]["code"], "missing_libraries");
    assert!(
        s["failure"]["message"]
            .as_str()
            .unwrap()
            .contains("libwebkitgtk-6.0.so.4")
    );
    assert!(!i.root.join("versions").join(NEW).exists());

    // A busy installer lock (the script's).
    let i = install();
    fs::create_dir(i.root.join(".lock")).unwrap();
    let f = serve(release(desktop_archive(NEW, "desktop")).files, vec![]);
    let u = updater(&i, &f, Channel::Release);
    u.check(true).await.unwrap();
    u.start_download(NEW).unwrap();
    assert_eq!(until_settled(&u).await["failure"]["code"], "installer_busy");
    assert!(i.root.join(".lock").exists());
}

fn staged_journal(i: &Install) -> Journal {
    exec_file(
        &i.root.join("versions").join(NEW).join("Metro"),
        b"#!/bin/sh\n",
    );
    let j = Journal {
        version: 1,
        id: "0123456789abcdef".into(),
        from: OLD.into(),
        to: NEW.into(),
        phase: JournalPhase::Staged,
        outcome: None,
        detail: None,
        local_db_snapshot: false,
        backup_manifest: None,
        watchdog_pid: None,
        source_pid: None,
        reported: false,
        started_at: 0,
    };
    install::write_journal(&i.root.join(install::JOURNAL), &j).unwrap();
    j
}

fn paths(i: &Install) -> TrialPaths {
    TrialPaths {
        root: i.root.clone(),
        desktop_root: i.desktop_root.clone(),
        update_dir: i.desktop_root.join("update"),
    }
}

fn db(i: &Install) -> (Vec<u8>, Option<Vec<u8>>) {
    let d = i.desktop_root.join("local/db");
    (
        fs::read(d.join("metro.sqlite3")).unwrap(),
        fs::read(d.join("metro.sqlite3-wal")).ok(),
    )
}

/// The new version "migrates" the DB, then behaves as `script` says.
fn trial_stub(i: &Install, update_dir: &Path, id: &str, then: &str) -> PathBuf {
    let db = i.desktop_root.join("local/db");
    let exe = i.root.join("versions").join(NEW).join("Metro");
    let script = format!(
        "#!/bin/sh\nprintf migrated > '{}'\nrm -f '{}'\n{then}\n",
        db.join("metro.sqlite3").display(),
        db.join("metro.sqlite3-wal").display(),
    )
    .replace(
        "$OK",
        &update_dir
            .join(format!("trial-{id}.ok"))
            .display()
            .to_string(),
    );
    exec_file(&exe, script.as_bytes());
    exe
}

fn begin(
    p: &TrialPaths,
    j: &mut Journal,
    lock: &install::Lock,
) -> metro_connector::desktop::store::InstanceLock {
    let instance = metro_connector::desktop::store::try_lock_instance(&p.desktop_root)
        .unwrap()
        .unwrap();
    apply::begin_trial(p, j, lock, &instance).unwrap();
    instance
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trial_confirmed_commits() {
    let i = install();
    let mut j = staged_journal(&i);
    let p = paths(&i);
    fs::create_dir_all(&p.update_dir).unwrap();
    let lock = install::Lock::take(&p.root).unwrap();
    let instance = begin(&p, &mut j, &lock);
    assert!(j.local_db_snapshot);
    let exe = trial_stub(&i, &p.update_dir, &j.id, "printf ok > '$OK'\nsleep 5");
    let mut child = apply::spawn_trial(&exe, &j.id).unwrap();
    let out = apply::watch_trial(
        &p,
        &mut j,
        &lock,
        &instance,
        &mut child,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(out, TrialOutcome::Committed);
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(NEW));
    let jj = install::read_journal(&i.root.join(install::JOURNAL)).unwrap();
    assert_eq!(
        (jj.phase.as_str(), jj.outcome.as_deref()),
        ("committed", Some("updated"))
    );
    let _ = child.kill().await;
    drop(lock);
    // The next start of the new version reports and cleans up.
    let new = Version::parse(NEW).unwrap();
    let done = install::take_finished(
        &InstallKind::LinuxInstaller {
            root: i.root.clone(),
        },
        &p.update_dir,
        &new,
    )
    .unwrap();
    assert_eq!(
        (done.from.as_str(), done.to.as_str(), done.outcome.as_str()),
        (OLD, NEW, "updated")
    );
    assert!(
        i.root.join("versions").join(OLD).exists(),
        "the last known-good copy is kept"
    );
    assert!(!p.update_dir.join(format!("db-snapshot-{}", j.id)).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trial_crash_rolls_back_and_restores_the_database() {
    let i = install();
    let before = db(&i);
    let mut j = staged_journal(&i);
    let p = paths(&i);
    fs::create_dir_all(&p.update_dir).unwrap();
    let lock = install::Lock::take(&p.root).unwrap();
    let instance = begin(&p, &mut j, &lock);
    let exe = trial_stub(&i, &p.update_dir, &j.id, "exit 3");
    let mut child = apply::spawn_trial(&exe, &j.id).unwrap();
    let out = apply::watch_trial(
        &p,
        &mut j,
        &lock,
        &instance,
        &mut child,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(out, TrialOutcome::RolledBack("exited with 3".into()));
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
    assert_eq!(db(&i), before, "database restored byte for byte");
    assert!(!i.root.join("versions").join(NEW).exists());
    drop(lock);
    let own = Version::parse(OLD).unwrap();
    let done = install::take_finished(
        &InstallKind::LinuxInstaller {
            root: i.root.clone(),
        },
        &p.update_dir,
        &own,
    )
    .unwrap();
    assert_eq!(done.outcome, "rolled_back");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bundled_server_that_outlives_the_trial_blocks_restore_and_other_launchers() {
    use metro_connector::desktop::{local::LocalWorkspaceHost, store};
    use metro_connector::desktop_local::LocalServer;
    use std::process::Stdio;
    const ROOT: &str = "METRO_TEST_ORPHAN_TRIAL_ROOT";
    const CASE: &str =
        "a_bundled_server_that_outlives_the_trial_blocks_restore_and_other_launchers";
    if let Some(root) = std::env::var_os(ROOT).map(PathBuf::from) {
        let instance = Arc::new(store::InstanceLock::adopt_trial(&root).unwrap());
        let local = root.join("local");
        let host = LocalServer::new(root.join("server"), local.clone()).with_instance(instance);
        tokio::spawn(async move {
            let _ = host.open().await;
        });
        let signal = local.join("db/orphan.pid");
        tokio::time::timeout(Duration::from_secs(8), async {
            while !signal.is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        std::process::exit(3); // no graceful workspace shutdown or reaping
    }

    let i = install();
    let p = paths(&i);
    let mut j = staged_journal(&i);
    let lock = install::Lock::take(&p.root).unwrap();
    let instance = begin(&p, &mut j, &lock);
    exec_file(
        &p.desktop_root.join("server"),
        br#"#!/usr/bin/env python3
import os, pathlib, signal, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
db = pathlib.Path(os.environ["METRO_DB"])
db.write_bytes(b"trial write")
pid = db.with_name("orphan.pid")
tmp = db.with_name("orphan.tmp")
tmp.write_text(str(os.getpid()))
tmp.rename(pid)
time.sleep(30)
"#,
    );
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CASE])
        .env(ROOT, &p.desktop_root)
        .stdin(instance.trial_input().unwrap())
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(12), child.wait())
            .await
            .unwrap()
            .unwrap()
            .code(),
        Some(3)
    );
    struct Orphan(i32);
    impl Drop for Orphan {
        fn drop(&mut self) {
            // SAFETY: this PID belongs to the server fixture we just started.
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
            }
        }
    }
    let orphan = Orphan(
        fs::read_to_string(p.desktop_root.join("local/db/orphan.pid"))
            .unwrap()
            .parse()
            .unwrap(),
    );
    let other_can_write = || {
        std::process::Command::new("flock")
            .args(["-n", "-E", "17"])
            .arg(p.desktop_root.join("instance.lock"))
            .args(["/bin/true"])
            .status()
            .unwrap()
            .code()
    };
    assert_eq!(
        other_can_write(),
        Some(17),
        "an old launcher could become primary"
    );
    assert_eq!(
        apply::watch_trial(
            &p,
            &mut j,
            &lock,
            &instance,
            &mut child,
            Duration::from_secs(1)
        )
        .await,
        TrialOutcome::NeedsRecovery
    );
    assert_eq!(db(&i).0, b"trial write");
    assert_eq!(
        install::read_journal(&p.root.join(install::JOURNAL)),
        Some(j.clone())
    );
    assert!(
        p.update_dir
            .join(format!("db-snapshot-{}/metro.sqlite3", j.id))
            .is_file()
    );
    assert!(
        apply::RootGate::read(&p.desktop_root)
            .unwrap()
            .unwrap()
            .matches(&p.root, &j.id)
    );
    drop(instance); // the watchdog is gone as well
    assert_eq!(
        other_can_write(),
        Some(17),
        "the orphan did not retain the old instance lock"
    );
    drop(orphan);
    drop(lock);
    assert_eq!(
        install::recover(
            &p.root,
            &p.desktop_root,
            &p.update_dir,
            &Version::parse(OLD).unwrap()
        ),
        install::Recovery::Blocked
    );
    assert_eq!(db(&i).0, b"trial write");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trial_timeout_kills_and_rolls_back() {
    let i = install();
    let before = db(&i);
    let mut j = staged_journal(&i);
    let p = paths(&i);
    fs::create_dir_all(&p.update_dir).unwrap();
    let lock = install::Lock::take(&p.root).unwrap();
    let instance = begin(&p, &mut j, &lock);
    let exe = trial_stub(&i, &p.update_dir, &j.id, "exec sleep 60");
    let mut child = apply::spawn_trial(&exe, &j.id).unwrap();
    let out = apply::watch_trial(
        &p,
        &mut j,
        &lock,
        &instance,
        &mut child,
        Duration::from_millis(800),
    )
    .await;
    assert!(matches!(out, TrialOutcome::RolledBack(_)));
    assert!(child.try_wait().unwrap().is_some(), "the trial was killed");
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
    assert_eq!(db(&i), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trial_closed_at_once_is_not_committed() {
    let i = install();
    let before = db(&i);
    let mut j = staged_journal(&i);
    let p = paths(&i);
    fs::create_dir_all(&p.update_dir).unwrap();
    let lock = install::Lock::take(&p.root).unwrap();
    let instance = begin(&p, &mut j, &lock);
    let exe = trial_stub(&i, &p.update_dir, &j.id, "exit 0");
    let mut child = apply::spawn_trial(&exe, &j.id).unwrap();
    assert_eq!(
        apply::watch_trial(
            &p,
            &mut j,
            &lock,
            &instance,
            &mut child,
            Duration::from_secs(10)
        )
        .await,
        TrialOutcome::NotConfirmed
    );
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
    // Whatever the new server wrote before it was closed is undone.
    assert_eq!(db(&i), before);
    assert!(!i.root.join("versions").join(NEW).exists());
    let j = install::read_journal(&i.root.join(install::JOURNAL)).unwrap();
    assert_eq!(
        (j.phase.as_str(), j.outcome.as_deref()),
        ("failed", Some("not_confirmed"))
    );
}

#[test]
fn legacy_msi_failure_does_not_prove_the_installer_stopped() {
    let i = install();
    let update_dir = i.desktop_root.join("update");
    let backup = update_dir.join("db-snapshot");
    fs::create_dir_all(&backup).unwrap();
    fs::write(backup.join("metro.sqlite3"), b"kept").unwrap();
    let before = db(&i);
    let mut j = staged_journal(&i);
    j.phase = JournalPhase::Failed;
    j.outcome = Some("install_failed".into());
    j.local_db_snapshot = true;
    let path = update_dir.join(install::MSI_JOURNAL);
    install::write_journal(&path, &j).unwrap();
    assert!(install::gate_legacy_msi(&update_dir).is_err());
    assert!(
        install::take_finished(
            &InstallKind::Portable,
            &update_dir,
            &Version::parse(OLD).unwrap()
        )
        .is_none()
    );
    assert_eq!(install::read_journal(&path), Some(j.clone()));
    assert_eq!(db(&i), before);
    assert_eq!(fs::read(backup.join("metro.sqlite3")).unwrap(), b"kept");
    j.phase = JournalPhase::Committed;
    j.outcome = Some("updated".into());
    install::write_journal(&path, &j).unwrap();
    install::gate_legacy_msi(&update_dir).unwrap();
    assert!(backup.is_dir());
}

#[test]
fn msi_is_hashed_again_before_it_runs() {
    let i = install();
    let j = staged_journal(&i);
    let update_dir = i.desktop_root.join("update");
    let assets = update_dir.join(format!("stage-{}", j.id));
    fs::create_dir_all(&assets).unwrap();
    install::write_journal(&update_dir.join(install::MSI_JOURNAL), &j).unwrap();
    let fixture = release(desktop_archive(NEW, "desktop"));
    let manifest = &fixture.files["SHA256SUMS"];
    let bundle = &fixture.files["SHA256SUMS.sigstore.json"];
    let release = metro_release::release::verify_manifest(&TestVerifier, manifest, bundle).unwrap();
    let msi = assets.join("package.msi");
    fs::write(&msi, &fixture.files[&release.msi]).unwrap();
    let lock = install::Lock::take(&update_dir).unwrap();
    let kind = InstallKind::WindowsMsi {
        dir: i.install_dir.clone(),
    };
    let verify =
        || install::verify_staged(&kind, &update_dir, &j, &TestVerifier, Some(manifest), &lock);
    assert!(verify().is_err(), "no signed evidence");
    fs::write(assets.join("SHA256SUMS"), manifest).unwrap();
    fs::write(assets.join("SHA256SUMS.sigstore.json"), bundle).unwrap();
    assert_eq!(verify().unwrap().path, msi);
    fs::write(&msi, b"swapped installer").unwrap();
    assert!(verify().is_err(), "changed installer");
    fs::remove_file(&msi).unwrap();
    assert!(verify().is_err(), "missing installer");
}

#[test]
fn dead_msi_helper_never_unblocks_startup_or_consumes_a_backup() {
    let i = install();
    let original = db(&i);
    let dir = i.desktop_root.join("update");
    let path = dir.join(install::MSI_JOURNAL);
    let mut j = staged_journal(&i);
    j.local_db_snapshot = true;
    j.source_pid = Some(i32::MAX as u32 - 2);
    j.watchdog_pid = Some(i32::MAX as u32 - 1);
    let backup = dir.join("db-snapshot");
    fs::create_dir(&backup).unwrap();
    fs::write(backup.join("metro.sqlite3"), b"backup").unwrap();
    for phase in [JournalPhase::Installing, JournalPhase::Trial] {
        j.phase = phase;
        install::write_journal(&path, &j).unwrap();
        install::mark_trial_ok(&dir, &j.id).unwrap();
        assert!(install::gate_legacy_msi(&dir).is_err());
        assert_eq!(install::read_journal(&path), Some(j.clone()));
        assert_eq!(fs::read(backup.join("metro.sqlite3")).unwrap(), b"backup");
        assert_eq!(db(&i), original);
    }
}

#[test]
fn backup_intent_recovers_on_both_sides_of_publication() {
    for published in [false, true] {
        let i = install();
        let p = paths(&i);
        let before = db(&i);
        let mut j = staged_journal(&i);
        let lock = install::Lock::take(&p.root).unwrap();
        install::begin_backup(&p.root, &mut j, &lock).unwrap();
        let old_orphan = p.update_dir.join("db-snapshot");
        fs::create_dir(&old_orphan).unwrap();
        fs::write(
            old_orphan.join("metro.sqlite3"),
            b"an unowned previous backup",
        )
        .unwrap();
        let pending = p.update_dir.join(format!(".snapshot-{}.tmp", j.id));
        let backup = p.update_dir.join(format!("db-snapshot-{}", j.id));
        if published {
            assert!(
                install::snapshot_db(&p.desktop_root, &p.update_dir, &j)
                    .unwrap()
                    .0
            );
            assert!(backup.join("snapshot.json").is_file());
        } else {
            fs::create_dir(&pending).unwrap();
            fs::write(pending.join("metro.sqlite3"), b"partial copy").unwrap();
        }
        drop(lock);
        let old = Version::parse(OLD).unwrap();
        assert_eq!(
            install::recover(&p.root, &p.desktop_root, &p.update_dir, &old),
            install::Recovery::Nothing
        );
        assert_eq!(db(&i), before);
        assert!(!pending.exists() && !backup.exists());
        assert!(old_orphan.join("metro.sqlite3").is_file());
        let mut j = install::read_journal(&p.root.join(install::JOURNAL)).unwrap();
        assert_eq!(j.phase, JournalPhase::Staged);
        let lock = install::Lock::take(&p.root).unwrap();
        let _instance = begin(&p, &mut j, &lock);
        assert_eq!(j.phase, JournalPhase::Trial);
        assert!(backup.join("snapshot.json").is_file());
    }
}

#[test]
fn terminal_journal_survives_failed_cleanup_and_retries_without_restore() {
    let i = install();
    let p = paths(&i);
    let mut j = staged_journal(&i);
    let lock = install::Lock::take(&p.root).unwrap();
    let instance = begin(&p, &mut j, &lock);
    install::rollback(&p, &mut j, &lock, &instance, "rolled_back", None).unwrap();
    drop(lock);
    let path = p.root.join(install::JOURNAL);
    let backup = p.update_dir.join(format!("db-snapshot-{}", j.id));
    let other = p.update_dir.join("unrelated");
    fs::create_dir(&other).unwrap();
    let partial = p.update_dir.join(format!(".snapshot-{}.tmp", j.id));
    std::os::unix::fs::symlink(&other, &partial).unwrap();
    let old = Version::parse(OLD).unwrap();
    let kind = InstallKind::LinuxInstaller {
        root: p.root.clone(),
    };
    assert!(install::take_finished(&kind, &p.update_dir, &old).is_none());
    assert_eq!(install::read_journal(&path), Some(j.clone()));
    assert!(backup.join("metro.sqlite3").is_file());
    fs::remove_file(partial).unwrap();
    // Simulate a crash partway through removing the backup and a subsequent
    // valid DB write. Terminal cleanup must not attempt another restore.
    fs::remove_file(backup.join("snapshot.json")).unwrap();
    fs::write(
        i.desktop_root.join("local/db/metro.sqlite3"),
        b"after rollback",
    )
    .unwrap();
    assert_eq!(
        install::take_finished(&kind, &p.update_dir, &old)
            .unwrap()
            .outcome,
        "rolled_back"
    );
    assert!(!path.exists() && !backup.exists());
    assert!(other.is_dir());
    assert_eq!(db(&i).0, b"after rollback");
}

#[test]
fn committing_and_recovering_retarget_the_menu_without_running_the_staged_script() {
    const CHILD: &str = "METRO_TEST_MENU_ISOLATED";
    if std::env::var_os(CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "committing_and_recovering_retarget_the_menu_without_running_the_staged_script",
            ])
            .env(CHILD, "1")
            .env("XDG_DATA_HOME", dir.path())
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
        return;
    }
    let apps = PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap()).join("applications");
    fs::create_dir_all(&apps).unwrap();
    let entry = apps.join("dev.metro.desktop.desktop");
    for interrupted in [false, true] {
        let i = install();
        let mut j = staged_journal(&i);
        let hit = i.root.join("script-was-run");
        exec_file(
            &i.root.join("versions").join(NEW).join("install-desktop"),
            format!("#!/bin/sh\n: > '{}'\n", hit.display()).as_bytes(),
        );
        fs::write(
            &entry,
            format!(
                "[Desktop Entry]\nExec=/usr/bin/env \"{}/Metro\"\nIcon=dev.metro.desktop\n",
                i.install_dir.display()
            ),
        )
        .unwrap();
        j.phase = if interrupted {
            JournalPhase::Committing
        } else {
            JournalPhase::Trial
        };
        install::write_journal(&i.root.join(install::JOURNAL), &j).unwrap();
        if interrupted {
            assert_eq!(
                install::recover(
                    &i.root,
                    &i.desktop_root,
                    &paths(&i).update_dir,
                    &Version::parse(NEW).unwrap()
                ),
                install::Recovery::Blocked
            );
        } else {
            let lock = install::Lock::take(&i.root).unwrap();
            install::commit(&i.root, &mut j, &lock).unwrap();
        }
        assert!(!hit.exists());
        assert_eq!(
            fs::read_to_string(&entry)
                .unwrap()
                .contains("/current/Metro\""),
            !interrupted
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unconfirmed_stage_is_preserved_until_a_newer_version_verifies() {
    let i = install();
    let f = serve_version(
        "0.9.2",
        FixtureRelease::new("0.9.2", BTreeMap::new()).files,
        vec![],
    );
    staged_journal(&i); // versions/NEW + journal "staged", but no check said NEW is the latest
    let u = updater(&i, &f, Channel::Release);
    assert_eq!(u.state(Busy::default())["phase"], "idle");
    assert!(i.root.join("versions").join(NEW).exists());
    assert!(u.request_apply(NEW, true, Busy::default()).is_err());
    u.check(true).await.unwrap();
    assert_eq!(u.state(Busy::default())["latest"]["version"], "0.9.2");
    assert!(!i.root.join("versions").join(NEW).exists());
    assert!(!i.root.join(install::JOURNAL).exists());
    assert!(
        i.root.join("versions").join(OLD).join("Metro").is_file(),
        "the running version is kept"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backwards_check_preserves_the_offer_but_manual_upgrade_invalidates_it() {
    let i = install();
    let f = serve(release(desktop_archive(NEW, "desktop")).files, vec![]);
    let u = updater(&i, &f, Channel::Release);
    u.check(true).await.unwrap();
    u.start_download(NEW).unwrap();
    assert_eq!(until_settled(&u).await["phase"], "ready");
    *f.files.lock().unwrap() = FixtureRelease::new(OLD, BTreeMap::new()).files;
    u.check(true).await.unwrap();
    let state = u.state(Busy::default());
    assert_eq!(state["latest"]["version"], NEW);
    assert_eq!(state["result"], "available");
    assert_eq!(state["anomaly"], "older_release_served");
    assert_eq!(state["phase"], "ready");
    drop(u);

    // A manual update moved the pointer but left the old cached check and
    // stage intact. Neither can authorize a reinstall of the running build.
    install::set_pointer(&i.root, NEW).unwrap();
    let own = Version::parse(NEW).unwrap();
    let mut newer = i;
    newer.install_dir = newer.root.join("versions").join(NEW);
    let dir = newer.desktop_root.join("update");
    let u = Updater::new(UpdaterConfig {
        dir,
        install_dir: newer.install_dir.clone(),
        own,
        channel: Channel::Release,
        kind: install::detect(&newer.install_dir, &own, Channel::Release, None),
        source: Source::loopback_fixture(&f.base).unwrap(),
        verifier: Arc::new(TestVerifier),
        trial: None,
        ldd: newer.ldd.clone(),
        trust: None,
    });
    assert!(u.state(Busy::default())["latest"].is_null());
    assert_eq!(u.start_download(NEW).unwrap_err().code, "not_latest");
    assert!(u.request_apply(NEW, true, Busy::default()).is_err());
    assert!(newer.install_dir.join("Metro").is_file());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_apply_requires_signed_evidence_and_every_staged_file() {
    let i = install();
    let f = serve(release(desktop_archive(NEW, "desktop")).files, vec![]);
    let u = updater(&i, &f, Channel::Release);
    u.check(true).await.unwrap();
    u.start_download(NEW).unwrap();
    assert_eq!(until_settled(&u).await["phase"], "ready");
    drop(u);

    let u = updater(&i, &f, Channel::Release);
    let stage_revision = u.state(Busy::default())["stage_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let rx = u.apply_requests();
    u.request_apply(NEW, true, Busy::default()).unwrap();
    let req = rx.borrow().clone().unwrap();
    let lock = install::Lock::take(&i.root).unwrap();
    u.verify_apply(&req, &lock).unwrap();
    let journal_path = i.root.join(install::JOURNAL);
    let original_journal = fs::read(&journal_path).unwrap();
    fs::write(&journal_path, b"{").unwrap();
    let ambiguous = u.verify_apply(&req, &lock).err().unwrap();
    assert!(
        ambiguous
            .get_ref()
            .is_some_and(|e| e.is::<install::JournalUncertain>())
    );
    assert_eq!(fs::read(&journal_path).unwrap(), b"{");
    fs::write(&journal_path, original_journal).unwrap();
    let stage = i.root.join("versions").join(NEW);
    let app_file = stage.join("ui/Metro");
    let original = fs::read(&app_file).unwrap();
    fs::write(&app_file, b"changed after staging").unwrap();
    let bad_stage = u.verify_apply(&req, &lock).err().unwrap();
    assert!(
        !bad_stage
            .get_ref()
            .is_some_and(|e| e.is::<install::JournalUncertain>())
    );
    fs::write(&app_file, original).unwrap();
    let assets = i
        .desktop_root
        .join("update")
        .join(format!("stage-{}", req.journal_id));
    let package = assets.join("package.tar.gz");
    let archive = fs::read(&package).unwrap();
    fs::write(&package, b"not the authenticated archive").unwrap();
    assert!(u.verify_apply(&req, &lock).is_err());
    fs::write(&package, archive).unwrap();
    fs::write(assets.join("SHA256SUMS.sigstore.json"), b"changed proof").unwrap();
    assert!(u.verify_apply(&req, &lock).is_err());
    assert_ne!(stage_revision, req.journal_id);
    let journal = fs::read(i.root.join(install::JOURNAL)).unwrap();
    assert!(u.record_install_failure(req.version, InstallFailure::StageVerificationFailed));
    drop(lock);
    drop(u);

    // Real release metadata still verifies, but it says nothing about the
    // staged bytes. Neither relaunch nor a fresh check repairs that stage.
    let u = updater(&i, &f, Channel::Release);
    assert_eq!(u.state(Busy::default())["phase"], "idle");
    u.check(true).await.unwrap();
    let view = u.state(Busy::default());
    assert_eq!(view["phase"], "idle");
    assert_eq!(view["latest"]["version"], NEW);
    assert_eq!(
        view["last_update"]["failure_code"],
        "stage_verification_failed"
    );
    assert!(view.get("stage_revision").is_none());
    assert!(u.request_apply(NEW, true, Busy::default()).is_err());
    assert_eq!(
        u.start_download(NEW).unwrap_err().code,
        "stage_verification_failed"
    );
    assert_eq!(fs::read(i.root.join(install::JOURNAL)).unwrap(), journal);
    assert!(stage.is_dir() && package.is_file());
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_cached_evidence_keeps_the_newer_floor_and_pending_stage() {
    use metro_release::{ReleaseVerifier, Signed, SignedFacts};
    struct Expired(Vec<u8>);
    impl ReleaseVerifier for Expired {
        fn verify(
            &self,
            signed: Signed<'_>,
            bundle: &[u8],
        ) -> Result<SignedFacts, metro_release::Error> {
            if matches!(&signed, Signed::Bytes(bytes) if *bytes == self.0) {
                return Err(metro_release::Error::new(
                    metro_release::ErrorCode::TrustRootStale,
                    "expired fixture lease",
                ));
            }
            TestVerifier.verify(signed, bundle)
        }
    }
    let mut i = install();
    // Use 0.8.0 so the lower signed 0.9.0 is still newer than this build.
    i.install_dir = i.root.join("versions/0.8.0");
    fs::rename(i.root.join("versions").join(OLD), &i.install_dir).unwrap();
    install::set_pointer(&i.root, "0.8.0").unwrap();
    let own = Version::parse("0.8.0").unwrap();
    let make = |fixture: &Fixture, verifier| {
        Updater::new(UpdaterConfig {
            dir: i.desktop_root.join("update"),
            install_dir: i.install_dir.clone(),
            own,
            channel: Channel::Release,
            kind: install::detect(&i.install_dir, &own, Channel::Release, None),
            source: Source::loopback_fixture(&fixture.base).unwrap(),
            verifier,
            trial: None,
            ldd: i.ldd.clone(),
            trust: None,
        })
    };
    let newer = release(desktop_archive(NEW, "desktop"));
    let expired = newer.files["SHA256SUMS"].clone();
    let newer = serve(newer.files, vec![]);
    let lower = serve_version(OLD, FixtureRelease::new(OLD, BTreeMap::new()).files, vec![]);
    let u = make(&newer, Arc::new(TestVerifier));
    u.check(true).await.unwrap();
    u.start_download(NEW).unwrap();
    assert_eq!(until_settled(&u).await["phase"], "ready");
    let dir = i.desktop_root.join("update");
    let before = install::read_journal(&i.root.join(install::JOURNAL)).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("release-floor.json")).unwrap(),
        format!("\"{NEW}\"")
    );
    drop(u);

    for legacy in [false, true] {
        if legacy {
            // Older builds stored the observed version only in status.json.
            fs::remove_file(dir.join("release-floor.json")).unwrap();
        }
        let u = make(&lower, Arc::new(Expired(expired.clone())));
        assert!(u.state(Busy::default())["latest"].is_null());
        assert!(u.request_apply(NEW, true, Busy::default()).is_err());
        u.check(true).await.unwrap();
        assert_eq!(u.state(Busy::default())["anomaly"], "older_release_served");
        assert!(u.state(Busy::default())["latest"].is_null());
        assert!(u.start_download(OLD).is_err());
        assert!(u.start_download(NEW).is_err());
        assert_eq!(
            install::read_journal(&i.root.join(install::JOURNAL)),
            Some(before.clone())
        );
        let status: metro_connector::desktop::update::Status =
            serde_json::from_slice(&fs::read(dir.join("status.json")).unwrap()).unwrap();
        assert_eq!(status.evidence.unwrap().manifest, expired);
        assert_eq!(
            fs::read_to_string(dir.join("release-floor.json")).unwrap(),
            format!("\"{NEW}\"")
        );
    }
    let status = dir.join("status.json");
    let moved = dir.join("saved-status.json");
    fs::rename(&status, &moved).unwrap();
    let u = make(&lower, Arc::new(TestVerifier));
    u.check(true).await.unwrap();
    assert_eq!(u.state(Busy::default())["anomaly"], "older_release_served");
    assert!(u.state(Busy::default())["latest"].is_null());
    drop(u);
    fs::rename(moved, status).unwrap();
    let u = make(&newer, Arc::new(TestVerifier));
    assert_eq!(u.state(Busy::default())["phase"], "ready");
    u.request_apply(NEW, true, Busy::default()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupted_settings_stay_off_and_failed_writes_are_reported() {
    let i = install();
    let f = serve(BTreeMap::new(), vec![]);
    let dir = i.desktop_root.join("update");
    fs::create_dir_all(&dir).unwrap();
    let settings = dir.join("settings.json");
    fs::write(&settings, b"{no").unwrap();
    let u = updater(&i, &f, Channel::Release);
    let state = u.state(Busy::default());
    assert_eq!(state["auto_check"], false);
    assert_eq!(state["persistence_error"], "settings_io");
    assert_eq!(fs::read(&settings).unwrap(), b"{no");
    fs::remove_file(&settings).unwrap();
    fs::create_dir(&settings).unwrap();
    assert_eq!(u.set_auto_check(true).unwrap_err().status, 500);
    assert_eq!(u.state(Busy::default())["auto_check"], false);
    assert!(settings.is_dir());
    fs::remove_dir(&settings).unwrap();
    u.set_auto_check(true).unwrap();
    assert_eq!(u.state(Busy::default())["auto_check"], true);
    assert!(u.state(Busy::default())["persistence_error"].is_null());
}

#[test]
fn failed_restore_keeps_the_snapshot_and_can_retry_under_the_same_watchdog() {
    let i = install();
    let before = db(&i);
    let mut j = staged_journal(&i);
    let p = paths(&i);
    fs::create_dir_all(&p.update_dir).unwrap();
    let lock = install::Lock::take(&p.root).unwrap();
    let instance = begin(&p, &mut j, &lock);
    let snap = p
        .update_dir
        .join(format!("db-snapshot-{}/metro.sqlite3-wal", j.id));
    let saved = fs::read(&snap).unwrap();
    fs::write(&snap, b"corrupt WAL with matching name").unwrap();
    fs::write(
        i.desktop_root.join("local/db/metro.sqlite3"),
        b"trial output",
    )
    .unwrap();
    let mut rollback = || install::rollback(&p, &mut j, &lock, &instance, "rolled_back", None);
    assert!(rollback().is_err());
    assert_eq!(
        install::read_journal(&p.root.join(install::JOURNAL))
            .unwrap()
            .phase,
        JournalPhase::RollingBack
    );
    assert_eq!(db(&i).0, b"trial output", "no partial restoration");
    assert!(snap.parent().unwrap().join("metro.sqlite3").is_file());
    assert!(p.root.join("versions").join(NEW).is_dir());
    fs::write(&snap, saved).unwrap();
    rollback().unwrap();
    assert_eq!(db(&i), before);
}

#[test]
fn unknown_or_foreign_journal_is_never_consumed() {
    let i = install();
    let mut j = staged_journal(&i);
    let p = paths(&i);
    let path = p.root.join(install::JOURNAL);
    let old = Version::parse(OLD).unwrap();
    let snap = p.update_dir.join(format!("db-snapshot-{}", j.id));
    fs::create_dir_all(&snap).unwrap();
    let backup = snap.join("metro.sqlite3");
    fs::write(&backup, b"preserve old database").unwrap();
    for raw in [
        b"{".as_slice(),
        br#"{"version":99,"id":"01","phase":"trial"}"#.as_slice(),
        br#"{"version":1,"id":"../../outside","from":"0.9.0","to":"0.9.1","phase":"trial","started_at":0}"#,
        br#"{"version":1,"id":"0123456789abcdef","from":"0.9.1","to":"0.9.0","phase":"trial","started_at":0}"#,
    ] {
        fs::write(&path, raw).unwrap();
        assert_eq!(
            install::recover(&p.root, &p.desktop_root, &p.update_dir, &old),
            install::Recovery::JournalUncertain
        );
        assert_eq!(fs::read(&path).unwrap(), raw);
        assert_eq!(install::read_pointer(&p.root).as_deref(), Some(OLD));
        assert_eq!(fs::read(&backup).unwrap(), b"preserve old database");
    }
    j.phase = JournalPhase::Trial;
    install::write_journal(&path, &j).unwrap();
    install::set_pointer(&p.root, "0.9.2").unwrap();
    assert_eq!(
        install::recover(&p.root, &p.desktop_root, &p.update_dir, &old),
        install::Recovery::JournalUncertain
    );
    assert_eq!(install::read_journal(&path), Some(j));
    assert_eq!(install::read_pointer(&p.root).as_deref(), Some("0.9.2"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_journal_that_becomes_unreadable_during_use_blocks_future_staging() {
    let i = install();
    let f = serve(release(desktop_archive(NEW, "desktop")).files, vec![]);
    let u = updater(&i, &f, Channel::Release);
    u.check(true).await.unwrap();
    let journal = i.root.join(install::JOURNAL);
    fs::write(&journal, b"{").unwrap();
    u.start_download(NEW).unwrap();
    let state = until_settled(&u).await;
    assert_eq!(state["phase"], "failed");
    assert_eq!(state["failure"]["code"], install::JOURNAL_UNCERTAIN);
    u.check(true).await.unwrap();
    assert_eq!(
        u.start_download(NEW).unwrap_err().code,
        install::JOURNAL_UNCERTAIN
    );
    assert_eq!(fs::read(&journal).unwrap(), b"{");
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
}

#[test]
fn a_trial_with_a_live_watchdog_is_left_alone() {
    let i = install();
    let mut j = staged_journal(&i);
    let p = paths(&i);
    fs::create_dir_all(&p.update_dir).unwrap();
    let lock = install::Lock::take(&p.root).unwrap();
    let _instance = begin(&p, &mut j, &lock);
    assert_eq!(j.watchdog_pid, Some(std::process::id()));
    assert!(install::trial_in_progress(&i.root));
    // A second start of the old version while the trial runs: no rollback.
    let old = Version::parse(OLD).unwrap();
    assert_eq!(
        install::recover(&i.root, &i.desktop_root, &p.update_dir, &old),
        install::Recovery::Blocked
    );
    assert!(i.root.join("versions").join(NEW).join("Metro").is_file());
    drop(lock);
    // Once the watchdog is gone, the same state is an interrupted update.
    j.watchdog_pid = Some(i32::MAX as u32 - 1);
    install::write_journal(&i.root.join(install::JOURNAL), &j).unwrap();
    assert!(!install::trial_in_progress(&i.root));
    assert_eq!(
        install::recover(&i.root, &i.desktop_root, &p.update_dir, &old),
        install::Recovery::Blocked
    );
}

/// Interruption matrix: a crash after each journal phase ends consistent.
#[test]
fn recovery_after_interruptions() {
    let old = Version::parse(OLD).unwrap();
    let new = Version::parse(NEW).unwrap();
    use JournalPhase::*;
    for (phase, pointer, running, expect_pointer, expect) in [
        (Staged, OLD, &old, OLD, install::Recovery::Nothing),
        (Trial, OLD, &old, OLD, install::Recovery::Blocked),
        (RollingBack, OLD, &old, OLD, install::Recovery::Blocked),
        (Committing, OLD, &old, OLD, install::Recovery::Blocked),
        (Committing, NEW, &old, NEW, install::Recovery::Blocked),
        (Committing, OLD, &new, OLD, install::Recovery::Blocked),
        (Committing, NEW, &new, NEW, install::Recovery::Blocked),
    ] {
        let i = install();
        let before = db(&i);
        let mut j = staged_journal(&i);
        let p = paths(&i);
        fs::create_dir_all(&p.update_dir).unwrap();
        if phase != "staged" {
            let lock = install::Lock::take(&p.root).unwrap();
            let _instance = begin(&p, &mut j, &lock);
            fs::write(i.desktop_root.join("local/db/metro.sqlite3"), b"migrated").unwrap();
        }
        j.phase = phase;
        // The process that was watching has crashed.
        j.watchdog_pid = j.watchdog_pid.map(|_| i32::MAX as u32 - 1);
        install::write_journal(&i.root.join(install::JOURNAL), &j).unwrap();
        install::set_pointer(&i.root, pointer).unwrap();
        let r = install::recover(&i.root, &i.desktop_root, &p.update_dir, running);
        assert_eq!(r, expect, "{phase:?} {pointer} {running}");
        assert_eq!(
            install::read_pointer(&i.root).as_deref(),
            Some(expect_pointer),
            "{phase:?} {pointer} {running}"
        );
        let target = i.root.join("versions").join(expect_pointer);
        assert!(
            target.join("Metro").is_file(),
            "pointer names a missing version: {phase:?}"
        );
        if expect == install::Recovery::Blocked {
            assert_eq!(db(&i).0, b"migrated", "{phase:?}");
            assert!(p.update_dir.join(format!("db-snapshot-{}", j.id)).is_dir());
        } else {
            assert_eq!(db(&i), before);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_update_endpoints() {
    let i = install();
    let f = serve(release(desktop_archive(NEW, "desktop")).files, vec![]);
    let u = updater(&i, &f, Channel::Release);
    let mut cfg = DesktopConfig::new(i.desktop_root.clone(), true, None);
    cfg.updater = Some(u.clone());
    let desktop = Desktop::new(cfg);
    let b = bridge::start(desktop).await.unwrap();
    let http = reqwest::Client::new();
    let call = |method: &str, path: &str, body: Option<Value>| {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut rb = http
            .request(m, format!("{}{path}", b.url))
            .bearer_auth(b.token.expose());
        if let Some(v) = body {
            rb = rb
                .header("content-type", "application/json")
                .body(v.to_string());
        }
        async move {
            let r = rb.send().await.unwrap();
            {
                let s = r.status().as_u16();
                (
                    s,
                    serde_json::from_str::<Value>(&r.text().await.unwrap_or_default())
                        .unwrap_or(Value::Null),
                )
            }
        }
    };
    let (s, v) = call("GET", "/v1/update", None).await;
    assert_eq!(s, 200);
    assert_eq!(v["current"]["version"], OLD);
    // Without the bearer: 401.
    let r = http
        .get(format!("{}/v1/update", b.url))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    let (s, v) = call(
        "PUT",
        "/v1/update/settings",
        Some(json!({"auto_check": false})),
    )
    .await;
    assert_eq!((s, v["auto_check"].as_bool()), (200, Some(false)));
    assert_eq!(
        call(
            "PUT",
            "/v1/update/settings",
            Some(json!({"auto_check": "no"}))
        )
        .await
        .0,
        400
    );
    assert_eq!(
        call("POST", "/v1/update/download", Some(json!({"version": NEW})))
            .await
            .0,
        409,
        "before a check"
    );
    let (s, v) = call("POST", "/v1/update/check", Some(json!({}))).await;
    assert_eq!((s, v["result"].as_str()), (200, Some("available")));
    let (s, v) = call("POST", "/v1/update/apply", Some(json!({"version": NEW}))).await;
    assert_eq!((s, v["error"]["code"].as_str()), (409, Some("not_ready")));
    assert_eq!(
        call(
            "POST",
            "/v1/update/download",
            Some(json!({"version": NEW, "url": "x"}))
        )
        .await
        .0,
        400
    );
    let (s, v) = call(
        "POST",
        "/v1/update/download",
        Some(json!({"version": "0.9.2"})),
    )
    .await;
    assert_eq!((s, v["error"]["code"].as_str()), (409, Some("not_latest")));
    assert_eq!(
        call("POST", "/v1/update/download", Some(json!({"version": NEW})))
            .await
            .0,
        200
    );
    until_settled(&u).await;
    let (s, v) = call("POST", "/v1/update/cancel", Some(json!({}))).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (409, Some("nothing_to_cancel"))
    );
    // `started` outside a trial is a harmless no-op, and idempotent.
    for _ in 0..2 {
        let (s, v) = call("POST", "/v1/update/started", Some(json!({}))).await;
        assert_eq!((s, v["trial"].as_bool()), (200, Some(false)));
    }
    let (s, v) = call(
        "POST",
        "/v1/update/apply",
        Some(json!({"version": NEW, "confirm_interrupt": true})),
    )
    .await;
    assert_eq!((s, v["phase"].as_str()), (200, Some("applying")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trial_started_writes_the_ok_file() {
    let i = install();
    let f = serve(BTreeMap::new(), vec![]);
    let own = Version::parse(NEW).unwrap();
    let mut j = staged_journal(&i);
    j.id = "00112233aabbccdd".into();
    j.phase = JournalPhase::Trial;
    j.watchdog_pid = Some(std::process::id());
    install::write_journal(&i.root.join(install::JOURNAL), &j).unwrap();
    let u = Updater::new(UpdaterConfig {
        dir: i.desktop_root.join("update"),
        install_dir: i.root.join("versions").join(NEW),
        own,
        channel: Channel::Release,
        kind: InstallKind::LinuxInstaller {
            root: i.root.clone(),
        },
        source: Source::loopback_fixture(&f.base).unwrap(),
        verifier: Arc::new(TestVerifier),
        trial: Some("00112233aabbccdd".into()),
        ldd: i.ldd.clone(),
        trust: None,
    });
    assert!(u.started());
    assert!(u.started());
    assert!(install::trial_ok(
        &i.desktop_root.join("update"),
        "00112233aabbccdd"
    ));
    assert_eq!(f.hits.load(Ordering::SeqCst), 0);
}
