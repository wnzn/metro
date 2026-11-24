//! Bounded reader for a Responses SSE stream. Success means a
//! `response.completed` event. Everything else is a visible error: a failed
//! or incomplete response, an `error` event, a stream that ends early, or a
//! stream over the limits.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::Value;
use tokio::time::Instant;

use super::diagnostic::{Detail, Phase, mark};
use super::http::{self, Category, categorize, error_code, tagged};

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
/// events. `prefix` is what was already read from `resp` (see [`sniff`]).
pub(crate) async fn collect_after(
    mut resp: reqwest::Response,
    prefix: Vec<u8>,
    deadline: Instant,
    limits: Limits,
) -> Result<Value> {
    let mut parser = Parser::new(limits);
    if !prefix.is_empty()
        && let Some(done) = parser.feed(&prefix)?
    {
        return Ok(done);
    }
    loop {
        let chunk = next_chunk(&mut resp, deadline, limits).await?;
        let Some(chunk) = chunk else {
            if let Some(done) = parser.finish()? {
                return Ok(done);
            }
            return Err(tagged(
                anyhow!("Responses stream ended without response.completed"),
                Some(Category::Interrupted),
            ));
        };
        if let Some(done) = parser.feed(chunk.as_ref())? {
            return Ok(done);
        }
    }
}

async fn next_chunk(
    resp: &mut reqwest::Response,
    deadline: Instant,
    limits: Limits,
) -> Result<Option<impl AsRef<[u8]> + use<>>> {
    let idle = Instant::now() + limits.idle;
    tokio::time::timeout_at(deadline.min(idle), resp.chunk())
        .await
        .map_err(|_| {
            tagged(
                anyhow!(
                    "Responses stream stalled or exceeded its time limit before response.completed"
                ),
                Some(Category::Interrupted),
            )
        })?
        .map_err(|e| {
            tagged(
                anyhow!(
                    "Responses stream interrupted before response.completed ({})",
                    http::net(e)
                ),
                Some(Category::Interrupted),
            )
        })
}

/// What a success body without a recognized `content-type` looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sniffed {
    EventStream,
    Json,
    Unknown,
}

/// How many bytes [`sniff`] reads at most before deciding.
const SNIFF_MAX: usize = 4096;

/// Classifies the start of a body: SSE field or comment lines, or a JSON
/// object. Leading BOM and whitespace are ignored.
pub(crate) fn classify(prefix: &[u8]) -> Option<Sniffed> {
    let p = prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix);
    let start = p.iter().position(|b| !b.is_ascii_whitespace())?;
    let p = &p[start..];
    if p[0] == b'{' {
        return Some(Sniffed::Json);
    }
    if p[0] == b':' {
        return Some(Sniffed::EventStream);
    }
    if !p[0].is_ascii_lowercase() {
        return Some(Sniffed::Unknown);
    }
    // A field name ends at ':' or at the end of the line.
    let end = p
        .iter()
        .position(|&b| matches!(b, b':' | b'\r' | b'\n'))
        .or((p.len() >= 6).then_some(p.len()));
    let Some(end) = end else {
        return None; // Need more bytes to read a whole field name.
    };
    Some(match &p[..end] {
        b"event" | b"data" | b"id" | b"retry" => Sniffed::EventStream,
        _ => Sniffed::Unknown,
    })
}

/// Reads the start of `resp` (within the idle and overall limits) until
/// [`classify`] can decide, at most [`SNIFF_MAX`] bytes. Returns the bytes
/// read, which belong to the body.
pub(crate) async fn sniff(
    resp: &mut reqwest::Response,
    deadline: Instant,
    limits: Limits,
) -> Result<(Sniffed, Vec<u8>)> {
    let mut buf = Vec::new();
    loop {
        if let Some(kind) = classify(&buf) {
            return Ok((kind, buf));
        }
        if buf.len() >= SNIFF_MAX {
            return Ok((Sniffed::Unknown, buf));
        }
        match next_chunk(resp, deadline, limits).await? {
            Some(c) => buf.extend_from_slice(c.as_ref()),
            None => return Ok((classify(&buf).unwrap_or(Sniffed::Unknown), buf)),
        }
    }
}

/// Incremental SSE parser. It's separate from I/O so it can be tested.
pub(crate) struct Parser {
    limits: Limits,
    total: usize,
    line: Vec<u8>,
    after_cr: bool,
    first_line: bool,
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
            after_cr: false,
            first_line: true,
            data: Vec::new(),
            event: None,
            items: BTreeMap::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Option<Value>> {
        self.total += bytes.len();
        if self.total > self.limits.max_total {
            return Err(too_large());
        }
        for &b in bytes {
            if std::mem::replace(&mut self.after_cr, b == b'\r') && b == b'\n' {
                continue;
            }
            if matches!(b, b'\r' | b'\n') {
                let line = std::mem::take(&mut self.line);
                if let Some(v) = self.line_done(&line)? {
                    return Ok(Some(v));
                }
            } else {
                self.line.push(b);
                if self.line.len() > self.limits.max_event {
                    return Err(too_large());
                }
            }
        }
        Ok(None)
    }

