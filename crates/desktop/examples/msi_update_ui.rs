//! Stand-in for the NSH UI in the native MSI update tests
//! (`packaging/windows-msi/test-msi-update.ps1`); never shipped.
//!
//! The launcher starts it as `ui/Metro.exe` with the bridge URL and token.
//! Each start appends one JSON line to `$METRO_MSI_TEST_OUT/ui.jsonl` with
//! the version it reports and the bridge's `GET /v1/update`, then:
//!
//! - always sends `POST /v1/update/started` (what the real UI does after
//!   its first render when it can update in-app);
//! - with `$METRO_MSI_TEST_UPDATE_TO=<version>`, while the reported
//!   version isn't that one and the log holds fewer than
//!   `$METRO_MSI_TEST_ATTEMPTS` (default 1) presses: check, download, wait
//!   until ready, and `POST /v1/update/apply` (Restart now), then wait to
//!   be closed;
//! - otherwise exits 0.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

fn request(url: &str, token: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let host = url.trim_start_matches("http://").trim_end_matches('/');
    let Ok(mut s) = TcpStream::connect(host) else {
        return (0, String::new());
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(120)));
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if s.write_all(head.as_bytes()).is_err() {
        return (0, String::new());
    }
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string());
    (status, body.unwrap_or_default())
}

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\":\""))? + key.len() + 4;
    let rest = &json[at..];
    Some(&rest[..rest.find('"')?])
}

fn log(line: &str) {
    let Ok(dir) = std::env::var("METRO_MSI_TEST_OUT") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::path::Path::new(&dir).join("ui.jsonl"))
    {
        let _ = writeln!(f, "{line}");
    }
}

fn main() {
    let url = std::env::var("METRO_DESKTOP_BRIDGE_URL").unwrap_or_default();
    let token = std::env::var("METRO_DESKTOP_BRIDGE_TOKEN").unwrap_or_default();
    let call = |m: &str, p: &str, b: &str| request(&url, &token, m, p, b);
    let (_, state) = call("GET", "/v1/update", "");
    let current = state
        .find("\"current\":")
        .and_then(|i| field(&state[i..], "version"))
        .unwrap_or("")
        .to_string();
    log(&format!(
        "{{\"event\":\"start\",\"version\":\"{current}\",\"state\":{}}}",
        if state.starts_with('{') {
            &state
        } else {
            "null"
        }
    ));
    let (code, started) = call("POST", "/v1/update/started", "{}");
    log(&format!(
        "{{\"event\":\"started\",\"code\":{code},\"body\":{}}}",
        if started.starts_with('{') {
            &started
        } else {
            "null"
        }
    ));
    let target = std::env::var("METRO_MSI_TEST_UPDATE_TO").unwrap_or_default();
    // At most this many Restart now presses per log (a relaunched old
    // version doesn't try again on its own).
    let attempts: usize = std::env::var("METRO_MSI_TEST_ATTEMPTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let pressed = std::env::var("METRO_MSI_TEST_OUT")
        .ok()
        .and_then(|d| std::fs::read_to_string(std::path::Path::new(&d).join("ui.jsonl")).ok())
        .map_or(0, |log| log.matches("\"event\":\"apply\"").count());
    if target.is_empty() || target == current || pressed >= attempts {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(300);
    let (code, body) = call("POST", "/v1/update/check", "{}");
    log(&format!("{{\"event\":\"check\",\"code\":{code}}}"));
    let _ = body;
    let version = format!("{{\"version\":\"{target}\"}}");
    let (code, _) = call("POST", "/v1/update/download", &version);
    log(&format!("{{\"event\":\"download\",\"code\":{code}}}"));
    loop {
        let (_, s) = call("GET", "/v1/update", "");
        match field(&s, "phase") {
            Some("ready") => break,
            Some("failed") | None => {
                log(&format!("{{\"event\":\"download_failed\",\"state\":{s}}}"));
                std::process::exit(5);
            }
            _ if Instant::now() > deadline => std::process::exit(6),
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    }
    let apply = format!("{{\"version\":\"{target}\",\"confirm_interrupt\":true}}");
    let (code, body) = call("POST", "/v1/update/apply", &apply);
    log(&format!(
        "{{\"event\":\"apply\",\"code\":{code},\"body\":{}}}",
        if body.starts_with('{') { &body } else { "null" }
    ));
    // The launcher closes this window to restart.
    std::thread::sleep(Duration::from_secs(120));
}
