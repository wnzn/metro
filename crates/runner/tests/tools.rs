//! Tool dispatch, check subprocess and stdio MCP through the registry.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{config, fixture};
use metro_runner::confine::Workspace;
use metro_runner::tools::Registry;
use serde_json::{Value, json};

fn registry(ws: &std::path::Path, extra: &[(&str, &str)]) -> Registry {
    let cfg = config(ws, extra);
    let w = Workspace::open(&cfg.workspace).unwrap();
    Registry::standard(Arc::new(cfg), Arc::new(w))
}

fn mcp_argv(mode: &str) -> String {
    serde_json::to_string(&[
        "python3",
        "-I",
        fixture("fake_mcp.py").to_str().unwrap(),
        mode,
    ])
    .unwrap()
}

fn have_python() -> bool {
    std::process::Command::new("python3")
        .arg("-c")
        .arg("1")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn registry_names() {
    let ws = tempfile::tempdir().unwrap();
    let r = registry(ws.path(), &[]);
    assert_eq!(
        r.names(),
        vec![
            "workspace.check",
            "workspace.list",
            "workspace.mcp",
            "workspace.mcp_list",
            "workspace.read"
        ]
    );
}

#[test]
fn list_defaults_to_root_and_validates_args() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("a"), "1").unwrap();
    let r = registry(ws.path(), &[]);
    let out = r.dispatch("workspace.list", &json!({}));
    assert!(out.ok, "{}", out.output);
    let v: Value = serde_json::from_str(&out.output).unwrap();
    assert_eq!(v["path"], ".");
    assert_eq!(
        v["entries"][0],
        json!({"name": "a", "type": "file", "size": 1})
    );

    assert!(!r.dispatch("workspace.list", &json!({"path": 5})).ok);
    assert!(
        !r.dispatch("workspace.list", &json!({"path": ".", "recursive": true}))
            .ok
    );
    assert!(!r.dispatch("workspace.list", &json!([])).ok);
    assert!(!r.dispatch("workspace.read", &json!({})).ok);
    assert!(
        !r.dispatch("workspace.read", &json!({"path": "/etc/passwd"}))
            .ok
    );
    assert!(
        !r.dispatch("workspace.check", &json!({"argv": ["sh"]})).ok,
        "check takes no arguments"
    );
}

#[test]
fn check_not_configured() {
    let ws = tempfile::tempdir().unwrap();
    let r = registry(ws.path(), &[]);
    let out = r.dispatch("workspace.check", &json!({}));
    assert!(!out.ok);
    assert!(out.output.contains("METRO_CHECK_ARGV"));
    let out = r.dispatch("workspace.mcp_list", &json!({}));
    assert!(!out.ok && out.output.contains("METRO_MCP_ARGV"));
}

