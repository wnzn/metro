//! Bounded reader for a Responses SSE stream. Success means a
//! `response.completed` event. Everything else is a visible error: a failed
//! or incomplete response, an `error` event, a stream that ends early, or a
//! stream over the limits.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use tokio::time::Instant;

use super::http::{self, Category, categorize, clean, tagged};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub max_total: usize,
    pub max_event: usize,
    pub idle: Duration,
    pub overall: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_total: 64 << 20,
            max_event: 16 << 20,
            idle: Duration::from_secs(300),
            overall: Duration::from_secs(60 * 60),
        }
    }
}

/// Returns the `response` object from `response.completed`. If that object's
/// `output` is empty, it's filled from the `response.output_item.done`
/// events.
pub(crate) async fn collect(mut resp: reqwest::Response, limits: Limits) -> Result<Value> {
    let deadline = Instant::now() + limits.overall;
    let mut parser = Parser::new(limits);
    loop {
        let idle = Instant::now() + limits.idle;
        let chunk = tokio::time::timeout_at(deadline.min(idle), resp.chunk())
            .await
            .map_err(|_| {
                tagged(
                    anyhow!(
                        "Responses stream stalled or exceeded its time limit before response.completed"
                    ),
                    Some(Category::Unavailable),
                )
            })?
            .map_err(|e| {
                tagged(
                    anyhow!(
                        "Responses stream interrupted before response.completed ({})",
                        http::net(e)
                    ),
                    Some(Category::Unavailable),
                )
            })?;
        let Some(chunk) = chunk else {
            parser.finish()?;
            return Err(tagged(
                anyhow!("Responses stream ended without response.completed"),
                Some(Category::Unavailable),
            ));
        };
        if let Some(done) = parser.feed(&chunk)? {
            return Ok(done);
        }
    }
}

/// Incremental SSE parser. It's separate from I/O so it can be tested.
pub(crate) struct Parser {
    limits: Limits,
    total: usize,
    line: Vec<u8>,
    data: Vec<u8>,
    event: Option<String>,
    items: BTreeMap<u64, Value>,
}

impl Parser {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            total: 0,
            line: Vec::new(),
            data: Vec::new(),
            event: None,
            items: BTreeMap::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Option<Value>> {
        self.total += bytes.len();
        if self.total > self.limits.max_total {
            bail!("Responses stream exceeded {} bytes", self.limits.max_total);
        }
        for &b in bytes {
            if b == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(v) = self.line_done(&line)? {
                    return Ok(Some(v));
                }
            } else {
                self.line.push(b);
                if self.line.len() > self.limits.max_event {
                    bail!(
                        "Responses stream event exceeded {} bytes",
                        self.limits.max_event
                    );
                }
            }
        }
        Ok(None)
    }

    /// Flushes a final event that has no trailing blank line.
    pub fn finish(&mut self) -> Result<Option<Value>> {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            if let Some(v) = self.line_done(&line)? {
                return Ok(Some(v));
            }
        }
        self.dispatch()
    }

    fn line_done(&mut self, line: &[u8]) -> Result<Option<Value>> {
        if line.is_empty() {
            return self.dispatch();
        }
        if line[0] == b':' {
            return Ok(None);
        }
        let (field, value) = match line.iter().position(|&b| b == b':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(b" ").unwrap_or(v))
            }
            None => (line, &[][..]),
        };
        match field {
            b"data" => {
                if !self.data.is_empty() {
                    self.data.push(b'\n');
                }
                self.data.extend_from_slice(value);
                if self.data.len() > self.limits.max_event {
                    bail!(
                        "Responses stream event exceeded {} bytes",
                        self.limits.max_event
                    );
                }
            }
            b"event" => self.event = Some(String::from_utf8_lossy(value).into_owned()),
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Result<Option<Value>> {
        let data = std::mem::take(&mut self.data);
        let event_name = self.event.take();
        if data.is_empty() || data == b"[DONE]" {
            return Ok(None);
        }
        let v: Value = serde_json::from_slice(&data)
            .map_err(|_| anyhow!("Responses stream sent a malformed event"))?;
        let kind = v
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(event_name)
            .unwrap_or_default();
        match kind.as_str() {
            "response.output_item.done" => {
                if let (Some(i), Some(item)) =
                    (v.get("output_index").and_then(Value::as_u64), v.get("item"))
                {
                    self.items.insert(i, item.clone());
                }
                Ok(None)
            }
            "response.completed" => {
                let mut r = v
                    .get("response")
                    .cloned()
                    .ok_or_else(|| anyhow!("response.completed carried no response"))?;
                let empty = r
                    .get("output")
                    .and_then(Value::as_array)
                    .is_none_or(Vec::is_empty);
                if empty && !self.items.is_empty() {
                    r["output"] =
                        Value::Array(std::mem::take(&mut self.items).into_values().collect());
                }
                Ok(Some(r))
            }
            "response.failed" => {
                let e = v.pointer("/response/error").cloned().unwrap_or(Value::Null);
                Err(stream_error(
                    "response.failed",
                    &e,
                    v.pointer("/response/id"),
                ))
            }
            "response.incomplete" => {
                let reason = v
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str)
                    .map(|s| clean(s, 96))
                    .unwrap_or_else(|| "unknown".into());
                bail!(
                    "Responses returned response.incomplete (reason {reason}); no completion was produced"
                )
            }
            "error" => {
                let e = v.get("error").cloned().unwrap_or_else(|| v.clone());
                Err(stream_error("stream error", &e, None))
            }
            _ => Ok(None),
        }
    }
}

