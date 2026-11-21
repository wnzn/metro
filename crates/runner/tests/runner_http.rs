//! End-to-end: claim -> dispatch -> result against a mock server.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{MockServer, Resp, config};
use metro_runner::client::ServerClient;
use metro_runner::confine::Workspace;
use metro_runner::runner::{Runner, Step};
use metro_runner::tools::Registry;
use serde_json::Value;

fn runner_for(server: &str, ws: &std::path::Path, extra: &[(&str, &str)]) -> Runner {
    let cfg = config(ws, extra);
    let client = ServerClient::with(server, cfg.token.expose(), Duration::from_secs(5));
    let w = Workspace::open(&cfg.workspace).unwrap();
    let mut r = Runner::new(
        client,
        Registry::standard(Arc::new(cfg), Arc::new(w)),
        Duration::from_millis(10),
    );
    r.result_backoff =
        metro_runner::util::Backoff::new(Duration::from_millis(5), Duration::from_millis(20));
    r
}

#[test]
fn idle_on_204_with_bearer_auth() {
    let ws = tempfile::tempdir().unwrap();
    let srv = MockServer::start(|_, _| Resp::new(204, ""));
    let mut r = runner_for(&srv.url, ws.path(), &[]);
    assert_eq!(r.step(), Step::Idle);
    let reqs = srv.requests();
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/v1/runner/claim");
    assert_eq!(reqs[0].body, "{}");
    assert_eq!(reqs[0].headers["authorization"], "Bearer test-token");
    assert_eq!(reqs[0].headers["content-type"], "application/json");
}

#[test]
fn dispatches_list_and_reports_result() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("hello.txt"), "hi").unwrap();
    let srv = MockServer::start(|req, _| {
        if req.path == "/v1/runner/claim" {
            Resp::new(
                200,
                r#"{"id":"job-1","tool":"workspace.read","arguments":{"path":"hello.txt"}}"#,
            )
        } else {
            Resp::new(204, "")
        }
    });
    let mut r = runner_for(&srv.url, ws.path(), &[]);
    assert_eq!(
        r.step(),
        Step::Ran {
            id: "job-1".into(),
            ok: true,
            reported: true
        }
    );
    let reqs = srv.requests();
    assert_eq!(reqs[1].path, "/v1/runner/jobs/job-1/result");
    assert_eq!(reqs[1].headers["authorization"], "Bearer test-token");
    let v: Value = serde_json::from_str(&reqs[1].body).unwrap();
    assert_eq!(v, serde_json::json!({"ok": true, "output": "hi"}));
}

#[test]
fn unknown_tool_and_traversal_report_failure() {
    let ws = tempfile::tempdir().unwrap();
    let srv = MockServer::start(|req, n| {
        if req.path == "/v1/runner/claim" {
            if n == 0 {
                Resp::new(
                    200,
                    r#"{"id":"a","tool":"shell.exec","arguments":{"cmd":"id"}}"#,
                )
            } else {
                Resp::new(
                    200,
                    r#"{"id":"b","tool":"workspace.read","arguments":{"path":"../../etc/passwd"}}"#,
                )
            }
        } else {
            Resp::new(200, "")
        }
    });
    let mut r = runner_for(&srv.url, ws.path(), &[]);
    assert_eq!(
        r.step(),
        Step::Ran {
            id: "a".into(),
            ok: false,
            reported: true
        }
    );
    assert_eq!(
        r.step(),
        Step::Ran {
            id: "b".into(),
            ok: false,
            reported: true
        }
    );
    let reqs = srv.requests();
    let a: Value = serde_json::from_str(&reqs[1].body).unwrap();
    assert!(a["output"].as_str().unwrap().contains("unknown tool"));
    let b: Value = serde_json::from_str(&reqs[3].body).unwrap();
    assert!(
        b["output"]
            .as_str()
            .unwrap()
            .contains("escapes the workspace"),
        "{b}"
    );
}

