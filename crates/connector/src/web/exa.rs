//! Exa through its hosted MCP server
//! (`https://mcp.exa.ai/mcp?tools=web_search_exa,web_fetch_exa`).
//!
//! Keyless access exists only there (at a free, unpublished rate limit) and
//! needs a **direct IPv4 connection**; the engine uses an IPv4-only client
//! without a proxy for it. A key goes to the same endpoint as `x-api-key`.
//! Rate limits: HTTP 429, or a tool result with `isError` and
//! `_meta["ai.exa/rateLimited"] = true`. A keyless 403 means the network
//! path isn't allowed (IPv6 or a proxy): blocked for an hour. 401/403 with a
//! key → key rejected. Sources: exa.ai/docs/reference/exa-mcp and
//! github.com/exa-labs/exa-mcp-server (webSearch.ts, webFetch.ts,
//! errorHandler.ts).

use reqwest::header::HeaderMap;
use serde_json::json;

use super::mcp::{self, McpError, Target, auth_header, result_text};
use super::{Answer, Call, Failure, FetchReq, Hit, MAX_PAGE_BYTES, Outcome, SearchReq};

const RATE_LIMITED_META: &str = "ai.exa/rateLimited";

fn target<'a>(c: &Call<'a>, base: &'a str, url: &'a str) -> Result<Target<'a>, Failure> {
    let (auth, mode, fp) = match c.key {
        Some(k) => (
            auth_header("x-api-key", k.expose())?,
            "key",
            super::keys::fingerprint(k),
        ),
        None => (HeaderMap::new(), "keyless", String::new()),
    };
    let _ = base;
    Ok(Target {
        http: c.http,
        url,
        auth,
        cache_key: format!("exa:{mode}:{fp}"),
        timeout: c.timeout,
        now: c.now,
    })
}

fn error(c: &Call<'_>, e: McpError) -> Failure {
    match e {
        McpError::Status {
            code: 429,
            retry_after,
        } => Failure {
            outcome: Outcome::Limited,
            cooldown: retry_after,
        },
        McpError::Status {
            code: 401 | 403, ..
        } if c.key.is_some() => Failure::of(Outcome::KeyRejected),
        McpError::Status { code: 403, .. } => Failure::for_secs(Outcome::Blocked, 3600),
        McpError::Status { code: 402, .. } => Failure::of(Outcome::OutOfCredits),
        McpError::Status { .. } => Failure::of(Outcome::Unavailable),
        McpError::Transport(f) => f,
        McpError::Bad => Failure::of(Outcome::BadResponse),
    }
}

async fn call(
    c: &Call<'_>,
    base: &str,
    tool: &str,
    args: serde_json::Value,
) -> Result<String, Failure> {
    let url = format!("{base}?tools=web_search_exa,web_fetch_exa");
    let t = target(c, base, &url)?;
    let result = mcp::call_tool(&t, c.sessions, tool, args)
        .await
        .map_err(|e| error(c, e))?;
    if result["isError"] == true {
        if result["_meta"][RATE_LIMITED_META] == true {
            return Err(Failure::of(Outcome::Limited));
        }
        return Err(Failure::of(Outcome::Unavailable));
    }
    let text = result_text(&result);
    if text.trim().is_empty() {
        return Err(Failure::of(Outcome::BadResponse));
    }
    Ok(text)
}

/// Results parsed from Exa's `Title:` / `URL:` / `Published:` lines, best
/// effort (the text itself still goes to Metro).
pub(crate) fn parse_results(text: &str) -> Vec<Hit> {
    let mut out: Vec<Hit> = Vec::new();
    let mut cur: Option<Hit> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(t) = line.strip_prefix("Title:") {
            if let Some(h) = cur.take().filter(|h| !h.url.is_empty()) {
                out.push(h);
            }
            cur = Some(Hit {
                title: t.trim().to_owned(),
                ..Hit::default()
            });
        } else if let Some(u) = line.strip_prefix("URL:") {
            cur.get_or_insert_with(Hit::default).url = u.trim().to_owned();
        } else if let Some(p) = line.strip_prefix("Published:") {
            let p = p.trim();
            if !p.is_empty() && p != "N/A" {
                cur.get_or_insert_with(Hit::default).published = Some(p.to_owned());
            }
        } else if let Some(h) = cur.as_mut()
            && !h.url.is_empty()
            && h.snippet.len() < 500
            && !line.is_empty()
            && !line.starts_with("Highlights:")
            && !line.starts_with("Text:")
        {
            if !h.snippet.is_empty() {
                h.snippet.push(' ');
            }
            h.snippet.push_str(line);
        }
    }
    if let Some(h) = cur.filter(|h| !h.url.is_empty()) {
        out.push(h);
    }
    out
}

pub(crate) async fn search(c: &Call<'_>, base: &str, r: &SearchReq) -> Result<Answer, Failure> {
    let text = call(
        c,
        base,
        "web_search_exa",
        json!({"query": r.query, "numResults": r.max_results}),
    )
    .await?;
    let mut results = parse_results(&text);
    results.truncate(r.max_results as usize);
    Ok(Answer {
        results,
        text: Some(text),
        ..Answer::default()
    })
}

pub(crate) async fn fetch(c: &Call<'_>, base: &str, r: &FetchReq) -> Result<Answer, Failure> {
    let text = call(
        c,
        base,
        "web_fetch_exa",
        json!({"urls": r.urls, "maxCharacters": MAX_PAGE_BYTES}),
    )
    .await?;
    Ok(Answer {
        text: Some(text),
        ..Answer::default()
    })
}
