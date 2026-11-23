//! Loopback listener on 127.0.0.1. It serves two routes:
//!
//! - `GET /auth/start?k=<key>` answers with a 302 to the authorization URL.
//!   That keeps the URL, and the `id_token_hint` in it, out of the browser
//!   process's argv. The key is random per attempt; the link works only while
//!   this attempt waits and at most `MAX_STARTS` times, so opening it again
//!   (a browser that didn't come up, a copied link) continues the same
//!   attempt: same state, nonce and PKCE.
//! - `GET /auth/callback?...` checks `state` and returns the callback
//!   parameters.
//!
//! Requests must name `Host: 127.0.0.1:<port>`, which guards against DNS
//! rebinding. A callback with the wrong state gets a 400, and the listener
//! keeps waiting. Only the matching callback ends the wait.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;

const MAX_REQUEST: usize = 16 * 1024;
const CONN_TIMEOUT: Duration = Duration::from_secs(10);
/// How many times one attempt's start link may be opened.
const MAX_STARTS: u32 = 5;

pub(crate) struct Loopback {
    listener: TcpListener,
    port: u16,
}

#[derive(Debug, Default)]
pub(crate) struct Callback {
    pub code: Option<String>,
    pub client_id: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

impl Loopback {
    pub async fn bind(preferred: u16) -> Result<Self> {
        let listener = match TcpListener::bind(("127.0.0.1", preferred)).await {
            Ok(l) => l,
            Err(_) => TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| anyhow!("cannot listen on 127.0.0.1 for the sign-in callback: {e}"))?,
        };
        let port = listener.local_addr()?.port();
        Ok(Self { listener, port })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/auth/callback", self.port)
    }

    pub fn start_url(&self, key: &str) -> String {
        format!("http://127.0.0.1:{}/auth/start?k={key}", self.port)
    }

    /// Serves until a callback with matching `state` arrives, or the deadline
    /// passes.
    pub async fn wait(
        self,
        start_key: &str,
        authorize_url: &str,
        state: &str,
        timeout: Duration,
    ) -> Result<Callback> {
        let deadline = Instant::now() + timeout;
        let host = format!("127.0.0.1:{}", self.port);
        let mut starts = 0u32;
        loop {
            let accept = tokio::time::timeout_at(deadline, self.listener.accept()).await;
            let (mut stream, _) = match accept {
                Err(_) => {
                    bail!("timed out waiting for the browser to finish signing in with ChatGPT")
                }
                Ok(Err(_)) => continue,
                Ok(Ok(s)) => s,
            };
            let req = match tokio::time::timeout(CONN_TIMEOUT, read_request(&mut stream)).await {
                Ok(Ok(r)) => r,
                _ => {
                    let _ = respond(&mut stream, 400, "Bad request", None).await;
                    continue;
                }
            };
            if req.host.as_deref() != Some(host.as_str()) {
                let _ = respond(
                    &mut stream,
                    421,
                    "Use the address Metro opened (127.0.0.1).",
                    None,
                )
                .await;
                continue;
            }
            if req.method != "GET" {
                let _ = respond(&mut stream, 405, "Method not allowed", None).await;
                continue;
            }
            let Some(q) = parse_query(req.query.as_deref().unwrap_or("")) else {
                let _ = respond(&mut stream, 400, "Malformed query", None).await;
                continue;
            };
            match req.path.as_str() {
                "/auth/start" => {
                    let k = q.get("k").map(String::as_str).unwrap_or("");
                    if starts < MAX_STARTS && bool::from(k.as_bytes().ct_eq(start_key.as_bytes())) {
                        starts += 1;
                        let _ = respond(
                            &mut stream,
                            302,
                            "Continuing to ChatGPT…",
                            Some(authorize_url),
                        )
                        .await;
                    } else {
                        let _ = respond(&mut stream, 410, "This sign-in link is no longer valid. Start the sign-in again from Metro.", None).await;
                    }
                }
                "/auth/callback" => {
                    let got = q.get("state").map(String::as_str).unwrap_or("");
                    if !bool::from(got.as_bytes().ct_eq(state.as_bytes())) {
                        let _ = respond(
                            &mut stream,
                            400,
                            "This sign-in response does not match the attempt Metro started.",
                            None,
                        )
                        .await;
                        continue;
                    }
                    let cb = Callback {
                        code: q.get("code").cloned(),
                        client_id: q.get("client_id").cloned(),
                        error: q.get("error").cloned(),
                        error_description: q.get("error_description").cloned(),
                    };
                    let msg = if cb.error.is_some() {
                        "Sign-in with ChatGPT did not complete. You can close this tab and return to Metro."
                    } else {
                        "Metro received your ChatGPT sign-in. You can close this tab and return to Metro."
                    };
                    let _ = respond(&mut stream, 200, msg, None).await;
                    return Ok(cb);
                }
                _ => {
                    let _ = respond(&mut stream, 404, "Not found", None).await;
                }
            }
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: Option<String>,
    host: Option<String>,
}

async fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            bail!("closed");
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > MAX_REQUEST {
            bail!("request too large");
        }
    }
    let text = std::str::from_utf8(&buf).map_err(|_| anyhow!("not utf-8"))?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(target), Some(version)) = (first.next(), first.next(), first.next())
    else {
        bail!("bad request line");
    };
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        bail!("bad request line");
    }
    let mut host = None;
    for l in lines {
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':')
            && k.trim().eq_ignore_ascii_case("host")
        {
            if host.is_some() {
                bail!("duplicate host");
            }
            host = Some(v.trim().to_owned());
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_owned(), Some(q.to_owned())),
        None => (target.to_owned(), None),
    };
    Ok(Request {
        method: method.to_owned(),
        path,
        query,
        host,
    })
}

/// Parses form-style query pairs. A repeated key makes the whole query
/// invalid, so a parameter can't be smuggled in twice.
fn parse_query(q: &str) -> Option<HashMap<String, String>> {
    let mut m = HashMap::new();
    for (k, v) in url::form_urlencoded::parse(q.as_bytes()) {
        if k.len() > 64 || v.len() > 8192 {
            return None;
        }
        if m.insert(k.into_owned(), v.into_owned()).is_some() {
            return None;
        }
    }
    Some(m)
}

async fn respond(
    stream: &mut TcpStream,
    status: u16,
    message: &str,
    location: Option<&str>,
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        410 => "Gone",
        421 => "Misdirected Request",
        _ => "Error",
    };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Metro</title></head>\
         <body style=\"font-family:system-ui,sans-serif;margin:3rem\"><p>{}</p></body></html>",
        escape(message)
    );
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\
         X-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\
         Connection: close\r\n",
        body.len()
    );
    if let Some(loc) = location {
        if loc.contains('\r') || loc.contains('\n') {
            bail!("invalid redirect");
        }
        head += &format!("Location: {loc}\r\n");
    }
    head += "\r\n";
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
