//! Runner self-update against a loopback fixture GitHub (test verifier), in a
//! temporary managed install root. Stub `metro-runner` binaries are shell
//! scripts; the switch-and-exec step runs in a forked child.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use metro_release::fetch::{BlockingFetcher, Source};
use metro_release::testing::{FixtureRelease, TestVerifier, release_json, tar_gz};
use metro_release::{Channel, Version};
use metro_runner::update::{self, InstallKind, PendingFile, SelfUpdate, Switch, Updater};

/// Every test here takes the install lock, a `flock` that belongs to its
/// open file description, and most start processes (start checks, `fork`).
/// A child forked by one test thread holds a copy of every descriptor until
/// its `exec`, so another test's dropped lock can stay held in that window
/// and its next take sees `installer_busy`. Run the tests one at a time.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

const OLD: &str = "0.9.0";
const NEW: &str = "0.9.1";

fn stub(check_exit: u8) -> Vec<u8> {
    format!(
        "#!/bin/sh\nif [ \"$1\" = --check-start ]; then exit {check_exit}; fi\n\
         printf '%s|%s|%s\\n' \"$0\" \"$*\" \"$STUB_MARKER_VALUE\" > \"$STUB_MARKER\"\nexit 0\n"
    )
    .into_bytes()
}

fn runner_archive(version: &str, kind: &str, binary: &[u8]) -> Vec<u8> {
    let run = b"#!/bin/sh\nexec \"$(dirname \"$0\")/metro-runner\"\n".to_vec();
    let payload: Vec<(&str, &[u8], u32)> =
        vec![("metro-runner", binary, 0o755), ("run-runner", &run, 0o755)];
    let rj = release_json(kind, "linux-x64", version, &payload);
    let mut all = vec![("RELEASE.json", rj.as_slice(), 0o644)];
    all.extend(payload);
    tar_gz(&format!("Metro-runner-{version}-linux-x64"), &all)
}

const SCRIPT: &[u8] = b"#!/bin/sh\n# new installer script\n";

fn release(archive: Vec<u8>) -> FixtureRelease {
    let mut overrides = BTreeMap::new();
    overrides.insert(format!("Metro-runner-{NEW}-linux-x64.tar.gz"), archive);
    overrides.insert("install-metro-runner-linux.sh".to_string(), SCRIPT.to_vec());
    FixtureRelease::new(NEW, overrides)
}

struct Fixture {
    base: String,
    hits: Arc<AtomicUsize>,
}

/// Serves `files` under `/wnzn/metro/releases/download/v<served_as>/`.
fn serve(files: BTreeMap<String, Vec<u8>>, served_as: &str) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let prefix = format!("/wnzn/metro/releases/download/v{served_as}/");
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
            let body = path
                .strip_prefix(&prefix)
                .and_then(|n| files.get(n))
                .cloned();
            let (status, body) = body.map_or((404, b"no".to_vec()), |b| (200, b));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    Fixture { base, hits }
}

