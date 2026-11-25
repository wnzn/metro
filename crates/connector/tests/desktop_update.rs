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
use metro_connector::desktop::update::install::{self, InstallKind, Journal};
use metro_connector::desktop::update::{Busy, Updater, UpdaterConfig};
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
            } else if path == format!("/api/repos/wnzn/metro/releases/tags/v{NEW}") {
                let notes = json!({"body": "Fixes and **notes**", "html_url": format!("https://github.com/wnzn/metro/releases/tag/v{NEW}")});
                response = (200, vec![], notes.to_string().into_bytes());
            } else {
                let name = path
                    .strip_prefix("/wnzn/metro/releases/latest/download/")
                    .or_else(|| {
                        path.strip_prefix(&format!("/wnzn/metro/releases/download/v{NEW}/"))
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
    fs::write(install_dir.join(install::LOCAL_RUNNER_MARKER), b"").unwrap();
    fs::write(root.join(install::MARKER), b"managed").unwrap();
    install::set_pointer(&root, OLD).unwrap();
    let desktop_root = dir.path().join("desktop");
    fs::create_dir_all(desktop_root.join("local/db")).unwrap();
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

fn updater(i: &Install, f: &Fixture, channel: Channel) -> Arc<Updater> {
    let own = Version::parse(OLD).unwrap();
    Updater::new(UpdaterConfig {
        dir: i.desktop_root.join("update"),
        install_dir: i.install_dir.clone(),
        own,
        channel,
        kind: install::detect(&i.install_dir, &own, channel, None),
        source: Source::loopback_fixture(&f.base).unwrap(),
        verifier: Arc::new(TestVerifier),
        trial: None,
        ldd: i.ldd.clone(),
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
        phase: "trial".into(),
        outcome: None,
        detail: None,
        local_db_snapshot: false,
        msi: None,
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
    u.set_auto_check(false);
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
        phase: "staged".into(),
        outcome: None,
        detail: None,
        local_db_snapshot: false,
        msi: None,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trial_confirmed_commits() {
    let i = install();
    let mut j = staged_journal(&i);
    let p = paths(&i);
    fs::create_dir_all(&p.update_dir).unwrap();
    apply::begin_trial(&p, &mut j).unwrap();
    assert!(j.local_db_snapshot);
    let exe = trial_stub(&i, &p.update_dir, &j.id, "printf ok > '$OK'\nsleep 5");
    let mut child = apply::spawn_trial(&exe, &j.id).unwrap();
    let out = apply::watch_trial(&p, &mut j, &mut child, Duration::from_secs(10)).await;
    assert_eq!(out, TrialOutcome::Committed);
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(NEW));
    let jj = install::read_journal(&i.root.join(install::JOURNAL)).unwrap();
    assert_eq!(
        (jj.phase.as_str(), jj.outcome.as_deref()),
        ("committed", Some("updated"))
    );
    let _ = child.kill().await;
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
    assert!(!i.root.join("versions").join(OLD).exists());
    assert!(!p.update_dir.join("db-snapshot").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trial_crash_rolls_back_and_restores_the_database() {
    let i = install();
    let before = db(&i);
    let mut j = staged_journal(&i);
    let p = paths(&i);
    fs::create_dir_all(&p.update_dir).unwrap();
    apply::begin_trial(&p, &mut j).unwrap();
    let exe = trial_stub(&i, &p.update_dir, &j.id, "exit 3");
    let mut child = apply::spawn_trial(&exe, &j.id).unwrap();
    let out = apply::watch_trial(&p, &mut j, &mut child, Duration::from_secs(10)).await;
    assert_eq!(out, TrialOutcome::RolledBack("exited with 3".into()));
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
    assert_eq!(db(&i), before, "database restored byte for byte");
    assert!(!i.root.join("versions").join(NEW).exists());
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trial_timeout_kills_and_rolls_back() {
    let i = install();
    let before = db(&i);
    let mut j = staged_journal(&i);
    let p = paths(&i);
    fs::create_dir_all(&p.update_dir).unwrap();
    apply::begin_trial(&p, &mut j).unwrap();
    let exe = trial_stub(&i, &p.update_dir, &j.id, "exec sleep 60");
    let mut child = apply::spawn_trial(&exe, &j.id).unwrap();
    let out = apply::watch_trial(&p, &mut j, &mut child, Duration::from_millis(800)).await;
    assert!(matches!(out, TrialOutcome::RolledBack(_)));
    assert!(child.try_wait().unwrap().is_some(), "the trial was killed");
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
    assert_eq!(db(&i), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trial_closed_at_once_is_not_committed() {
    let i = install();
    let mut j = staged_journal(&i);
    let p = paths(&i);
    fs::create_dir_all(&p.update_dir).unwrap();
    apply::begin_trial(&p, &mut j).unwrap();
    let exe = trial_stub(&i, &p.update_dir, &j.id, "exit 0");
    let mut child = apply::spawn_trial(&exe, &j.id).unwrap();
    assert_eq!(
        apply::watch_trial(&p, &mut j, &mut child, Duration::from_secs(10)).await,
        TrialOutcome::NotConfirmed
    );
    assert_eq!(install::read_pointer(&i.root).as_deref(), Some(OLD));
}

/// Interruption matrix: a crash after each journal phase ends consistent.
#[test]
fn recovery_after_interruptions() {
    let old = Version::parse(OLD).unwrap();
    let new = Version::parse(NEW).unwrap();
    for (phase, pointer, running, expect_pointer, expect) in [
        ("staged", OLD, &old, OLD, install::Recovery::Nothing),
        ("trial", OLD, &old, OLD, install::Recovery::RolledBack),
        ("committing", OLD, &old, OLD, install::Recovery::RolledBack),
        ("committing", NEW, &old, OLD, install::Recovery::RolledBack),
        ("committing", OLD, &new, NEW, install::Recovery::Committed),
        ("committing", NEW, &new, NEW, install::Recovery::Committed),
    ] {
        let i = install();
        let before = db(&i);
        let mut j = staged_journal(&i);
        let p = paths(&i);
        fs::create_dir_all(&p.update_dir).unwrap();
        if phase != "staged" {
            apply::begin_trial(&p, &mut j).unwrap();
            fs::write(i.desktop_root.join("local/db/metro.sqlite3"), b"migrated").unwrap();
        }
        j.phase = phase.into();
        install::write_journal(&i.root.join(install::JOURNAL), &j).unwrap();
        install::set_pointer(&i.root, pointer).unwrap();
        let r = install::recover(&i.root, &i.desktop_root, &p.update_dir, running);
        assert_eq!(r, expect, "{phase} {pointer} {running}");
        assert_eq!(
            install::read_pointer(&i.root).as_deref(),
            Some(expect_pointer),
            "{phase} {pointer} {running}"
        );
        let target = i.root.join("versions").join(expect_pointer);
        assert!(
            target.join("Metro").is_file(),
            "pointer names a missing version: {phase}"
        );
        if expect == install::Recovery::RolledBack {
            assert_eq!(db(&i), before, "{phase}");
            assert!(!i.root.join("versions").join(NEW).exists());
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
    let own = Version::parse(OLD).unwrap();
    let u = Updater::new(UpdaterConfig {
        dir: i.desktop_root.join("update"),
        install_dir: i.install_dir.clone(),
        own,
        channel: Channel::Release,
        kind: InstallKind::LinuxInstaller {
            root: i.root.clone(),
        },
        source: Source::loopback_fixture(&f.base).unwrap(),
        verifier: Arc::new(TestVerifier),
        trial: Some("00112233aabbccdd".into()),
        ldd: i.ldd.clone(),
    });
    assert!(u.started());
    assert!(u.started());
    assert!(install::trial_ok(
        &i.desktop_root.join("update"),
        "00112233aabbccdd"
    ));
    assert_eq!(f.hits.load(Ordering::SeqCst), 0);
}
