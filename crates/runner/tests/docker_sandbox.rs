//! Docker sandbox tests.
//!
//! - Fail-closed tests need only a working `docker` CLI/daemon; they are
//!   skipped (with a message) when docker is unavailable.
//! - Isolation tests additionally need `METRO_TEST_DOCKER_IMAGE`: a locally
//!   present, digest-pinned image with `/bin/sh`, `id`, `ls`, `grep`, `sleep`
//!   (busybox/alpine-based), e.g. `redis@sha256:...` from `docker images --digests`.

mod common;

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::config;
use metro_runner::confine::Workspace;
use metro_runner::tools::Registry;
use serde_json::json;

const MISSING_IMAGE: &str = "metro-runner-test-missing@sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn docker_ok() -> bool {
    Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Docker tests share the per-process container-name prefix; serialize them so
/// the "no container left behind" checks don't see a sibling test's container.
static DOCKER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    DOCKER_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn image() -> Option<String> {
    std::env::var("METRO_TEST_DOCKER_IMAGE")
        .ok()
        .filter(|s| !s.is_empty())
}

fn registry(ws: &std::path::Path, extra: &[(&str, &str)]) -> Registry {
    let cfg = config(ws, extra);
    let w = Workspace::open(&cfg.workspace).unwrap();
    Registry::standard(Arc::new(cfg), Arc::new(w))
}

fn our_containers() -> String {
    let out = Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            "label=dev.metro.runner=1",
            "--filter",
            &format!("name=metro-runner-{}-", std::process::id()),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn missing_image_fails_closed_without_host_fallback() {
    let _g = lock();
    if !docker_ok() {
        eprintln!("skipping: docker not available");
        return;
    }
    let ws = tempfile::tempdir().unwrap();
    let marker = ws.path().join("ran-on-host");
    let argv =
        serde_json::to_string(&["/bin/sh", "-c", &format!("touch {}", marker.display())]).unwrap();
    let r = registry(
        ws.path(),
        &[
            ("METRO_SANDBOX", "docker"),
            ("METRO_SANDBOX_IMAGE", MISSING_IMAGE),
            ("METRO_CHECK_ARGV", &argv),
        ],
    );
    let out = r.dispatch("workspace.check", &json!({}));
    assert!(!out.ok, "{}", out.output);
    assert!(!marker.exists(), "check must never fall back to the host");

    let out = r.dispatch("workspace.mcp_list", &json!({}));
    assert!(!out.ok, "{}", out.output);
}

#[test]
fn binary_preflight_refuses_missing_image() {
    if !docker_ok() {
        eprintln!("skipping: docker not available");
        return;
    }
    let ws = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_metro-runner"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("METRO_RUNNER_TOKEN", "t")
        .env("METRO_WORKSPACE", ws.path())
        .env("METRO_SERVER_URL", "http://127.0.0.1:9")
        .env("METRO_SANDBOX", "docker")
        .env("METRO_SANDBOX_IMAGE", MISSING_IMAGE)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("no host fallback"));
}

#[test]
fn binary_refuses_unpinned_image_and_missing_token() {
    let ws = tempfile::tempdir().unwrap();
    let run = |envs: &[(&str, &str)]| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_metro-runner"));
        c.env_clear().env("METRO_WORKSPACE", ws.path());
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    };
    let o = run(&[
        ("METRO_RUNNER_TOKEN", "t"),
        ("METRO_SANDBOX", "docker"),
        ("METRO_SANDBOX_IMAGE", "alpine:latest"),
    ]);
    assert_eq!(o.status.code(), Some(2));
    let o = run(&[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("METRO_RUNNER_TOKEN"));
}

#[test]
fn check_runs_isolated() {
    let _g = lock();
    let Some(img) = image() else {
        eprintln!("skipping: METRO_TEST_DOCKER_IMAGE not set");
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("in.txt"), "from-workspace").unwrap();
    let script = "id -u; id -g; ls /sys/class/net | tr '\\n' ' '; echo; \
        cat /workspace/in.txt; echo; \
        (touch /workspace/x 2>/dev/null && echo ws-WRITABLE) || echo ws-ro; \
        (touch /rootfile 2>/dev/null && echo root-WRITABLE) || echo root-ro; \
        (touch /tmp/t && echo tmp-ok); \
        grep CapEff /proc/self/status; \
        grep NoNewPrivs /proc/self/status; \
        ls /var/run/docker.sock 2>/dev/null || echo no-sock; \
        env | grep -c METRO || true";
    let argv = serde_json::to_string(&["/bin/sh", "-c", script]).unwrap();
    // SAFETY: test-only; ensure the token exists in our env to prove it isn't forwarded.
    unsafe { std::env::set_var("METRO_RUNNER_TOKEN", "must-not-leak") };
    let r = registry(
        ws.path(),
        &[
            ("METRO_SANDBOX", "docker"),
            ("METRO_SANDBOX_IMAGE", &img),
            ("METRO_SANDBOX_MEMORY", "128m"),
            ("METRO_SANDBOX_PIDS", "32"),
            ("METRO_CHECK_ARGV", &argv),
            ("METRO_CHECK_TIMEOUT_SECS", "60"),
        ],
    );
    let out = r.dispatch("workspace.check", &json!({}));
    assert!(out.ok, "{}", out.output);
    let o = &out.output;
    let expect = [
        "65534\n65534\n",
        "lo \n",
        "from-workspace",
        "ws-ro",
        "root-ro",
        "tmp-ok",
        "CapEff:\t0000000000000000",
        "NoNewPrivs:\t1",
        "no-sock",
    ];
    for e in expect {
        assert!(o.contains(e), "missing {e:?} in:\n{o}");
    }
    assert!(!o.contains("WRITABLE"), "{o}");
    assert!(o.contains("\n0\n"), "METRO_* env leaked: {o}");
    assert!(!ws.path().join("x").exists());
    assert_eq!(our_containers(), "", "container left behind");
}