struct Root {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

/// A managed install of OLD.
fn managed_root() -> Root {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metro-runner");
    fs::create_dir_all(path.join("versions").join(OLD)).unwrap();
    for d in [
        path.clone(),
        path.join("versions"),
        path.join("versions").join(OLD),
    ] {
        fs::set_permissions(d, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        path.join("versions").join(OLD).join("metro-runner"),
        stub(0),
    )
    .unwrap();
    set_exec(&path.join("versions").join(OLD).join("metro-runner"));
    fs::write(
        path.join(update::MARKER),
        metro_release::installer_lock::MIGRATION,
    )
    .unwrap();
    fs::write(
        path.join(update::SCRIPT),
        b"#!/bin/sh\n# old installer script\n",
    )
    .unwrap();
    update::point_current(&path, Path::new("versions/0.9.0")).unwrap();
    Root { _dir: dir, path }
}

fn set_exec(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn runner_updater_and_published_installer_exclude_each_other() {
    let _serial = serial();
    use std::process::{Command, Stdio};
    let r = managed_root();
    let temp = tempfile::tempdir().unwrap();
    let archive = temp
        .path()
        .join(format!("Metro-runner-{NEW}-linux-x64.tar.gz"));
    let bytes = runner_archive(NEW, "runner", &stub(0));
    let sum = metro_release::hex(&metro_release::sha256(&bytes));
    fs::write(&archive, bytes).unwrap();
    for (name, script) in [
        ("getconf", "#!/bin/sh\nprintf 'glibc 2.39\\n'\n"),
        (
            "uname",
            "#!/bin/sh\ncase \"$1\" in -s) echo Linux;; -m) echo x86_64;; -r) echo 6.6.0;; esac\n",
        ),
        (
            "tar",
            "#!/bin/sh\nprintf 'locked\\n' >&2\nread -r done\nexit 1\n",
        ),
    ] {
        let path = temp.path().join(name);
        fs::write(&path, script).unwrap();
        set_exec(&path);
    }
    let published = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/test-fixtures/install-metro-runner-linux-0.4.0.sh");
    let command = || {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg(&published)
            .args(["update", "--archive"])
            .arg(&archive)
            .args(["--sha256", &sum])
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", temp.path().display()))
            .env("HOME", temp.path())
            .env("METRO_RUNNER_INSTALL_DIR", &r.path);
        cmd
    };
    let lock = update::Lock::take(&r.path).unwrap();
    let refused = command().output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("another install is running"));
    drop(lock);

    let mut holder = command()
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut marker = String::new();
    let mut errors = BufReader::new(holder.stderr.take().unwrap());
    errors.read_line(&mut marker).unwrap();
    let refused = update::Lock::take(&r.path);
    drop(holder.stdin.take());
    assert!(!holder.wait().unwrap().success()); // the tar fixture stops there
    assert_eq!(marker, "locked\n");
    assert_eq!(refused.err().unwrap().code, "installer_busy");
    assert_eq!(current(&r.path), Path::new("versions").join(OLD));
    assert!(!r.path.join(update::LOCK).exists());
}

fn updater(root: &Path, f: &Fixture) -> Updater {
    Updater {
        root: root.to_path_buf(),
        own: Version::parse(OLD).unwrap(),
        fetcher: BlockingFetcher::new(
            Source::loopback_fixture(&f.base).unwrap(),
            Duration::from_secs(10),
        ),
        verifier: Box::new(TestVerifier),
    }
}

fn current(root: &Path) -> PathBuf {
    fs::read_link(root.join("current")).unwrap()
}

fn leftovers(root: &Path) -> Vec<String> {
    fs::read_dir(root)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            n.starts_with(".stage.")
                || n.starts_with(".old.")
                || n == ".lock"
                || n.starts_with(".update-dl")
        })
        .collect()
}

