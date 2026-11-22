//! The real `metro-desktop` binary in a packaged layout (`<dir>/Metro`,
//! `<dir>/ui/Metro`), with a scripted stand-in for the NSH UI that reports
//! what it was given and probes the bridge. Unix only (the stand-in is a
//! Python script); Windows is covered by the cross build.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const FAKE_UI: &str = r#"#!/usr/bin/env python3
import http.client, json, os, sys, time
out = os.environ["FAKE_UI_OUT"]
url = os.environ.get("METRO_DESKTOP_BRIDGE_URL", "")
tok = os.environ.get("METRO_DESKTOP_BRIDGE_TOKEN", "")
res = {"url": url, "token_len": len(tok), "token_hex": all(c in "0123456789abcdef" for c in tok),
       "nsh_app_dir": os.environ.get("NSH_APP_DIR"), "no_proxy": os.environ.get("NO_PROXY"),
       "cwd": os.getcwd(), "argv": sys.argv[1:], "pid": os.getpid(), "pgid": os.getpgid(0)}
hp = url[len("http://"):]
host, port = hp.split(":")
def req(method, path, headers, body=None):
    c = http.client.HTTPConnection(host, int(port), timeout=5)
    c.request(method, path, body=body, headers=headers)
    r = c.getresponse()
    return r.status, r.read().decode()
auth = {"Authorization": "Bearer " + tok}
s, b = req("GET", "/v1/status", auth)
res["status_code"] = s
res["status"] = json.loads(b)
res["no_auth"] = req("GET", "/v1/status", {})[0]
res["origin"] = req("GET", "/v1/status", dict(auth, Origin="http://evil.example"))[0]
res["local_open"] = req("POST", "/v1/local-workspace/open", dict(auth, **{"Content-Type": "application/json"}), "{}")[0]
if os.environ.get("FAKE_UI_MODE") == "local":
    j = dict(auth, **{"Content-Type": "application/json"})
    s, b = req("POST", "/v1/local-workspace/open", j, "{}")
    res["open_code"] = s
    w = json.loads(b)
    res["local_url"] = w.get("server_url")
    s, b = req("POST", "/v1/session", j, json.dumps({"server_url": w.get("server_url"), "session_token": w.get("token")}))
    res["session_code"] = s
    res["session"] = json.loads(b)
    s, b = req("DELETE", "/v1/session", auth)
    res["close_code"] = s
tmp = out + ".tmp"
with open(tmp, "w") as f:
    json.dump(res, f)
os.rename(tmp, out)
if os.environ.get("FAKE_UI_MODE") == "sleep":
    time.sleep(60)
