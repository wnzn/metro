//! Reusable (`metro_rt_v2_`) credentials: one runner, several Spaces, each in
//! its own `spaces/<space_id>/` root under METRO_WORKSPACE.

mod common;

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{MockServer, Resp, config};
use metro_runner::client::ServerClient;
use metro_runner::confine::Workspace;
use metro_runner::runner::{Runner, Step};
use metro_runner::spaces::SpaceRoots;
use metro_runner::tools::Registry;
use serde_json::{Value, json};

const V2: &str = "metro_rt_v2_test-credential";

fn v2_runner(server: &str, base: &Path, extra: &[(&str, &str)]) -> Runner {
    let mut e: Vec<(&str, &str)> = vec![("METRO_RUNNER_TOKEN", V2)];
    e.extend_from_slice(extra);
    let cfg = config(base, &e);
    let client = ServerClient::with(server, cfg.token.expose(), Duration::from_secs(5));
    let roots = SpaceRoots::open(&cfg.workspace).unwrap();
    let mut r = Runner::per_space(
        client,
        Arc::new(cfg),
        Arc::new(roots),
        Duration::from_millis(10),
    );
    r.result_backoff =
        metro_runner::util::Backoff::new(Duration::from_millis(5), Duration::from_millis(20));
    r
}

/// Serves the given claim bodies in order (then 204), acks every result.
fn scripted(claims: Vec<String>) -> MockServer {
    let next = std::sync::Mutex::new(claims.into_iter());
    MockServer::start(move |req, _| {
        if req.path == "/v1/runner/claim" {
            match next.lock().unwrap().next() {
                Some(b) => Resp::new(200, &b),
                None => Resp::new(204, ""),
            }
        } else {
            Resp::new(200, "{}")
        }
    })
}

fn claim(id: &str, tool: &str, args: Value, space: Option<&str>) -> String {
    let mut v = json!({"id": id, "tool": tool, "arguments": args});
    if let Some(s) = space {
        v["space_id"] = json!(s);
    }
    v.to_string()
}

fn results(srv: &MockServer) -> Vec<Value> {
    srv.requests()
        .iter()
        .filter(|r| r.path.ends_with("/result"))
        .map(|r| serde_json::from_str(&r.body).unwrap())
        .collect()
}

/// base/root-secret, base/spaces/s-a/marker-a, base/spaces/s-b/marker-b
fn base() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("root-secret"), "ROOT").unwrap();
    for s in ["a", "b"] {
        let p = d.path().join("spaces").join(format!("s-{s}"));
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join(format!("marker-{s}")), format!("space {s}")).unwrap();
    }
    d
}

#[test]
fn independent_claims_see_only_their_space() {
    let d = base();
    let srv = scripted(vec![
        claim("j1", "workspace.list", json!({}), Some("s-a")),
        claim("j2", "workspace.list", json!({}), Some("s-b")),
        claim(
            "j3",
            "workspace.read",
            json!({"path": "marker-a"}),
            Some("s-a"),
        ),
        claim(
            "j4",
            "workspace.read",
            json!({"path": "marker-a"}),
            Some("s-b"),
        ),
        claim(
            "j5",
            "workspace.read",
            json!({"path": "../s-a/marker-a"}),
            Some("s-b"),
        ),
        claim("j6", "workspace.list", json!({"path": ".."}), Some("s-b")),
        // A brand-new Space gets an empty root, created on demand.
        claim("j7", "workspace.list", json!({}), Some("s-new")),
    ]);
    let mut r = v2_runner(&srv.url, d.path(), &[]);
    for _ in 0..7 {
        assert!(matches!(r.step(), Step::Ran { reported: true, .. }));
    }
    let res = results(&srv);
    let out = |i: usize| res[i]["output"].as_str().unwrap().to_string();
    assert!(res[0]["ok"].as_bool().unwrap());
    assert!(
        out(0).contains("marker-a") && !out(0).contains("marker-b"),
        "{}",
        out(0)
    );
    assert!(!out(0).contains("root-secret") && !out(0).contains("spaces"));
    assert!(
        out(1).contains("marker-b") && !out(1).contains("marker-a"),
        "{}",
        out(1)
    );
    assert_eq!(out(2), "space a");
    assert!(!res[3]["ok"].as_bool().unwrap(), "{}", out(3));
    assert!(
        !res[4]["ok"].as_bool().unwrap() && out(4).contains("escapes"),
        "{}",
        out(4)
    );
    assert!(!res[5]["ok"].as_bool().unwrap(), "{}", out(5));
    assert!(res[6]["ok"].as_bool().unwrap(), "{}", out(6));
    assert!(!out(6).contains("marker"), "{}", out(6));
    assert!(d.path().join("spaces/s-new").is_dir());

    // Protocol header on claims and results.
    for req in srv.requests() {
        assert_eq!(
            req.headers
                .get("x-metro-runner-protocol")
                .map(String::as_str),
            Some("2"),
            "{}",
            req.path
        );
        assert_eq!(req.headers["authorization"], format!("Bearer {V2}"));
    }
}