#[test]
fn result_retried_without_reexecuting_tool() {
    let ws = tempfile::tempdir().unwrap();
    let counter = ws.path().join("count");
    // The check appends a line per execution, outside the workspace contract
    // but inside the temp dir so we can count runs.
    let argv = format!(r#"["/bin/sh","-c","echo run >> {}"]"#, counter.display());
    let srv = MockServer::start(|req, n| {
        if req.path == "/v1/runner/claim" {
            Resp::new(
                200,
                r#"{"id":"c1","tool":"workspace.check","arguments":{}}"#,
            )
        } else if n < 3 {
            Resp::new(503, "busy")
        } else {
            Resp::new(200, "{}")
        }
    });
    let mut r = runner_for(&srv.url, ws.path(), &[("METRO_CHECK_ARGV", &argv)]);
    assert_eq!(
        r.step(),
        Step::Ran {
            id: "c1".into(),
            ok: true,
            reported: true
        }
    );
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap(),
        "run\n",
        "tool must run exactly once"
    );
    let posts = srv
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/result"))
        .count();
    assert_eq!(posts, 3);
}

#[test]
fn non_retryable_result_error_gives_up_immediately() {
    let ws = tempfile::tempdir().unwrap();
    let srv = MockServer::start(|req, _| {
        if req.path == "/v1/runner/claim" {
            Resp::new(200, r#"{"id":"d1","tool":"workspace.list","arguments":{}}"#)
        } else {
            Resp::new(409, "already finished")
        }
    });
    let mut r = runner_for(&srv.url, ws.path(), &[]);
    assert_eq!(
        r.step(),
        Step::Ran {
            id: "d1".into(),
            ok: true,
            reported: false
        }
    );
    assert_eq!(srv.requests().len(), 2);
}

#[test]
fn redirects_are_not_followed() {
    let ws = tempfile::tempdir().unwrap();
    let other = MockServer::start(|_, _| Resp::new(204, ""));
    let loc = format!("{}/v1/runner/claim", other.url);
    let srv = MockServer::start(move |_, _| {
        let mut r = Resp::new(307, "");
        r.headers.push(("Location".into(), loc.clone()));
        r
    });
    let mut r = runner_for(&srv.url, ws.path(), &[]);
    assert_eq!(r.step(), Step::ClaimError);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        other.requests().is_empty(),
        "token must not be sent to the redirect target"
    );
}

#[test]
fn server_errors_and_bad_bodies_are_claim_errors() {
    let ws = tempfile::tempdir().unwrap();
    let srv = MockServer::start(|_, n| match n {
        0 => Resp::new(500, "oops"),
        1 => Resp::new(401, "no"),
        _ => Resp::new(200, "not json"),
    });
    let mut r = runner_for(&srv.url, ws.path(), &[]);
    assert_eq!(r.step(), Step::ClaimError);
    assert_eq!(r.step(), Step::ClaimError);
    assert_eq!(r.step(), Step::ClaimError);
}

#[test]
fn invalid_job_shape_is_reported() {
    let ws = tempfile::tempdir().unwrap();
    let srv = MockServer::start(|req, _| {
        if req.path == "/v1/runner/claim" {
            Resp::new(
                200,
                r#"{"id":"e1","tool":"workspace.list","arguments":"nope"}"#,
            )
        } else {
            Resp::new(204, "")
        }
    });
    let mut r = runner_for(&srv.url, ws.path(), &[]);
    assert_eq!(
        r.step(),
        Step::Ran {
            id: "e1".into(),
            ok: false,
            reported: true
        }
    );
}

#[test]
fn unreachable_server_is_claim_error() {
    let ws = tempfile::tempdir().unwrap();
    // Bind then drop to get a closed port.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut r = runner_for(&format!("http://127.0.0.1:{port}"), ws.path(), &[]);
    assert_eq!(r.step(), Step::ClaimError);
}
