//! A stand-in for the packaged UI in native update tests only (feature
//! `e2e-test-release`; packaging/windows_update_e2e.py). It talks to the
//! launcher's bridge like the UI does, as told by its environment:
//!
//! - `METRO_UI_STUB_LOG`: append one JSON line per start (the update state
//!   and whether this start was a trial).
//! - `METRO_UI_STUB_FAIL_VERSION`: when this launcher is that version, write
//!   over the local database and exit 3 before the first "render".
//! - `METRO_UI_STUB_HOLD_VERSION` and `METRO_UI_STUB_HOLD_FILE`: when this
//!   launcher is that version, wait for the file before the first "render"
//!   (so a test can act while a trial runs).
//! - `METRO_UI_STUB_DRIVE_FROM`: when this launcher is that version and its
//!   last update attempt didn't fail, check, download, verify and apply the
//!   offered update (Settings > About & updates, without the clicks).
//! - `METRO_UI_STUB_STOP`: otherwise stay open until this file exists.
//!
//! It never prints the bridge token.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Bridge {
    host: String,
    auth: String,
}

impl Bridge {
    fn from_env() -> Option<Bridge> {
        let url = std::env::var("METRO_DESKTOP_BRIDGE_URL").ok()?;
        let host = url
            .strip_prefix("http://")?
            .trim_end_matches('/')
            .to_string();
        let token = std::env::var("METRO_DESKTOP_BRIDGE_TOKEN").ok()?;
        Some(Bridge {
            host,
            auth: format!("Bearer {token}"),
        })
    }

    fn call(&self, method: &str, path: &str, body: Option<Value>) -> Option<(u16, Value)> {
        let mut s = TcpStream::connect(&self.host).ok()?;
        s.set_read_timeout(Some(Duration::from_secs(120))).ok()?;
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: {}\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.host,
            self.auth,
            body.len()
        );
        if !body.is_empty() {
            req.push_str("Content-Type: application/json\r\n");
        }
        req.push_str("\r\n");
        req.push_str(&body);
        s.write_all(req.as_bytes()).ok()?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).ok()?;
        let text = String::from_utf8_lossy(&raw);
        let (head, rest) = text.split_once("\r\n\r\n")?;
        let status = head.split_whitespace().nth(1)?.parse().ok()?;
        Some((status, serde_json::from_str(rest).unwrap_or(Value::Null)))
    }

    fn state(&self) -> Value {
        self.call("GET", "/v1/update", None)
            .map(|(_, v)| v)
            .unwrap_or(Value::Null)
    }
}

fn log(entry: Value) {
    let Some(path) = std::env::var_os("METRO_UI_STUB_LOG") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        // One append write per line: two instances may log at once.
        let _ = f.write_all(format!("{entry}\n").as_bytes());
    }
}

fn until(deadline: Duration, mut probe: impl FnMut() -> Option<Value>) -> Option<Value> {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if let Some(v) = probe() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

fn drive(bridge: &Bridge) -> Result<(), String> {
    let (status, _) = bridge
        .call("POST", "/v1/update/check", Some(json!({})))
        .ok_or("check")?;
    if status != 200 {
        return Err(format!("check answered {status}"));
    }
    let state = bridge.state();
    let version = state["latest"]["version"]
        .as_str()
        .ok_or("no offer")?
        .to_string();
    let (status, _) = bridge
        .call(
            "POST",
            "/v1/update/download",
            Some(json!({ "version": version })),
        )
        .ok_or("download")?;
    if status != 200 {
        return Err(format!("download answered {status}"));
    }
    let ready = until(Duration::from_secs(300), || {
        let s = bridge.state();
        match s["phase"].as_str() {
            Some("ready") | Some("failed") | Some("idle") => Some(s),
            _ => None,
        }
    })
    .ok_or("download did not finish")?;
    if ready["phase"] != "ready" {
        return Err(format!("download ended in {}", ready["phase"]));
    }
    log(json!({"event": "ready", "version": version}));
    let (status, v) = bridge
        .call(
            "POST",
            "/v1/update/apply",
            Some(json!({ "version": version, "confirm_interrupt": true })),
        )
        .ok_or("apply")?;
    if status != 200 {
        return Err(format!("apply answered {status}: {v}"));
    }
    Ok(())
}

fn main() {
    let Some(bridge) = Bridge::from_env() else {
        std::process::exit(2);
    };
    let state = bridge.state();
    let version = state["current"]["version"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    if env("METRO_UI_STUB_FAIL_VERSION").as_deref() == Some(version.as_str()) {
        if let Some(home) = env("METRO_DESKTOP_HOME") {
            let db = PathBuf::from(home).join("local/db/metro.sqlite3");
            let _ = std::fs::write(db, b"written by a broken version");
        }
        log(json!({"event": "fail", "version": version}));
        std::process::exit(3);
    }
    if env("METRO_UI_STUB_HOLD_VERSION").as_deref() == Some(version.as_str())
        && let Some(hold) = env("METRO_UI_STUB_HOLD_FILE").map(PathBuf::from)
    {
        log(json!({"event": "holding", "version": version}));
        let _ = until(Duration::from_secs(300), || {
            hold.exists().then_some(Value::Null)
        });
    }
    let trial = bridge
        .call("POST", "/v1/update/started", Some(json!({})))
        .is_some_and(|(_, v)| v["trial"] == true);
    log(json!({"event": "start", "version": version, "trial": trial, "state": state}));
    // Not again after a failed attempt (the rolled-back version starts again).
    let failed_before = matches!(
        state["last_update"]["outcome"].as_str(),
        Some("rolled_back" | "install_failed" | "not_confirmed")
    );
    if env("METRO_UI_STUB_DRIVE_FROM").as_deref() == Some(version.as_str())
        && !failed_before
        && let Err(e) = drive(&bridge)
    {
        log(
            json!({"event": "drive_failed", "version": version, "error": e, "state": bridge.state()}),
        );
    }
    let stop = env("METRO_UI_STUB_STOP").map(PathBuf::from);
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(600) {
        if stop.as_ref().is_some_and(|p| p.exists()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
