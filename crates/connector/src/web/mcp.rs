//! A minimal MCP client over Streamable HTTP, for the hosted Exa and
//! Parallel search servers.
//!
//! `initialize` (protocol `2025-11-25`), keep `MCP-Session-Id` if the
//! server gives one, `notifications/initialized`, then `tools/call` with the
//! negotiated `MCP-Protocol-Version` and the session header. A reply is one
//! JSON body or an SSE stream read only until the response with our id
//! arrives. Sessions are cached per provider, mode and key fingerprint for
//! up to 10 minutes; a 404 on a session re-initializes once. Nothing else
//! of MCP is used: no `tools/list`, no sampling, and requests the server
//! sends us are ignored. Bodies are capped, redirects never followed.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

use super::{Failure, MAX_VENDOR_BYTES, Outcome, retry_after, transport};

pub const PROTOCOL_VERSION: &str = "2025-11-25";
const SESSION_TTL: Duration = Duration::from_secs(600);
const SESSION_HEADER: &str = "mcp-session-id";

#[derive(Clone)]
struct Session {
    id: Option<String>,
    version: String,
    at: Instant,
}

/// Cached MCP sessions, by `provider:mode:key-fingerprint`.
#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Session>>);

impl Sessions {
    fn get(&self, key: &str) -> Option<Session> {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        m.retain(|_, s| s.at.elapsed() < SESSION_TTL);
        m.get(key).cloned()
    }

    fn put(&self, key: &str, s: Session) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key.to_owned(), s);
    }

    fn drop_key(&self, key: &str) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).remove(key);
    }
}

/// How an MCP call failed.
#[derive(Debug)]
pub enum McpError {
    /// The HTTP status of a refused request, with `Retry-After`.
    Status { code: u16, retry_after: Option<u64> },
    /// Network trouble or a timeout.
    Transport(Failure),
    /// An unreadable reply or a JSON-RPC error.
    Bad,
}

/// One MCP request's context.
pub struct Target<'a> {
    pub http: &'a reqwest::Client,
    pub url: &'a str,
    /// Credential headers (already marked sensitive); empty when keyless.
    pub auth: HeaderMap,
    pub cache_key: String,
    pub timeout: Duration,
    pub now: u64,
}

struct Reply {
    status: u16,
    headers: HeaderMap,
    message: Option<Value>,
}

async fn post(
    t: &Target<'_>,
    session: Option<&Session>,
    body: &Value,
    want: Option<u64>,
) -> Result<Reply, McpError> {
    let mut rb = t
        .http
        .post(t.url)
        .timeout(t.timeout)
        .headers(t.auth.clone())
        .header(CONTENT_TYPE, "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(body.to_string());
    if let Some(s) = session {
        rb = rb.header("MCP-Protocol-Version", &s.version);
        if let Some(id) = &s.id {
            rb = rb.header(SESSION_HEADER, id);
        }
    }
    let mut resp = rb
        .send()
        .await
        .map_err(|e| McpError::Transport(transport(&e)))?;
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    if !(200..300).contains(&status) || want.is_none() {
        return Ok(Reply {
            status,
            headers,
            message: None,
        });
    }
    let want = want.unwrap_or_default();
    let sse = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));
    let message = if sse {
        read_sse(&mut resp, want).await?
    } else {
        let bytes = crate::http::read_capped(resp, MAX_VENDOR_BYTES)
            .await
            .map_err(|_| McpError::Bad)?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| McpError::Bad)?;
        // A batch is allowed by older servers: find ours.
        match v {
            Value::Array(list) => list.into_iter().find(|m| m["id"] == json!(want)),
            v => Some(v),
        }
    };
    Ok(Reply {
        status,
        headers,
        message,
    })
}

/// An incremental SSE splitter: bytes go in as they arrive, the `data` of
/// each complete event comes out. Every byte is scanned for a delimiter once
/// (a cursor remembers how far it got) and the consumed prefix is dropped
/// once per pushed chunk, so fragmented input and many small events both
/// cost linear work.
#[derive(Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
    /// Start of the event not yet emitted.
    consumed: usize,
    /// Where the delimiter search resumes.
    scanned: usize,
    /// Bytes inspected, for tests.
    pub(crate) work: usize,
}

impl SseParser {
    /// Add `chunk`; returns the `data` of each event it completed.
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut i = self.scanned.max(self.consumed);
        while i < self.buf.len() {
            self.work += 1;
            // An event ends at a blank line: "\n\n" or "\r\n\r\n" (or a
            // mix); look back from each '\n'.
            if self.buf[i] == b'\n' {
                let before = &self.buf[self.consumed..i];
                let blank = before.ends_with(b"\n") || before.ends_with(b"\n\r");
                if blank {
                    let event = &self.buf[self.consumed..i];
                    if let Some(d) = event_data(event) {
                        out.push(d);
                    }
                    self.consumed = i + 1;
                }
            }
            i += 1;
        }
        self.scanned = i;
        // Compact once per chunk.
        if self.consumed > 0 {
            self.buf.drain(..self.consumed);
            self.scanned -= self.consumed;
            self.consumed = 0;
        }
        out
    }
}

/// The joined `data:` lines of one event, if any.
fn event_data(event: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(event);
    let data: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|d| d.strip_prefix(' ').unwrap_or(d))
        .collect();
    (!data.is_empty()).then(|| data.join("\n"))
}

