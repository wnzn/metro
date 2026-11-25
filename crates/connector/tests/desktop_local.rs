//! The desktop's local workspace against the real `metro-server` binary.
//!
//! The server comes from `METRO_SERVER_BIN`, else the workspace's
//! `target/debug/metro-server` (build it first: `cargo build -p metro-server`).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use metro_connector::desktop::local::{LocalState, LocalWorkspaceHost};
use metro_connector::desktop_local::{LOCAL_USER, LocalServer};
use serde_json::Value;

fn server_bin() -> PathBuf {
    let path = std::env::var_os("METRO_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/metro-server")
        });
    assert!(
        path.is_file(),
        "{} is missing: run `cargo build -p metro-server` or set METRO_SERVER_BIN",
        path.display()
    );
    path.canonicalize().unwrap()
}

fn scratch() -> tempfile::TempDir {
    // Not /tmp: keep test data on the same disk as the build.
    let base = option_env!("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix("desktop-local-")
        .tempdir_in(base)
        .unwrap()
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

async fn get(url: &str, token: &str) -> (u16, Value) {
    let r = http().get(url).bearer_auth(token).send().await.unwrap();
    let status = r.status().as_u16();
    let body = r.bytes().await.unwrap_or_default();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// The server (a child of this test process) has exited and its parent's
/// wait can see it. Asks the OS directly with `WNOWAIT`, so the launcher can
/// still reap it; `ECHILD` means the launcher already has. (Guessing this
/// from `/proc` thread lists raced with the kernel's delayed exit report of a
/// multithreaded process and failed CI once.)
fn exited(pid: u32) -> bool {
    // SAFETY: an all-zero siginfo_t is valid; waitid only fills it in.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: WNOWAIT leaves the child waitable; `info` outlives the call.
    let r = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if r != 0 {
        let err = std::io::Error::last_os_error();
        assert_eq!(err.raw_os_error(), Some(libc::ECHILD), "waitid: {err}");
        return true;
    }
    // With WNOHANG and nothing to report, the zeroed siginfo stays zero.
    info.si_signo == libc::SIGCHLD
}

fn server_pid(root: &Path) -> u32 {
    let ready: Value =
        serde_json::from_slice(&std::fs::read(root.join("run/ready.json")).unwrap()).unwrap();
    ready["pid"].as_u64().unwrap() as u32
}

async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// No file under `root` (database included) holds `needle`.
fn assert_nowhere(root: &Path, needle: &str) {
    for entry in walk(root) {
        let bytes = std::fs::read(&entry).unwrap_or_default();
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "a secret was found in {}",
            entry.display()
        );
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn starts_authenticates_persists_and_stops() {
    let bin = server_bin();
    let dir = scratch();
    let root = dir.path().join("local");
    let host = LocalServer::new(bin.clone(), root.clone());
    assert_eq!(host.state(), LocalState::Stopped);
    assert!(host.current().is_none());

    // Fresh instance: starts, and its bearer signs in the local admin.
    let (a, b) = tokio::join!(host.open(), host.open());
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(
        a.server_url, b.server_url,
        "concurrent opens share one server"
    );
    assert_eq!(a.token.expose(), b.token.expose());
    assert_eq!(host.state(), LocalState::Running);
    let url = a.server_url.clone();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert_eq!(
        metro_connector::netpolicy::validate_base_url(&url, "x").unwrap(),
        url
    );
    let token = a.token.expose().to_string();
    let (status, me) = get(&format!("{url}/v1/me"), &token).await;
    assert_eq!(status, 200);
    assert_eq!(me["id"], LOCAL_USER);
    assert_eq!(me["is_org_admin"], true);
    let (status, _) = get(&format!("{url}/v1/me"), &"0".repeat(64)).await;
    assert_eq!(status, 401);
    // Org admin of its own server: managed users are reachable.
    let (status, _) = get(&format!("{url}/v1/admin/users"), &token).await;
    assert_eq!(status, 200);
    let cur = host.current().unwrap();
    assert_eq!(cur.server_url, url);
    let via_trait = LocalWorkspaceHost::current(&host).unwrap();
    assert_eq!(via_trait.server_url, url);
    assert_eq!(via_trait.token.expose(), token);
    assert_eq!(host.service_url().as_deref(), Some(url.as_str()));
    assert_eq!(host.token().unwrap().expose(), token);

    // Private files; the bootstrap file is gone; the bearer is on no disk.
    for d in ["", "db", "keys", "run"] {
        assert_eq!(mode(&root.join(d)), 0o700, "{d}");
    }
    assert_eq!(mode(&root.join("keys/provider-master.key")), 0o600);
    assert_eq!(mode(&root.join("run/server.log")), 0o600);
    assert!(!root.join("run/dev-users.json").exists());
    let saved: Value =
        serde_json::from_slice(&std::fs::read(root.join("state.json")).unwrap()).unwrap();
    let port = saved["port"].as_u64().unwrap();
    assert!(url.ends_with(&format!(":{port}")));
    assert!(std::net::TcpListener::bind(("127.0.0.1", port as u16)).is_err());

    // Some data, then a second host on the same root is refused.
    let created = http()
        .post(format!("{url}/v1/spaces"))
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(r#"{"name":"Solo"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(created.status().as_u16(), 201);
    let other = LocalServer::new(bin.clone(), root.clone());
    let err = other.open().await.err().unwrap();
    assert!(err.as_str().contains("already open"), "{err}");
    drop(other);

    // Stop: the process ends and credentials are gone. The app is quitting,
    // so the launcher gives the address up.
    let pid = server_pid(&root);
    assert!(pid_alive(pid));
    host.shutdown().await;
    assert_eq!(host.state(), LocalState::Stopped);
    assert!(host.current().is_none());
    assert!(!pid_alive(pid));
    drop(std::net::TcpListener::bind(("127.0.0.1", port as u16)).expect("released at shutdown"));
    host.shutdown().await; // idempotent

    // Reopen: same origin, new bearer, the old one is dead, data is kept.
    let again = host.open().await.unwrap();
    assert_eq!(again.server_url, url);
    assert_ne!(again.token.expose(), token);
    let (status, _) = get(&format!("{url}/v1/me"), &token).await;
    assert_eq!(status, 401);
    let (status, spaces) = get(&format!("{url}/v1/spaces"), again.token.expose()).await;
    assert_eq!(status, 200);
    let names: Vec<&str> = spaces["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(names, ["Solo"]);
    let token2 = again.token.expose().to_string();

    // A crash is seen synchronously: once the OS shows the process exited,
    // the very next read reports no server (no background thread involved).
    let pid = server_pid(&root);
    assert!(host.current().is_some());
    // SAFETY: killing our own test's child server.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    wait_until("the server process to exit", || exited(pid)).await;
    assert!(host.current().is_none(), "a dead server is never current");
    assert_eq!(host.state(), LocalState::Failed);
    assert!(host.service_url().is_none() && host.token().is_none());
    let dyn_host: &dyn LocalWorkspaceHost = &host;
    assert!(dyn_host.current().is_none());
    // The server is gone, but the launcher still owns its address: no other
    // program can bind it while the app runs, so nothing the UI or a worker
    // still sends there can reach anyone else.
    assert!(std::net::TcpListener::bind(("127.0.0.1", port as u16)).is_err());
    assert!(std::net::TcpListener::bind(("0.0.0.0", port as u16)).is_err());
    let third = host.open().await.unwrap();
    assert_ne!(third.token.expose(), token2);
    let (status, spaces) = get(&format!("{url}/v1/spaces"), third.token.expose()).await;
    assert_eq!(status, 200);
    assert_eq!(spaces["spaces"][0]["name"], "Solo");
    let (status, _) = get(&format!("{url}/v1/me"), &token2).await;
    assert_eq!(status, 401);
    assert_eq!(third.server_url, url);
    assert_eq!(host.state(), LocalState::Running);

    // The saved port taken by another program: a clear error, no move.
    host.shutdown().await;
    let squatter = std::net::TcpListener::bind(("127.0.0.1", port as u16)).unwrap();
    let err = host.open().await.err().unwrap();
    assert!(err.as_str().contains(&port.to_string()), "{err}");
    assert!(err.as_str().contains("another program"), "{err}");
    assert_eq!(host.state(), LocalState::Failed);
    drop(squatter);
    let fourth = host.open().await.unwrap();
    assert_eq!(fourth.server_url, url);
    let token4 = fourth.token.expose().to_string();
    host.shutdown().await;
    drop(std::net::TcpListener::bind(("127.0.0.1", port as u16)).expect("released at shutdown"));

    for secret in [&token, &token2, &token4] {
        assert_nowhere(&root, secret);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_binary_is_a_clear_error() {
    let dir = scratch();
    let host = LocalServer::new(dir.path().join("metro-server"), dir.path().join("local"));
    let err = host.open().await.err().unwrap();
    assert!(err.as_str().contains("no local server"), "{err}");
    assert_eq!(host.state(), LocalState::Failed);
}

#[tokio::test]
async fn exiting_child_is_detected_during_readiness_wait() {
    let dir = scratch();
    let bin = dir.path().join("metro-server");
    std::fs::write(&bin, "#!/bin/sh\nexit 3\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = dir.path().join("local");
    let host = LocalServer::new(bin, root.clone());

    let err = host.open().await.err().unwrap();
    assert!(err.as_str().contains("stopped while starting"), "{err}");
    assert_eq!(host.state(), LocalState::Failed);
    assert!(host.current().is_none());
    assert!(!root.join("run/dev-users.json").exists());
}

/// The launcher dying without `shutdown` (SIGKILL) leaves no server behind,
/// and the developer's own METRO_* settings never reach the local server.
#[tokio::test(flavor = "multi_thread")]
async fn no_server_outlives_a_killed_launcher() {
    let dir = scratch();
    let root = dir.path().join("local");
    let mut launcher = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "launcher_process",
            "--include-ignored",
            "--nocapture",
        ])
        .env("DESKTOP_LOCAL_TEST_ROOT", &root)
        .env("METRO_SERVER_BIN", server_bin())
        // Would make a server fail to start (two auth modes) if inherited.
        .env("METRO_OIDC_ISSUER", "https://issuer.invalid")
        .env("METRO_DEV_USERS", r#"{"mallory":"0123456789abcdef0123"}"#)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let ready = root.join("launcher-ready");
    wait_until("the launcher to open", || ready.exists()).await;
    let pid = server_pid(&root);
    assert!(pid_alive(pid));
    launcher.kill().unwrap(); // SIGKILL: no cleanup code runs
    launcher.wait().unwrap();
    wait_until("the orphaned server to exit", || !pid_alive(pid)).await;
    // With the launcher and its server gone, the address is free again.
    let saved: Value =
        serde_json::from_slice(&std::fs::read(root.join("state.json")).unwrap()).unwrap();
    let port = saved["port"].as_u64().unwrap() as u16;
    wait_until("the port to be released", || {
        std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
    })
    .await;
}

/// The child half of `no_server_outlives_a_killed_launcher`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "run by no_server_outlives_a_killed_launcher"]
async fn launcher_process() {
    let Some(root) = std::env::var_os("DESKTOP_LOCAL_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let host = LocalServer::new(server_bin(), root.clone());
    let ws = host.open().await.unwrap();
    let (status, me) = get(&format!("{}/v1/me", ws.server_url), ws.token.expose()).await;
    assert_eq!((status, me["id"].as_str()), (200, Some(LOCAL_USER)));
    std::fs::write(root.join("launcher-ready"), b"1").unwrap();
    std::future::pending::<()>().await;
}