#[test]
fn happy_path_stages_verifies_and_execs_the_new_version() {
    let _serial = serial();
    let root = managed_root();
    let rel = release(runner_archive(NEW, "runner", &stub(0)));
    let f = serve(rel.files.clone(), NEW);
    let switch = updater(&root.path, &f)
        .prepare(NEW)
        .expect("update prepared");
    // Staged, start-checked, script replaced; `current` not yet moved.
    let new_bin = root.path.join("versions").join(NEW).join("metro-runner");
    assert_eq!(fs::read(&new_bin).unwrap(), stub(0));
    assert_eq!(fs::read(root.path.join(update::SCRIPT)).unwrap(), SCRIPT);
    assert_eq!(current(&root.path), Path::new("versions/0.9.0"));
    // The prepared switch still holds the installers' lock...
    assert_eq!(
        update::Lock::take(&root.path).err().unwrap().code,
        "installer_busy"
    );
    // ...and a dropped (abandoned) switch lets go of it.
    drop(switch);
    assert!(update::Lock::take(&root.path).is_ok());
    assert!(
        leftovers(&root.path).is_empty(),
        "{:?}",
        leftovers(&root.path)
    );

    // The switch itself, in a forked child: it must point `current` at the new
    // version, write the pending file, and execve the new binary with the
    // given argv and environment.
    let marker = root.path.join("marker");
    let switch = Switch::new(
        &root.path,
        NEW,
        OLD,
        Some(&PendingFile {
            version: 1,
            from: OLD.into(),
            to: NEW.into(),
            attempts: 0,
            rolling_back: false,
        }),
        (
            vec![OsString::from("metro-runner"), OsString::from("arg1")],
            vec![
                (
                    OsString::from("STUB_MARKER"),
                    marker.clone().into_os_string(),
                ),
                (
                    OsString::from("STUB_MARKER_VALUE"),
                    OsString::from("kept-env"),
                ),
            ],
        ),
        update::Lock::take(&root.path).unwrap(),
    )
    .unwrap();
    // SAFETY: the child only runs Switch::exec (libc calls on prepared
    // buffers) and _exit.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0);
    if pid == 0 {
        let _ = switch.exec();
        unsafe { libc::_exit(99) };
    }
    // Drop the parent's reference; only the exec'ing child holds the lock.
    switch.handoff_to_fork();
    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "child status {status}"
    );
    let line = fs::read_to_string(&marker).unwrap();
    assert!(line.ends_with("|arg1|kept-env\n"), "{line}");
    assert!(
        update::Lock::take(&root.path).is_ok(),
        "the lock is released at the exec"
    );
    assert_eq!(current(&root.path), Path::new("versions/0.9.1"));
    let pending: PendingFile =
        serde_json::from_slice(&fs::read(root.path.join(update::PENDING)).unwrap()).unwrap();
    assert_eq!(
        pending,
        PendingFile {
            version: 1,
            from: OLD.into(),
            to: NEW.into(),
            attempts: 0,
            rolling_back: false,
        }
    );
    // The new version's first start counts, then a good claim confirms.
    let new = Version::parse(NEW).unwrap();
    assert!(matches!(
        update::on_start(&root.path, &new).unwrap(),
        update::StartAction::Continue
    ));
    update::confirm_started(&root.path, &new);
    assert!(!root.path.join(update::PENDING).exists());
    assert!(
        root.path.join("versions").join(OLD).exists(),
        "the previous version is kept"
    );
}

#[test]
fn a_failed_exec_points_current_back() {
    let _serial = serial();
    let root = managed_root();
    let dest = root.path.join("versions").join(NEW);
    fs::create_dir_all(&dest).unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o700)).unwrap();
    let exe = dest.join(update::BINARY);
    fs::write(&exe, b"#!/no-such-interpreter\n").unwrap();
    set_exec(&exe);
    let switch = Switch::new(
        &root.path,
        NEW,
        OLD,
        Some(&PendingFile {
            version: 1,
            from: OLD.into(),
            to: NEW.into(),
            attempts: 0,
            rolling_back: false,
        }),
        (vec![OsString::from("x")], vec![]),
        update::Lock::take(&root.path).unwrap(),
    )
    .unwrap();
    let e = switch.exec();
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
    assert!(
        update::Lock::take(&root.path).is_ok(),
        "a failed exec releases the lock"
    );
    assert_eq!(current(&root.path), Path::new("versions/0.9.0"));
    assert!(!root.path.join(update::PENDING).exists());
}