#[test]
fn missing_or_bad_space_id_is_refused_never_root() {
    let d = base();
    let mut claims = vec![claim("m0", "workspace.list", json!({}), None)];
    for (i, bad) in ["", "..", ".", "../s-a", "spaces/s-a", "S-A", "s-a/..", "/"]
        .iter()
        .enumerate()
    {
        claims.push(claim(
            &format!("m{}", i + 1),
            "workspace.list",
            json!({}),
            Some(bad),
        ));
    }
    claims.push(r#"{"id":"m90","tool":"workspace.list","arguments":{},"space_id":null}"#.into());
    claims.push(r#"{"id":"m91","tool":"workspace.list","arguments":{},"space_id":42}"#.into());
    let n = claims.len();
    let srv = scripted(claims);
    let mut r = v2_runner(&srv.url, d.path(), &[]);
    for _ in 0..n {
        assert!(matches!(
            r.step(),
            Step::Ran {
                ok: false,
                reported: true,
                ..
            }
        ));
    }
    for v in results(&srv) {
        let o = v["output"].as_str().unwrap();
        assert!(o.contains("refusing job"), "{o}");
        assert!(!o.contains("root-secret") && !o.contains("marker"), "{o}");
    }
    // Nothing was created for the bad ids.
    let mut names: Vec<_> = std::fs::read_dir(d.path().join("spaces"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, vec!["s-a", "s-b"]);
}

#[test]
fn symlinks_cannot_cross_spaces() {
    let d = base();
    let sp = d.path().join("spaces");
    symlink("../s-b/marker-b", sp.join("s-a/rel-to-b")).unwrap();
    symlink(sp.join("s-b/marker-b"), sp.join("s-a/abs-to-b")).unwrap();
    symlink("../s-b", sp.join("s-a/dir-to-b")).unwrap();
    symlink("../../root-secret", sp.join("s-a/to-root")).unwrap();
    // A whole Space aliased to another.
    symlink("s-b", sp.join("s-c")).unwrap();
    let srv = scripted(vec![
        claim(
            "y1",
            "workspace.read",
            json!({"path": "rel-to-b"}),
            Some("s-a"),
        ),
        claim(
            "y2",
            "workspace.read",
            json!({"path": "abs-to-b"}),
            Some("s-a"),
        ),
        claim(
            "y3",
            "workspace.list",
            json!({"path": "dir-to-b"}),
            Some("s-a"),
        ),
        claim(
            "y4",
            "workspace.read",
            json!({"path": "to-root"}),
            Some("s-a"),
        ),
        claim("y5", "workspace.list", json!({}), Some("s-c")),
        claim(
            "y6",
            "workspace.read",
            json!({"path": "marker-b"}),
            Some("s-c"),
        ),
    ]);
    let mut r = v2_runner(&srv.url, d.path(), &[]);
    for _ in 0..6 {
        assert!(matches!(
            r.step(),
            Step::Ran {
                ok: false,
                reported: true,
                ..
            }
        ));
    }
    for v in results(&srv) {
        let o = v["output"].as_str().unwrap();
        assert!(!o.contains("space b") && !o.contains("ROOT"), "{o}");
        assert!(!o.contains("marker-b\""), "{o}");
    }
    // A symlinked `spaces/` is refused at startup.
    let d2 = tempfile::tempdir().unwrap();
    std::fs::create_dir(d2.path().join("elsewhere")).unwrap();
    symlink("elsewhere", d2.path().join("spaces")).unwrap();
    assert!(SpaceRoots::open(&std::fs::canonicalize(d2.path()).unwrap()).is_err());
}

#[test]
fn host_subprocesses_disabled_for_reusable_credentials() {
    let d = base();
    let ran = tempfile::tempdir().unwrap();
    let marker = ran.path().join("ran");
    let argv =
        serde_json::to_string(&["/bin/sh", "-c", &format!("touch {}", marker.display())]).unwrap();
    let srv = scripted(vec![
        claim("h1", "workspace.check", json!({}), Some("s-a")),
        claim("h2", "workspace.mcp_list", json!({}), Some("s-a")),
        claim("h3", "workspace.mcp", json!({"name": "x"}), Some("s-a")),
        claim(
            "h4",
            "workspace.read",
            json!({"path": "marker-a"}),
            Some("s-a"),
        ),
    ]);
    let mut r = v2_runner(
        &srv.url,
        d.path(),
        &[("METRO_CHECK_ARGV", &argv), ("METRO_MCP_ARGV", &argv)],
    );
    for _ in 0..4 {
        r.step();
    }
    let res = results(&srv);
    for v in &res[..3] {
        assert!(!v["ok"].as_bool().unwrap());
        let o = v["output"].as_str().unwrap();
        assert!(
            o.contains("METRO_SANDBOX=docker") && o.contains("reusable"),
            "{o}"
        );
    }
    assert!(!marker.exists(), "host subprocess must not run");
    assert!(res[3]["ok"].as_bool().unwrap(), "list/read still work");
}

#[test]
fn dedicated_credential_keeps_legacy_root_semantics() {
    // A legacy token ignores space_id: tools see METRO_WORKSPACE itself.
    let d = base();
    let srv = scripted(vec![claim(
        "l1",
        "workspace.read",
        json!({"path": "root-secret"}),
        Some("s-a"),
    )]);
    let cfg = config(d.path(), &[]);
    let client = ServerClient::with(&srv.url, cfg.token.expose(), Duration::from_secs(5));
    let w = Workspace::open(&cfg.workspace).unwrap();
    let mut r = Runner::new(
        client,
        Registry::standard(Arc::new(cfg), Arc::new(w)),
        Duration::from_millis(10),
    );
    assert!(matches!(r.step(), Step::Ran { ok: true, .. }));
    assert_eq!(results(&srv)[0]["output"], "ROOT");
    assert_eq!(srv.requests()[0].headers["x-metro-runner-protocol"], "2");
}

/// With METRO_SANDBOX=docker the bind-mount source is the claiming Space's
/// root, never the base. Uses a fake docker CLI that records its argv, so
/// no daemon is needed.
#[test]
fn docker_mount_operand_is_the_selected_space() {
    let d = base();
    let bin = tempfile::tempdir().unwrap();
    let log = bin.path().join("argv.log");
    let fake = bin.path().join("docker");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\necho ---- >> '{}'\n",
            log.display(),
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let img = "img@sha256:0000000000000000000000000000000000000000000000000000000000000000";
    let srv = scripted(vec![
        claim("k1", "workspace.check", json!({}), Some("s-b")),
        claim("k2", "workspace.check", json!({}), Some("s-a")),
    ]);
    let mut r = v2_runner(
        &srv.url,
        d.path(),
        &[
            ("METRO_SANDBOX", "docker"),
            ("METRO_SANDBOX_IMAGE", img),
            ("METRO_DOCKER_BIN", fake.to_str().unwrap()),
            ("METRO_CHECK_ARGV", r#"["make","check"]"#),
        ],
    );
    r.step();
    r.step();
    let res = results(&srv);
    assert!(res.iter().all(|v| v["ok"] == true), "{res:?}");
    let base = std::fs::canonicalize(d.path()).unwrap();
    let logged = std::fs::read_to_string(&log).unwrap();
    let runs: Vec<&str> = logged.split("----\n").filter(|s| !s.is_empty()).collect();
    assert_eq!(runs.len(), 2, "{logged}");
    for (run, sid) in runs.iter().zip(["s-b", "s-a"]) {
        let want = format!(
            "type=bind,source={}/spaces/{sid},target=/workspace,readonly",
            base.display()
        );
        let mounts: Vec<&str> = run.lines().filter(|l| l.starts_with("type=bind")).collect();
        assert_eq!(mounts, vec![want.as_str()], "{run}");
        assert!(run.contains("--network=none") && !run.contains("docker.sock"));
    }
}