/// Read an event stream until the JSON-RPC response with id `want`.
async fn read_sse(resp: &mut reqwest::Response, want: u64) -> Result<Option<Value>, McpError> {
    let mut parser = SseParser::default();
    let mut read = 0usize;
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                read += chunk.len();
                if read > MAX_VENDOR_BYTES {
                    return Err(McpError::Bad);
                }
                for data in parser.push(&chunk) {
                    if let Ok(v) = serde_json::from_str::<Value>(&data)
                        && v["id"] == json!(want)
                        && (v.get("result").is_some() || v.get("error").is_some())
                    {
                        return Ok(Some(v));
                    }
                }
                // Let other tasks run between chunks of a busy stream.
                tokio::task::yield_now().await;
            }
            Ok(None) => return Ok(None),
            Err(e) => return Err(McpError::Transport(transport(&e))),
        }
    }
}

fn status_error(r: &Reply, now: u64) -> McpError {
    McpError::Status {
        code: r.status,
        retry_after: retry_after(&r.headers, now),
    }
}

async fn initialize(t: &Target<'_>) -> Result<Session, McpError> {
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "metro-connector", "version": env!("CARGO_PKG_VERSION")}
        }
    });
    let r = post(t, None, &init, Some(1)).await?;
    if !(200..300).contains(&r.status) {
        return Err(status_error(&r, t.now));
    }
    let msg = r.message.ok_or(McpError::Bad)?;
    if msg.get("error").is_some() || !msg["result"].is_object() {
        return Err(McpError::Bad);
    }
    let version = msg["result"]["protocolVersion"]
        .as_str()
        .filter(|v| v.len() <= 32 && v.bytes().all(|b| b.is_ascii_graphic()))
        .unwrap_or(PROTOCOL_VERSION)
        .to_owned();
    let id = r
        .headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 256 && v.bytes().all(|b| b.is_ascii_graphic()))
        .map(str::to_owned);
    let session = Session {
        id,
        version,
        at: Instant::now(),
    };
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let r = post(t, Some(&session), &note, None).await?;
    if matches!(r.status, 401 | 402 | 403 | 429) {
        return Err(status_error(&r, t.now));
    }
    Ok(session)
}

/// Call `tool` with `arguments`; returns the tool result object
/// (`{content, isError?, _meta?}`).
pub async fn call_tool(
    t: &Target<'_>,
    sessions: &Sessions,
    tool: &str,
    arguments: Value,
) -> Result<Value, McpError> {
    let body = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": tool, "arguments": arguments}
    });
    let mut retried = false;
    loop {
        let session = match sessions.get(&t.cache_key) {
            Some(s) => s,
            None => {
                let s = initialize(t).await?;
                sessions.put(&t.cache_key, s.clone());
                s
            }
        };
        let r = post(t, Some(&session), &body, Some(2)).await?;
        if r.status == 404 && session.id.is_some() && !retried {
            // The server forgot the session: start a new one, once.
            sessions.drop_key(&t.cache_key);
            retried = true;
            continue;
        }
        if !(200..300).contains(&r.status) {
            if matches!(r.status, 401 | 403 | 404) {
                sessions.drop_key(&t.cache_key);
            }
            return Err(status_error(&r, t.now));
        }
        let msg = r.message.ok_or(McpError::Bad)?;
        if msg.get("error").is_some() {
            return Err(McpError::Bad);
        }
        return msg
            .get("result")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or(McpError::Bad);
    }
}

/// The text parts of a tool result, joined.
pub fn result_text(result: &Value) -> String {
    result["content"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p["type"] == "text")
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

/// A credential header, marked sensitive so it never shows in debug output.
pub fn auth_header(name: &'static str, value: &str) -> Result<HeaderMap, Failure> {
    let mut v = HeaderValue::from_str(value).map_err(|_| Failure::of(Outcome::KeyRejected))?;
    v.set_sensitive(true);
    let mut h = HeaderMap::new();
    h.insert(HeaderName::from_static(name), v);
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_events_parse_with_linear_work() {
        let payload = "x".repeat(200_000);
        let stream = format!("event: message\r\ndata: {payload}\r\n\r\ndata: tail\n\n");
        let mut p = SseParser::default();
        let mut got = Vec::new();
        for b in stream.as_bytes().chunks(1) {
            got.extend(p.push(b));
        }
        assert_eq!(got, vec![payload, "tail".to_owned()]);
        assert!(
            p.work <= stream.len(),
            "work {} for {} bytes",
            p.work,
            stream.len()
        );
    }

    #[test]
    fn dense_small_events_parse_with_linear_work() {
        let n = 20_000;
        let stream: String = (0..n).map(|i| format!("data: {i}\n\n")).collect();
        let mut p = SseParser::default();
        let got = p.push(stream.as_bytes());
        assert_eq!(got.len(), n);
        assert_eq!(got[n - 1], (n - 1).to_string());
        assert!(p.work <= stream.len());
        // Split mid-delimiter and mid-line.
        let mut p = SseParser::default();
        let mut got = p.push(b"data: a\n");
        got.extend(p.push(b"\ndata: b\r\n\r"));
        got.extend(p.push(b"\n: comment\n\nevent: x\n\n"));
        assert_eq!(got, vec!["a", "b"]);
    }
}