#[test]
fn check_reports_exit_and_streams() {
    let ws = tempfile::tempdir().unwrap();
    let argv = r#"["/bin/sh","-c","echo out; echo err >&2; exit 3"]"#;
    let r = registry(ws.path(), &[("METRO_CHECK_ARGV", argv)]);
    let out = r.dispatch("workspace.check", &json!({}));
    assert!(!out.ok);
    assert_eq!(
        out.output,
        "exit: 3\n--- stdout ---\nout\n--- stderr ---\nerr\n"
    );

    let r = registry(ws.path(), &[("METRO_CHECK_ARGV", r#"["/bin/true"]"#)]);
    let out = r.dispatch("workspace.check", &json!({}));
    assert!(out.ok, "{}", out.output);
}

#[test]
fn check_timeout() {
    let ws = tempfile::tempdir().unwrap();
    let r = registry(
        ws.path(),
        &[
            ("METRO_CHECK_ARGV", r#"["/bin/sleep","30"]"#),
            ("METRO_CHECK_TIMEOUT_SECS", "1"),
        ],
    );
    let t = Instant::now();
    let out = r.dispatch("workspace.check", &json!({}));
    assert!(!out.ok);
    assert!(out.output.contains("timed out"), "{}", out.output);
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn mcp_list_paginates() {
    if !have_python() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let ws = tempfile::tempdir().unwrap();
    let r = registry(ws.path(), &[("METRO_MCP_ARGV", &mcp_argv("normal"))]);
    let out = r.dispatch("workspace.mcp_list", &json!({}));
    assert!(out.ok, "{}", out.output);
    let v: Value = serde_json::from_str(&out.output).unwrap();
    let names: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["echo", "fail", "env", "page2"]);
    assert!(v[0]["inputSchema"].is_object());
}

#[test]
fn mcp_call_paths() {
    if !have_python() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let ws = tempfile::tempdir().unwrap();
    let r = registry(ws.path(), &[("METRO_MCP_ARGV", &mcp_argv("normal"))]);

    let out = r.dispatch(
        "workspace.mcp",
        &json!({"name": "echo", "arguments": {"text": "hey"}}),
    );
    assert_eq!((out.ok, out.output.as_str()), (true, "echo:hey"));

    let out = r.dispatch("workspace.mcp", &json!({"name": "fail", "arguments": {}}));
    assert_eq!((out.ok, out.output.as_str()), (false, "it broke"));

    let out = r.dispatch("workspace.mcp", &json!({"name": "nope", "arguments": {}}));
    assert!(!out.ok && out.output.contains("-32602"), "{}", out.output);

    // No inherited secrets, cwd is the workspace.
    // SAFETY: test-only env mutation before spawning.
    unsafe { std::env::set_var("METRO_RUNNER_TOKEN", "should-not-leak") };
    let out = r.dispatch("workspace.mcp", &json!({"name": "env", "arguments": {}}));
    assert!(out.ok);
    let canon = std::fs::canonicalize(ws.path()).unwrap();
    assert_eq!(out.output, format!("cwd={} keys=", canon.display()));

    assert!(
        !r.dispatch("workspace.mcp", &json!({"name": "echo", "arguments": [1]}))
            .ok
    );
    assert!(!r.dispatch("workspace.mcp", &json!({"arguments": {}})).ok);
}

#[test]
fn mcp_failures_fail_cleanly() {
    if !have_python() {
        eprintln!("skipping: python3 not available");
        return;
    }
    let ws = tempfile::tempdir().unwrap();

    let r = registry(ws.path(), &[("METRO_MCP_ARGV", &mcp_argv("badversion"))]);
    let out = r.dispatch("workspace.mcp_list", &json!({}));
    assert!(
        !out.ok && out.output.contains("unsupported MCP protocol version"),
        "{}",
        out.output
    );

    let r = registry(ws.path(), &[("METRO_MCP_ARGV", &mcp_argv("notools"))]);
    let out = r.dispatch("workspace.mcp_list", &json!({}));
    assert!(
        !out.ok && out.output.contains("'tools' capability"),
        "{}",
        out.output
    );

    let r = registry(ws.path(), &[("METRO_MCP_ARGV", &mcp_argv("crash"))]);
    let out = r.dispatch("workspace.mcp_list", &json!({}));
    assert!(
        !out.ok && out.output.contains("exited") && out.output.contains("fatal: boom"),
        "{}",
        out.output
    );

    let r = registry(
        ws.path(),
        &[
            ("METRO_MCP_ARGV", &mcp_argv("hang")),
            ("METRO_MCP_TIMEOUT_SECS", "1"),
        ],
    );
    let t = Instant::now();
    let out = r.dispatch("workspace.mcp_list", &json!({}));
    assert!(
        !out.ok && out.output.contains("timed out"),
        "{}",
        out.output
    );
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());

    let r = registry(ws.path(), &[("METRO_MCP_ARGV", r#"["/nonexistent/mcp"]"#)]);
    let out = r.dispatch("workspace.mcp_list", &json!({}));
    assert!(
        !out.ok && out.output.contains("failed to start"),
        "{}",
        out.output
    );
}

/// Opt-in interop check against a real MCP server, e.g.
/// `METRO_TEST_REAL_MCP_ARGV='["uvx","mcp-server-time"]'`.
#[test]
fn real_mcp_server_interop() {
    let Some(argv) = std::env::var("METRO_TEST_REAL_MCP_ARGV")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        eprintln!("skipping: METRO_TEST_REAL_MCP_ARGV not set");
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    let r = registry(
        ws.path(),
        &[("METRO_MCP_ARGV", &argv), ("METRO_MCP_TIMEOUT_SECS", "90")],
    );
    let out = r.dispatch("workspace.mcp_list", &json!({}));
    assert!(out.ok, "{}", out.output);
    let tools: Value = serde_json::from_str(&out.output).unwrap();
    assert!(!tools.as_array().unwrap().is_empty());
    eprintln!(
        "real MCP tools: {}",
        tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let (Ok(name), Ok(args)) = (
        std::env::var("METRO_TEST_REAL_MCP_TOOL"),
        std::env::var("METRO_TEST_REAL_MCP_ARGS"),
    ) {
        let args: Value = serde_json::from_str(&args).unwrap();
        let out = r.dispatch("workspace.mcp", &json!({"name": name, "arguments": args}));
        eprintln!("real MCP call ok={} output={}", out.ok, out.output);
        assert!(out.ok, "{}", out.output);
    }
}
