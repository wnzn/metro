//! Minimal stdio MCP client (JSON-RPC 2.0, newline-delimited).
//!
//! One server process per job: `initialize` -> `notifications/initialized`
//! -> `tools/list` (paginated) or `tools/call` -> close stdin -> reap/kill.
//! Server-to-client requests are answered: `ping` with `{}`, everything else
//! (sampling, roots, elicitation) with JSON-RPC "method not found", since the
//! runner advertises no client capabilities.

use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::config::Sandbox;
use crate::exec::{self, Interactive, LineRecv};
use std::path::Path;

pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_LINE: usize = 8 * 1024 * 1024;
const MAX_PAGES: usize = 50;
const MAX_TOOLS: usize = 2000;
/// Unrelated messages tolerated while waiting for one response.
const MAX_INTERLEAVED: usize = 10_000;

pub struct McpSession {
    proc: Option<Interactive>,
    next_id: u64,
    deadline: Instant,
    pub server_info: Value,
    pub protocol_version: String,
}

impl McpSession {
    /// Spawn the server and perform the initialize handshake.
    pub fn connect(
        argv: &[String],
        workspace: &Path,
        sandbox: &Sandbox,
        timeout: Duration,
    ) -> Result<Self, String> {
        let deadline = Instant::now() + timeout;
        let prepared = exec::prepare(argv, workspace, sandbox, true).map_err(|e| e.to_string())?;
        let proc = Interactive::spawn(prepared, MAX_LINE).map_err(|e| e.to_string())?;
        let mut s = McpSession {
            proc: Some(proc),
            next_id: 1,
            deadline,
            server_info: Value::Null,
            protocol_version: String::new(),
        };
        let init = s.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "metro-runner", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        let v = init
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !SUPPORTED_VERSIONS.contains(&v) {
            return Err(s.fail(format!(
                "server negotiated unsupported MCP protocol version {v:?}"
            )));
        }
        if init
            .get("capabilities")
            .and_then(|c| c.get("tools"))
            .is_none()
        {
            return Err(s.fail("server does not advertise the 'tools' capability".into()));
        }
        s.protocol_version = v.to_string();
        s.server_info = init.get("serverInfo").cloned().unwrap_or(Value::Null);
        s.notify("notifications/initialized", None)?;
        Ok(s)
    }

    fn proc(&mut self) -> &mut Interactive {
        self.proc.as_mut().expect("session open")
    }

    /// Attach stderr from the server to an error message.
    fn fail(&mut self, msg: String) -> String {
        let tail = match self.proc.take() {
            Some(p) => p.shutdown(Duration::from_millis(200)),
            None => String::new(),
        };
        let tail = tail.trim();
        if tail.is_empty() {
            msg
        } else {
            let tail = crate::util::truncate_utf8(tail.to_string(), 4096);
            format!("{msg}\n--- server stderr ---\n{tail}")
        }
    }

    fn send(&mut self, v: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(v).map_err(|e| e.to_string())?;
        let r = self.proc().send_line(bytes);
        r.map_err(|e| self.fail(format!("MCP server: {e}")))
    }

    fn notify(&mut self, method: &str, params: Option<Value>) -> Result<(), String> {
        let mut m = json!({"jsonrpc": "2.0", "method": method});
        if let Some(p) = params {
            m["params"] = p;
        }
        self.send(&m)
    }