sys.exit(7)
"#;

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("launch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `<dir>/Metro` (the launcher) and `<dir>/ui/Metro` (the stand-in UI).
fn layout(dir: &Path, with_ui: bool) -> PathBuf {
    let launcher = dir.join("app").join("Metro");
    std::fs::create_dir_all(dir.join("app").join("ui")).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_metro-desktop"), &launcher).unwrap();
    if with_ui {
        let ui = dir.join("app").join("ui").join("Metro");
        std::fs::write(&ui, FAKE_UI).unwrap();
        std::fs::set_permissions(&ui, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    launcher
}

fn command(dir: &Path, launcher: &Path, out: &Path, home: &str) -> Command {
    let mut c = Command::new(launcher);
    c.env("METRO_DESKTOP_HOME", dir.join(home))
        .env("FAKE_UI_OUT", out)
        .env("NSH_APP_DIR", "/somewhere/else")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "example.com")
        .env_remove("no_proxy")
        .env_remove("METRO_DESKTOP_BRIDGE_URL")
        .env_remove("METRO_DESKTOP_BRIDGE_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn wait_file(p: &Path) -> Value {
    let t = Instant::now();
    loop {
        if let Ok(s) = std::fs::read_to_string(p) {
            return serde_json::from_str(&s).unwrap();
        }
        assert!(
            t.elapsed() < Duration::from_secs(20),
            "the UI never reported"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn alive(pid: i64) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[test]
fn launches_the_packaged_ui_with_a_private_bridge_and_cleans_up() {
    let dir = scratch("ok");
    let launcher = layout(&dir, true);
    let out = dir.join("ui.json");
    let o = command(&dir, &launcher, &out, "home").output().unwrap();
    assert_eq!(
        o.status.code(),
        Some(7),
        "stderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let r = wait_file(&out);
    let url = r["url"].as_str().unwrap();
    let port: u16 = url
        .strip_prefix("http://127.0.0.1:")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(r["token_len"], 64);
    assert_eq!(r["token_hex"], true);
    assert_eq!(
        r["nsh_app_dir"],
        Value::Null,
        "NSH_APP_DIR must not reach the UI"
    );
    assert_eq!(r["no_proxy"], "example.com,127.0.0.1,localhost,::1");
    assert_eq!(r["argv"], serde_json::json!([]));
    let ui_dir = std::fs::canonicalize(dir.join("app/ui")).unwrap();
    assert_eq!(Path::new(r["cwd"].as_str().unwrap()), ui_dir);
    assert_eq!(r["status_code"], 200);
    assert_eq!(r["status"]["instance"], "primary");
    assert_eq!(r["status"]["session"], Value::Null);
    assert_eq!(r["status"]["local"], "unavailable"); // no metro-server beside it
    assert_eq!(r["no_auth"], 401);
    assert_eq!(r["origin"], 403);
    assert_eq!(r["local_open"], 503);
    // The bridge is gone with the UI.
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    // Nothing printed the token.
    let token_free = |b: &[u8]| !String::from_utf8_lossy(b).contains("METRO_DESKTOP_BRIDGE_TOKEN=");
    assert!(token_free(&o.stdout) && token_free(&o.stderr));
    // The data root is private.
    let mode = std::fs::metadata(dir.join("home"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn refuses_an_incomplete_install() {
    let dir = scratch("missing");
    let launcher = layout(&dir, false);
    let o = command(&dir, &launcher, &dir.join("ui.json"), "home")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("incomplete"));

    // A symlinked UI is not a shipped component either.
    let target = dir.join("elsewhere");
    std::fs::write(&target, FAKE_UI).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&target, dir.join("app/ui/Metro")).unwrap();
    let o = command(&dir, &launcher, &dir.join("ui.json"), "home")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(!dir.join("ui.json").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn stopping_the_launcher_ends_the_ui_and_a_second_instance_is_secondary() {
    let dir = scratch("stop");
    let launcher = layout(&dir, true);
    let out = dir.join("first.json");
    let mut first = command(&dir, &launcher, &out, "home");
    first.env("FAKE_UI_MODE", "sleep");
    let mut first = first.spawn().unwrap();
    let r = wait_file(&out);
    let pid = r["pid"].as_i64().unwrap();
    assert_eq!(r["status"]["instance"], "primary");
    assert_ne!(
        r["pgid"].as_i64().unwrap(),
        first.id() as i64,
        "UI has its own process group"
    );

    // Same user, same data root: the second window serves nothing.
    let out2 = dir.join("second.json");
    let o = command(&dir, &launcher, &out2, "home").output().unwrap();
    assert_eq!(o.status.code(), Some(7));
    assert_eq!(wait_file(&out2)["status"]["instance"], "secondary");

    assert!(alive(pid));
    // SAFETY: signalling our own child.
    unsafe {
        libc::kill(first.id() as i32, libc::SIGTERM);
    }
    let t = Instant::now();
    let status = loop {
        if let Some(s) = first.try_wait().unwrap() {
            break s;
        }
        assert!(
            t.elapsed() < Duration::from_secs(15),
            "launcher did not stop"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    // The UI was ended by SIGTERM: 128 + 15.
    assert_eq!(status.code(), Some(143));
    let t = Instant::now();
    while alive(pid) {
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "the UI outlived the launcher"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_ui_dies_with_a_killed_launcher() {
    let dir = scratch("kill");
    let launcher = layout(&dir, true);
    let out = dir.join("ui.json");
    let mut c = command(&dir, &launcher, &out, "home");
    c.env("FAKE_UI_MODE", "sleep");
    let mut child = c.spawn().unwrap();
    let pid = wait_file(&out)["pid"].as_i64().unwrap();
    child.kill().unwrap(); // SIGKILL: no cleanup code runs in the launcher
    child.wait().unwrap();
    let t = Instant::now();
    while alive(pid) {
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "the UI outlived a killed launcher"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The bundled local workspace end to end: the launcher starts the real
/// `metro-server` beside it on demand, the UI signs in to it with the
/// in-memory bearer, and the server is gone after exit.
#[test]
fn local_workspace_through_the_launcher() {
    let server = std::env::var_os("METRO_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/metro-server")
        });
    if !server.is_file() {
        eprintln!(
            "skipped: build metro-server (cargo build -p metro-server) or set METRO_SERVER_BIN"
        );
        return;
    }
    let dir = scratch("local");
    let launcher = layout(&dir, true);
    std::fs::copy(&server, dir.join("app/metro-server")).unwrap();
    let out = dir.join("ui.json");
    let mut c = command(&dir, &launcher, &out, "home");
    c.env("FAKE_UI_MODE", "local");
    let o = c.output().unwrap();
    let r = wait_file(&out);
    assert_eq!(
        o.status.code(),
        Some(7),
        "{r}\nstderr: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(r["status"]["local"], "stopped", "{r}");
    assert_eq!(r["open_code"], 200, "{r}");
    let url = r["local_url"].as_str().unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert_eq!(r["session_code"], 200, "{r}");
    assert_eq!(r["session"]["session"]["server_url"], url);
    assert_eq!(r["session"]["local"], "running");
    assert_eq!(r["close_code"], 200);
    // The local server stopped with the app.
    let port: u16 = url.rsplit(':').next().unwrap().parse().unwrap();
    let t = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "local server outlived the app"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The bearer was never left on disk.
    let token_files: Vec<_> = walk(&dir.join("home"))
        .into_iter()
        .filter(|p| p.ends_with("dev-users.json"))
        .collect();
    assert!(token_files.is_empty(), "{token_files:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}