#[test]
fn check_timeout_removes_container() {
    let _g = lock();
    let Some(img) = image() else {
        eprintln!("skipping: METRO_TEST_DOCKER_IMAGE not set");
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    // `trap '' TERM` so only a forced removal stops it.
    let argv = serde_json::to_string(&["/bin/sh", "-c", "trap '' TERM INT; sleep 300"]).unwrap();
    let r = registry(
        ws.path(),
        &[
            ("METRO_SANDBOX", "docker"),
            ("METRO_SANDBOX_IMAGE", &img),
            ("METRO_CHECK_ARGV", &argv),
            ("METRO_CHECK_TIMEOUT_SECS", "3"),
        ],
    );
    let t = Instant::now();
    let out = r.dispatch("workspace.check", &json!({}));
    assert!(
        !out.ok && out.output.contains("timed out"),
        "{}",
        out.output
    );
    assert!(t.elapsed() < Duration::from_secs(40), "{:?}", t.elapsed());
    assert_eq!(our_containers(), "", "container survived timeout");
}

#[test]
fn mcp_over_docker_stdio() {
    let _g = lock();
    let Some(img) = image() else {
        eprintln!("skipping: METRO_TEST_DOCKER_IMAGE not set");
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    // A tiny scripted MCP peer: answers initialize (id 1) and tools/list (id 2).
    let script = r#"read l; echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"sh","version":"0"}}}'; read l; read l; echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"in_docker","inputSchema":{"type":"object"}}]}}'; read l"#;
    let argv = serde_json::to_string(&["/bin/sh", "-c", script]).unwrap();
    let r = registry(
        ws.path(),
        &[
            ("METRO_SANDBOX", "docker"),
            ("METRO_SANDBOX_IMAGE", &img),
            ("METRO_MCP_ARGV", &argv),
            ("METRO_MCP_TIMEOUT_SECS", "60"),
        ],
    );
    let out = r.dispatch("workspace.mcp_list", &json!({}));
    assert!(out.ok, "{}", out.output);
    assert!(out.output.contains("in_docker"));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(our_containers(), "", "container left behind");
}

/// Reusable credential + docker: the container sees only the claiming
/// Space's root, read-only, and nothing from the base or a sibling Space.
#[test]
fn reusable_check_mounts_only_the_claiming_space() {
    let _g = lock();
    let Some(img) = image() else {
        eprintln!("skipping: METRO_TEST_DOCKER_IMAGE not set");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    std::fs::write(base.path().join("root-secret"), "ROOT").unwrap();
    for s in ["a", "b"] {
        let p = base.path().join("spaces").join(format!("s-{s}"));
        std::fs::create_dir_all(&p).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let f = p.join(format!("marker-{s}"));
        std::fs::write(&f, s).unwrap();
        std::fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    }
    let script = "ls -A /workspace | tr '\\n' ' '; echo; \
        (ls /workspace/.. ; ls /workspace/../s-b) 2>&1 | grep -c marker-b; \
        (touch /workspace/w 2>/dev/null && echo ws-WRITABLE) || echo ws-ro; \
        env | grep -c METRO || true";
    let argv = serde_json::to_string(&["/bin/sh", "-c", script]).unwrap();
    let cfg = config(
        base.path(),
        &[
            ("METRO_RUNNER_TOKEN", "metro_rt_v2_docker-test"),
            ("METRO_SANDBOX", "docker"),
            ("METRO_SANDBOX_IMAGE", &img),
            ("METRO_CHECK_ARGV", &argv),
            ("METRO_CHECK_TIMEOUT_SECS", "60"),
        ],
    );
    let roots = metro_runner::spaces::SpaceRoots::open(&cfg.workspace).unwrap();
    let ws = roots.open_space("s-a").unwrap();
    let r = Registry::for_space(Arc::new(cfg), Arc::new(ws));
    let out = r.dispatch("workspace.check", &json!({}));
    assert!(out.ok, "{}", out.output);
    let o = &out.output;
    assert!(o.contains("marker-a \n"), "only s-a's files expected:\n{o}");
    assert!(!o.contains("root-secret") && !o.contains("WRITABLE"), "{o}");
    assert!(o.contains("ws-ro"), "{o}");
    assert!(
        o.contains("\n0\nws-ro\n0\n"),
        "sibling visible or env leaked:\n{o}"
    );
    assert_eq!(our_containers(), "", "container left behind");
}