    pub fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        for _ in 0..MAX_INTERLEAVED {
            let deadline = self.deadline;
            let line = match self.proc().recv_line(deadline) {
                LineRecv::Line(l) => l,
                LineRecv::Eof => {
                    return Err(self.fail(format!("MCP server exited before answering {method}")));
                }
                LineRecv::TimedOut => {
                    return Err(self.fail(format!("MCP server timed out during {method}")));
                }
                LineRecv::Shutdown => return Err(self.fail("runner is shutting down".into())),
                LineRecv::Err(e) => return Err(self.fail(format!("MCP server output error: {e}"))),
            };
            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => return Err(self.fail(format!("MCP server sent invalid JSON: {e}"))),
            };
            // Batches are not used by this client; servers must not send them unprompted.
            let Some(obj) = msg.as_object() else {
                return Err(self.fail("MCP server sent a non-object message".into()));
            };
            if let Some(m) = obj.get("method").and_then(Value::as_str) {
                if let Some(req_id) = obj.get("id") {
                    let reply = if m == "ping" {
                        json!({"jsonrpc": "2.0", "id": req_id, "result": {}})
                    } else {
                        json!({"jsonrpc": "2.0", "id": req_id,
                               "error": {"code": -32601, "message": "method not supported by metro-runner"}})
                    };
                    self.send(&reply)?;
                }
                // Notifications (progress, logging, list_changed) are ignored.
                continue;
            }
            if obj.get("id").and_then(Value::as_u64) != Some(id) {
                continue; // stale or unrelated response
            }
            if let Some(err) = obj.get("error") {
                let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("(no message)");
                return Err(format!("MCP {method} failed: [{code}] {message}"));
            }
            return match obj.get("result") {
                Some(r) => Ok(r.clone()),
                None => Err(self.fail(format!(
                    "MCP response to {method} has neither result nor error"
                ))),
            };
        }
        Err(self.fail(format!(
            "too many unrelated MCP messages while waiting for {method}"
        )))
    }

    pub fn list_tools(&mut self) -> Result<Vec<Value>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let r = self.request("tools/list", params)?;
            let page = r
                .get("tools")
                .and_then(Value::as_array)
                .ok_or("tools/list result has no 'tools' array")?;
            for t in page {
                if tools.len() >= MAX_TOOLS {
                    return Ok(tools);
                }
                let mut o = Map::new();
                for k in ["name", "title", "description", "inputSchema"] {
                    if let Some(v) = t.get(k) {
                        o.insert(k.into(), v.clone());
                    }
                }
                tools.push(Value::Object(o));
            }
            match r.get("nextCursor").and_then(Value::as_str) {
                Some(c) if !c.is_empty() && Some(c) != cursor.as_deref() => {
                    cursor = Some(c.to_string())
                }
                _ => return Ok(tools),
            }
        }
        Ok(tools)
    }

    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
    }

    pub fn close(mut self) {
        if let Some(p) = self.proc.take() {
            let _ = p.shutdown(Duration::from_secs(2));
        }
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        if let Some(p) = self.proc.take() {
            let _ = p.shutdown(Duration::from_millis(500));
        }
    }
}

/// Render a `tools/call` result to (ok, text).
pub fn render_call_result(r: &Value) -> (bool, String) {
    let is_error = r.get("isError").and_then(Value::as_bool).unwrap_or(false);
    let mut parts = Vec::new();
    if let Some(content) = r.get("content").and_then(Value::as_array) {
        for c in content {
            match (
                c.get("type").and_then(Value::as_str),
                c.get("text").and_then(Value::as_str),
            ) {
                (Some("text"), Some(t)) => parts.push(t.to_string()),
                (Some(kind @ ("image" | "audio")), _) => {
                    let mime = c.get("mimeType").and_then(Value::as_str).unwrap_or("?");
                    let len = c
                        .get("data")
                        .and_then(Value::as_str)
                        .map(str::len)
                        .unwrap_or(0);
                    parts.push(format!(
                        "[{kind} content omitted: {mime}, {len} base64 chars]"
                    ));
                }
                _ => parts.push(c.to_string()),
            }
        }
    }
    if parts.is_empty()
        && let Some(s) = r.get("structuredContent")
    {
        parts.push(s.to_string());
    }
    (!is_error, parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_content() {
        let (ok, t) = render_call_result(
            &json!({"content":[{"type":"text","text":"a"},{"type":"image","data":"AAAA","mimeType":"image/png"}]}),
        );
        assert!(ok);
        assert_eq!(t, "a\n[image content omitted: image/png, 4 base64 chars]");
        let (ok, t) =
            render_call_result(&json!({"content":[], "structuredContent":{"x":1}, "isError":true}));
        assert!(!ok);
        assert_eq!(t, r#"{"x":1}"#);
    }
}