#[test]
fn changing_a_non_executable_after_prepare_refuses_the_switch() {
    let _serial = serial();
    let root = managed_root();
    let rel = release(runner_archive(NEW, "runner", &stub(0)));
    let f = serve(rel.files, NEW);
    let switch = updater(&root.path, &f).prepare(NEW).unwrap();
    fs::write(
        root.path.join("versions").join(NEW).join("run-runner"),
        b"changed",
    )
    .unwrap();
    assert_eq!(switch.exec().kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(current(&root.path), Path::new("versions/0.9.0"));
    assert!(!root.path.join(update::PENDING).exists());
    assert!(update::Lock::take(&root.path).is_ok());
}

#[test]
fn failed_rollback_exec_keeps_retry_intent_and_the_previous_copy() {
    let _serial = serial();
    let root = managed_root();
    let old = root.path.join("versions").join(OLD).join(update::BINARY);
    fs::write(&old, b"#!/no-such-interpreter\n").unwrap();
    let new = root.path.join("versions").join(NEW);
    fs::create_dir_all(&new).unwrap();
    fs::set_permissions(&new, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(new.join(update::BINARY), stub(0)).unwrap();
    set_exec(&new.join(update::BINARY));
    update::point_current(&root.path, &Path::new("versions").join(NEW)).unwrap();
    let pending = PendingFile {
        version: 1,
        from: OLD.into(),
        to: NEW.into(),
        attempts: update::MAX_STARTS,
        rolling_back: false,
    };
    let journal = root.path.join(update::PENDING);
    fs::write(&journal, serde_json::to_vec(&pending).unwrap()).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
    let version = Version::parse(NEW).unwrap();
    let update::StartAction::RollBack(switch) = update::on_start(&root.path, &version).unwrap()
    else {
        panic!("expected rollback");
    };
    assert_eq!(switch.exec().kind(), std::io::ErrorKind::NotFound);
    assert_eq!(current(&root.path), Path::new("versions/0.9.1"));
    assert!(old.is_file());
    let after: PendingFile = serde_json::from_slice(&fs::read(journal).unwrap()).unwrap();
    assert!(after.rolling_back);
    assert!(matches!(
        update::on_start(&root.path, &version).unwrap(),
        update::StartAction::RollBack(_)
    ));
}

#[test]
fn rollback_preparation_failure_does_not_allow_the_trial_to_confirm() {
    let _serial = serial();
    let root = managed_root();
    let old = root.path.join("versions").join(OLD).join(update::BINARY);
    let keep = old.with_extension("temporarily-unavailable");
    fs::rename(&old, &keep).unwrap();
    let new = root.path.join("versions").join(NEW);
    fs::create_dir(&new).unwrap();
    fs::set_permissions(&new, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(new.join(update::BINARY), stub(0)).unwrap();
    set_exec(&new.join(update::BINARY));
    update::point_current(&root.path, &Path::new("versions").join(NEW)).unwrap();
    let journal = root.path.join(update::PENDING);
    let pending = PendingFile {
        version: 1,
        from: OLD.into(),
        to: NEW.into(),
        attempts: update::MAX_STARTS,
        rolling_back: false,
    };
    fs::write(&journal, serde_json::to_vec(&pending).unwrap()).unwrap();
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o600)).unwrap();
    let version = Version::parse(NEW).unwrap();
    assert!(update::on_start(&root.path, &version).is_err());
    let after: PendingFile = serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
    assert!(after.rolling_back);
    assert!(!update::confirm_started(&root.path, &version));
    assert!(journal.is_file());
    fs::rename(keep, old).unwrap();
    assert!(matches!(
        update::on_start(&root.path, &version).unwrap(),
        update::StartAction::RollBack(_)
    ));
}

#[test]
fn a_published_layout_needs_the_supported_migration_before_self_update() {
    let _serial = serial();
    let root = managed_root();
    fs::write(
        root.path.join(update::MARKER),
        b"Metro runner per-user install\n",
    )
    .unwrap();
    let rel = release(runner_archive(NEW, "runner", &stub(0)));
    let f = serve(rel.files, NEW);
    let kind = InstallKind::Managed {
        root: root.path.clone(),
    };
    let u = SelfUpdate::new(kind, Channel::Release, true, Some(updater(&root.path, &f)));
    assert!(!u.enabled);
    assert_eq!(
        updater(&root.path, &f).prepare(NEW).err().unwrap().code,
        "manual_migration_required"
    );
    assert_eq!(f.hits.load(Ordering::SeqCst), 0);
    assert!(!root.path.join(update::LOCK).exists());
}

#[test]
fn a_failing_start_check_changes_nothing() {
    let _serial = serial();
    let root = managed_root();
    let rel = release(runner_archive(NEW, "runner", &stub(3)));
    let f = serve(rel.files.clone(), NEW);
    let e = updater(&root.path, &f).prepare(NEW).err().unwrap();
    assert_eq!(e.code, "start_check_failed");
    assert!(!root.path.join("versions").join(NEW).exists());
    assert_eq!(current(&root.path), Path::new("versions/0.9.0"));
    assert_eq!(
        fs::read(root.path.join(update::SCRIPT)).unwrap(),
        b"#!/bin/sh\n# old installer script\n"
    );
    assert!(leftovers(&root.path).is_empty());
}

#[test]
fn refusals() {
    let _serial = serial();
    // Not newer: no request at all.
    let root = managed_root();
    let rel = release(runner_archive(NEW, "runner", &stub(0)));
    let f = serve(rel.files.clone(), NEW);
    let u = updater(&root.path, &f);
    assert_eq!(u.prepare(OLD).err().unwrap().code, "not_newer");
    assert_eq!(u.prepare("0.8.9").err().unwrap().code, "not_newer");
    assert_eq!(u.prepare("v0.9.1").err().unwrap().code, "bad_version");
    assert_eq!(u.prepare("0.9.1-rc.1").err().unwrap().code, "bad_version");
    assert_eq!(f.hits.load(Ordering::SeqCst), 0);

    // The header names another version than the one served there.
    let f = serve(rel.files.clone(), "0.9.2");
    assert_eq!(
        updater(&root.path, &f).prepare("0.9.2").err().unwrap().code,
        "version_mismatch"
    );
    // A version that doesn't exist.
    let f = serve(rel.files.clone(), NEW);
    assert_eq!(
        updater(&root.path, &f).prepare("0.9.3").err().unwrap().code,
        "download_failed"
    );

    // A tampered archive (bytes changed after signing).
    let mut files = rel.files.clone();
    let name = format!("Metro-runner-{NEW}-linux-x64.tar.gz");
    files.insert(
        name.clone(),
        runner_archive(NEW, "runner", b"#!/bin/sh\necho evil\n"),
    );
    let f = serve(files, NEW);
    assert_eq!(
        updater(&root.path, &f).prepare(NEW).err().unwrap().code,
        "verify_failed"
    );

    // A tampered SHA256SUMS.
    let mut files = rel.files.clone();
    let mut sums = files["SHA256SUMS"].clone();
    sums[0] = if sums[0] == b'0' { b'1' } else { b'0' };
    files.insert("SHA256SUMS".into(), sums);
    let f = serve(files, NEW);
    assert_eq!(
        updater(&root.path, &f).prepare(NEW).err().unwrap().code,
        "verify_failed"
    );

    // A tampered installer script.
    let mut files = rel.files.clone();
    files.insert(
        "install-metro-runner-linux.sh".into(),
        b"#!/bin/sh\nrm -rf ~\n".to_vec(),
    );
    let f = serve(files, NEW);
    assert_eq!(
        updater(&root.path, &f).prepare(NEW).err().unwrap().code,
        "verify_failed"
    );

    // A correctly signed archive of the wrong kind.
    let wrong = release(runner_archive(NEW, "desktop", &stub(0)));
    let f = serve(wrong.files.clone(), NEW);
    assert_eq!(
        updater(&root.path, &f).prepare(NEW).err().unwrap().code,
        "archive_invalid"
    );

    // Nothing was moved by any of them.
    assert_eq!(current(&root.path), Path::new("versions/0.9.0"));
    assert!(!root.path.join("versions").join(NEW).exists());
    assert!(
        leftovers(&root.path).is_empty(),
        "{:?}",
        leftovers(&root.path)
    );
}

#[test]
fn a_busy_installer_lock_stops_the_update() {
    let _serial = serial();
    let root = managed_root();
    fs::create_dir(root.path.join(update::LOCK)).unwrap(); // the script's lock
    let rel = release(runner_archive(NEW, "runner", &stub(0)));
    let f = serve(rel.files.clone(), NEW);
    assert_eq!(
        updater(&root.path, &f).prepare(NEW).err().unwrap().code,
        "installer_busy"
    );
    assert!(!root.path.join("versions").join(NEW).exists());
    assert!(
        root.path.join(update::LOCK).exists(),
        "someone else's lock is left alone"
    );
}

#[test]
fn only_an_enabled_managed_release_runner_acts_on_a_request() {
    let _serial = serial();
    let root = managed_root();
    let rel = release(runner_archive(NEW, "runner", &stub(0)));
    let f = serve(rel.files.clone(), NEW);
    let managed = InstallKind::Managed {
        root: root.path.clone(),
    };
    for (install, channel, opt_in) in [
        (managed.clone(), Channel::Release, false),
        (managed.clone(), Channel::Source, true),
        (managed.clone(), Channel::Preview, true),
        (InstallKind::Unmanaged, Channel::Release, true),
        (InstallKind::Container, Channel::Release, true),
    ] {
        let mut s = SelfUpdate::new(
            install.clone(),
            channel,
            opt_in,
            Some(updater(&root.path, &f)),
        );
        assert!(!s.enabled, "{install:?} {channel:?} {opt_in}");
        s.on_request(NEW);
        let body: serde_json::Value = serde_json::from_str(&s.body()).unwrap();
        assert_eq!(body["runner"]["self_update"], false);
    }
    assert_eq!(
        f.hits.load(Ordering::SeqCst),
        0,
        "a disabled runner made a request"
    );
    // Enabled: a failure is reported in the next claim body, once.
    let mut files = rel.files.clone();
    files.remove("SHA256SUMS.sigstore.json");
    let f = serve(files, NEW);
    let mut s = SelfUpdate::new(
        managed,
        Channel::Release,
        true,
        Some(updater(&root.path, &f)),
    );
    assert!(s.enabled);
    s.on_request(NEW);
    let body: serde_json::Value = serde_json::from_str(&s.body()).unwrap();
    assert_eq!(body["runner"]["update"]["state"], "failed");
    assert_eq!(body["runner"]["update"]["target"], NEW);
    assert_eq!(body["runner"]["update"]["code"], "download_failed");
    assert_eq!(body["runner"]["install"], "managed");
    // A new click for the same version tries again (and reports again).
    let before = f.hits.load(Ordering::SeqCst);
    s.on_request(NEW);
    assert!(f.hits.load(Ordering::SeqCst) > before);
    let body: serde_json::Value = serde_json::from_str(&s.body()).unwrap();
    assert_eq!(body["runner"]["update"]["code"], "download_failed");
    // The same version as this runner's own is never attempted.
    let before = f.hits.load(Ordering::SeqCst);
    s.on_request(OLD);
    assert_eq!(f.hits.load(Ordering::SeqCst), before);
}

#[test]
fn check_start_validates_without_contacting_the_server() {
    let _serial = serial();
    use std::process::Command;
    let ws = tempfile::tempdir().unwrap();
    // A listener that records whether anything connected.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let run = |args: &[&str], envs: &[(&str, &str)]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_metro-runner"));
        c.env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default());
        for (k, v) in envs {
            c.env(k, v);
        }
        c.args(args).output().unwrap()
    };
    let ws_path = ws.path().to_str().unwrap();
    let good = [
        ("METRO_RUNNER_TOKEN", "t"),
        ("METRO_WORKSPACE", ws_path),
        ("METRO_SERVER_URL", url.as_str()),
    ];
    let out = run(&["--check-start"], &good);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        listener.accept().is_err(),
        "the start check contacted the server"
    );
    // A broken configuration fails the check.
    let out = run(&["--check-start"], &[("METRO_WORKSPACE", ws_path)]);
    assert_eq!(out.status.code(), Some(2));
    let out = run(
        &["--check-start"],
        &[
            ("METRO_RUNNER_TOKEN", "t"),
            ("METRO_WORKSPACE", "/nonexistent/x"),
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    let bad = [
        ("METRO_RUNNER_TOKEN", "t"),
        ("METRO_WORKSPACE", ws_path),
        ("METRO_RUNNER_SELF_UPDATE", "maybe"),
    ];
    assert_eq!(run(&["--check-start"], &bad).status.code(), Some(2));
    // Unknown arguments are refused.
    assert_eq!(run(&["--nope"], &good).status.code(), Some(2));
    assert_eq!(run(&["--check-start", "x"], &good).status.code(), Some(2));
}