/// The detailed local error, tagged by its documented code. The status is
/// unknown because the stream already returned HTTP 200.
fn stream_error(what: &str, e: &Value, id: Option<&Value>) -> anyhow::Error {
    let code = e.get("code").and_then(Value::as_str).map(|s| clean(s, 96));
    let param = e
        .get("param")
        .and_then(Value::as_str)
        .map(|s| clean(s, 128));
    let message = e
        .get("message")
        .and_then(Value::as_str)
        .map(|s| clean(s, 300));
    let f = http::Failure {
        status: 200,
        code: code.clone(),
        param,
        message,
        request_id: id.and_then(Value::as_str).map(|s| clean(s, 128)),
    };
    // The stream already returned HTTP 200, so the status adds nothing; keep
    // the code, param, message and hint.
    let text = f
        .describe(&format!("Responses ({what})"))
        .replacen(": HTTP 200", "", 1);
    tagged(anyhow!(text), categorize(None, code.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> Limits {
        Limits {
            max_total: 256,
            max_event: 64,
            idle: Duration::from_secs(1),
            overall: Duration::from_secs(1),
        }
    }

    #[test]
    fn completed_across_chunks_with_crlf() {
        let mut p = Parser::new(Limits::default());
        let ev = "event: response.completed\r\ndata: {\"type\":\"response.completed\",\r\ndata: \"response\":{\"status\":\"completed\",\"output\":[]}}\r\n\r\n";
        let (a, b) = ev.split_at(17);
        assert!(p.feed(a.as_bytes()).unwrap().is_none());
        let r = p.feed(b.as_bytes()).unwrap().unwrap();
        assert_eq!(r["status"], "completed");
    }

    #[test]
    fn limits_are_enforced() {
        let mut p = Parser::new(small());
        let big = format!("data: {}\n", "x".repeat(100));
        assert!(
            p.feed(big.as_bytes())
                .unwrap_err()
                .to_string()
                .contains("event exceeded")
        );
        let mut p = Parser::new(small());
        let e = (0..10)
            .map(|_| ": keepalive comment padding\n")
            .collect::<String>();
        assert!(
            p.feed(e.as_bytes())
                .unwrap_err()
                .to_string()
                .contains("exceeded 256 bytes")
        );
    }

    #[test]
    fn error_and_incomplete_events_fail() {
        let mut p = Parser::new(Limits::default());
        let e = p.feed(b"data: {\"type\":\"error\",\"error\":{\"code\":\"subscription_sharing_usage_unavailable\",\"message\":\"m\"}}\n\n").unwrap_err();
        assert!(format!("{e:#}").contains("subscription_sharing_usage_unavailable"));
        assert_eq!(e.downcast_ref::<Category>(), Some(&Category::Unavailable));
        let mut p = Parser::new(Limits::default());
        let e = p.feed(b"data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n").unwrap_err();
        assert!(e.to_string().contains("max_output_tokens"));
        let mut p = Parser::new(Limits::default());
        assert!(p.feed(b"data: not json\n\n").is_err());
    }
}