    /// Accepts an EOF directly after a complete terminal JSON event even if
    /// the peer omitted the final delimiter. A delta or `[DONE]` isn't one.
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
        let line = if std::mem::take(&mut self.first_line) {
            line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line)
        } else {
            line
        };
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
                self.data.extend_from_slice(value);
                self.data.push(b'\n');
                if self.data.len() - 1 > self.limits.max_event {
                    return Err(too_large());
                }
            }
            b"event" => self.event = Some(String::from_utf8_lossy(value).into_owned()),
            _ => {}
        }
        Ok(None)
    }

    fn dispatch(&mut self) -> Result<Option<Value>> {
        let mut data = std::mem::take(&mut self.data);
        data.pop(); // SSE joins data lines with LF and removes the final LF.
        let event_name = self.event.take();
        if data.is_empty() || data == b"[DONE]" {
            return Ok(None);
        }
        let v: Value = serde_json::from_slice(&data)
            .map_err(|_| anyhow!("Responses stream sent a malformed event"))?;
        let kind = v
            .get("type")
            .and_then(Value::as_str)
            .or(event_name.as_deref())
            .unwrap_or_default();
        if kind == "error" || event_name.as_deref() == Some("error") {
            return Err(stream_error(
                v.get("error").filter(|e| e.is_object()).unwrap_or(&v),
            ));
        }
        match kind {
            "response.output_item.done" => {
                if let (Some(i), Some(item)) =
                    (v.get("output_index").and_then(Value::as_u64), v.get("item"))
                {
                    self.items.insert(i, item.clone());
                }
                Ok(None)
            }
            "response.completed" => {
                let r = v
                    .get("response")
                    .and_then(Value::as_object)
                    .cloned()
                    .ok_or_else(|| anyhow!("response.completed carried no response"))?;
                let mut r = Value::Object(r);
                let empty = r
                    .get("output")
                    .is_none_or(|v| v.is_null() || v.as_array().is_some_and(Vec::is_empty));
                if empty && !self.items.is_empty() {
                    r["output"] =
                        Value::Array(std::mem::take(&mut self.items).into_values().collect());
                }
                Ok(Some(r))
            }
            "response.failed" => Err(stream_error(
                v.pointer("/response/error").unwrap_or(&Value::Null),
            )),
            "response.incomplete" => {
                let category = match v
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str)
                {
                    Some("content_filter") => Category::ContentFilter,
                    _ => Category::Incomplete,
                };
                Err(tagged(
                    anyhow!("Responses stream reported an incomplete response"),
                    Some(category),
                ))
            }
            _ => Ok(None),
        }
    }
}

fn too_large() -> anyhow::Error {
    crate::SafeError::new(
        "The OpenAI response stream is too large for Metro. Ask for a shorter answer and try again.",
    )
    .into()
}

/// A mid-stream error has no separate HTTP status; inspect machine fields only.
fn stream_error(e: &Value) -> anyhow::Error {
    let code = error_code(e);
    let param = e.get("param").and_then(Value::as_str);
    let mut detail = Detail::phase(Phase::Stream);
    detail.code = super::diagnostic::provider_code(code);
    detail.param = super::diagnostic::param(param);
    mark(
        tagged(
            anyhow!("Responses stream reported a failure"),
            Some(categorize(None, code, param).unwrap_or(Category::Rejected)),
        ),
        detail,
    )
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
    fn accepts_bom_and_bare_cr() {
        let mut p = Parser::new(Limits::default());
        let ev = "\u{feff}: comment\revent: response.completed\rdata: {\"response\":{\"status\":\"completed\",\"output\":[]}}\r\r";
        let (a, b) = ev.as_bytes().split_at(2);
        assert!(p.feed(a).unwrap().is_none());
        assert_eq!(p.feed(b).unwrap().unwrap()["status"], "completed");
    }

    #[test]
    fn limits_are_enforced() {
        let mut p = Parser::new(small());
        let big = format!("data: {}\n", "x".repeat(100));
        assert!(
            p.feed(big.as_bytes())
                .unwrap_err()
                .downcast_ref::<crate::SafeError>()
                .is_some()
        );
        let mut p = Parser::new(small());
        let e = (0..10)
            .map(|_| ": keepalive comment padding\n")
            .collect::<String>();
        assert!(
            p.feed(e.as_bytes())
                .unwrap_err()
                .downcast_ref::<crate::SafeError>()
                .is_some()
        );
    }

    #[test]
    fn error_and_incomplete_events_fail() {
        let mut p = Parser::new(Limits::default());
        let e = p.feed(b"data: {\"type\":\"error\",\"error\":{\"code\":\"subscription_sharing_usage_unavailable\",\"message\":\"m\"}}\n\n").unwrap_err();
        assert_eq!(e.downcast_ref::<Category>(), Some(&Category::Unavailable));
        let mut p = Parser::new(Limits::default());
        let e = p.feed(b"data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n").unwrap_err();
        assert_eq!(e.downcast_ref::<Category>(), Some(&Category::Incomplete));
        let mut p = Parser::new(Limits::default());
        assert!(p.feed(b"data: not json\n\n").is_err());
    }

    #[test]
    fn sniffing_needs_a_whole_field_name() {
        assert_eq!(classify(b""), None);
        assert_eq!(classify(b"  \n"), None);
        assert_eq!(classify(b"\xef\xbb\xbf\n{\"a\":1}"), Some(Sniffed::Json));
        assert_eq!(classify(b": keepalive"), Some(Sniffed::EventStream));
        assert_eq!(classify(b"event: x"), Some(Sniffed::EventStream));
        assert_eq!(classify(b"data:{}"), Some(Sniffed::EventStream));
        assert_eq!(classify(b"retry: 1\n"), Some(Sniffed::EventStream));
        assert_eq!(classify(b"even"), None);
        assert_eq!(classify(b"<html>"), Some(Sniffed::Unknown));
        assert_eq!(classify(b"[1,2]"), Some(Sniffed::Unknown));
        assert_eq!(classify(b"datum: x"), Some(Sniffed::Unknown));
    }
}
