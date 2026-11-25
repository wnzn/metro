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
) -> Result<(String, serde_json::Value), Failure> {
    let url = format!("{base}?tools=web_search_exa,web_fetch_exa");
    let t = target(c, base, &url)?;
    let result = mcp::call_tool(&t, c.sessions, tool, args)
        .await
        .map_err(|e| error(c, e))?;
    let result = super::checked(c, result)?;
    if result["isError"] == true {
        if result["_meta"][RATE_LIMITED_META] == true {
            return Err(Failure::of(Outcome::Limited));
        }
        return Err(Failure::of(Outcome::Unavailable));
    }
    let text = result_text(&result);
    // Text, structured output, or both; neither is unusable.
    if text.trim().is_empty() && !result["structuredContent"].is_object() {
        return Err(Failure::of(Outcome::BadResponse));
    }
    Ok((text, result))
}

/// Results from the tool result's `structuredContent` (MCP structured tool
/// output), when Exa sends it. Only those become `results`, which Metro may
/// later let the agent fetch. Exa's text output is never parsed into
/// results: excerpt text can imitate a result boundary (`Title:`/`URL:`), so
/// text-only answers go to Metro as untrusted text and grant no fetch
/// permission.
pub(crate) fn structured_results(result: &serde_json::Value) -> Vec<Hit> {
    result["structuredContent"]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| {
            let url = x["url"].as_str()?;
            let snippet = match &x["highlights"] {
                serde_json::Value::Array(a) => a
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => x["text"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(500)
                    .collect(),
            };
            Some(Hit {
                title: x["title"].as_str().unwrap_or_default().to_owned(),
                url: url.to_owned(),
                snippet,
                published: x["publishedDate"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

pub(crate) async fn search(c: &Call<'_>, base: &str, r: &SearchReq) -> Result<Answer, Failure> {
    let (text, result) = call(
        c,
        base,
        "web_search_exa",
        json!({"query": r.query, "numResults": r.max_results}),
    )
    .await?;
    let mut results = structured_results(&result);
    results.truncate(r.max_results as usize);
    Ok(Answer {
        results,
        text: (!text.trim().is_empty()).then_some(text),
        ..Answer::default()
    })
}

pub(crate) async fn fetch(c: &Call<'_>, base: &str, r: &FetchReq) -> Result<Answer, Failure> {
    let (text, _) = call(
        c,
        base,
        "web_fetch_exa",
        json!({"urls": r.urls, "maxCharacters": MAX_PAGE_BYTES}),
    )
    .await?;
    if text.trim().is_empty() {
        return Err(Failure::of(Outcome::BadResponse));
    }
    Ok(Answer {
        text: Some(text),
        ..Answer::default()
    })
}
