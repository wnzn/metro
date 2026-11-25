//! Parallel through its hosted MCP server
//! (`https://mcp.parallel.ai/mcp?tools=web_search,web_fetch`).
//!
//! No key is needed for `web_search` and `web_fetch` in fast mode, at lower
//! rate limits; a key goes to the same endpoint as `Authorization: Bearer`.
//! `x-parallel-mcp-config` is never sent (without credentials it gets a
//! 401), and the URL has no trailing slash (404). 429 → limited; 402 →
//! insufficient credits (6 h); 401/403 with a key → key rejected. A tool
//! error without a machine-readable reason is "unavailable": prose is never
//! parsed. Sources: docs.parallel.ai/integrations/mcp (quickstart,
//! search-mcp).

use reqwest::header::HeaderMap;
use serde_json::{Value, json};

use super::mcp::{self, McpError, Target, auth_header, result_text};
use super::{Answer, Call, Failure, FetchReq, Hit, Outcome, SearchReq};

/// `objective` is the query; `search_queries` the keywords, or the query
/// cut to 200 characters.
pub(crate) fn search_arguments(r: &SearchReq) -> Value {
    let queries: Vec<String> = if r.keywords.is_empty() {
        vec![r.query.chars().take(200).collect()]
    } else {
        r.keywords.clone()
    };
    json!({"objective": r.query, "search_queries": queries})
}

fn error(c: &Call<'_>, e: McpError) -> Failure {
    match e {
        McpError::Status {
            code: 429,
            retry_after,
        } => Failure::with_cooldown(Outcome::Limited, retry_after),
        McpError::Status { code: 402, .. } => Failure::for_secs(Outcome::OutOfCredits, 6 * 3600),
        McpError::Status {
            code: 401 | 403, ..
        } if c.key.is_some() => Failure::of(Outcome::KeyRejected),
        McpError::Status { .. } => Failure::of(Outcome::Unavailable),
        McpError::Transport(f) => f,
        McpError::Bad => Failure::of(Outcome::BadResponse),
        McpError::Stopped(why) => Failure::Stopped(why),
    }
}

async fn call(
    c: &Call<'_>,
    base: &str,
    tool: &str,
    args: Value,
) -> Result<(String, Value), Failure> {
    let url = format!("{}?tools=web_search,web_fetch", base.trim_end_matches('/'));
    let (auth, mode, fp) = match c.key {
        Some(k) => (
            auth_header("authorization", &format!("Bearer {}", k.expose()))?,
            "key",
            super::keys::fingerprint(k),
        ),
        None => (HeaderMap::new(), "keyless", String::new()),
    };
    let t = Target {
        gate: c.gate,
        http: c.http,
        url: &url,
        auth,
        cache_key: format!("parallel:{mode}:{fp}"),
        timeout: c.timeout,
        now: c.now,
    };
    let result = mcp::call_tool(&t, c.sessions, tool, args)
        .await
        .map_err(|e| error(c, e))?;
    let result = super::checked(c, result)?;
    if result["isError"] == true {
        return Err(Failure::of(Outcome::Unavailable));
    }
    let text = result_text(&result);
    // Text, structured output, or both; neither is unusable.
    if text.trim().is_empty() && !result["structuredContent"].is_object() {
        return Err(Failure::of(Outcome::BadResponse));
    }
    Ok((text, result))
}

/// Results from structured data only: the tool result's
/// `structuredContent`, or a text part that is one whole JSON document
/// (`{results:[{url,title,excerpts|…}]}`). In both, a URL is its own JSON
/// field, so excerpt text can't create a result. Free text is never parsed
/// into results; it goes to Metro as untrusted text without fetch
/// permission.
pub(crate) fn structured_results(result: &Value, text: &str) -> Vec<Hit> {
    let parsed;
    let doc = if result["structuredContent"]["results"].is_array() {
        &result["structuredContent"]
    } else {
        match serde_json::from_str::<Value>(text.trim()) {
            Ok(v) if v.is_object() => {
                parsed = v;
                &parsed
            }
            _ => return Vec::new(),
        }
    };
    doc["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| {
            let url = x["url"].as_str()?;
            let snippet = match &x["excerpts"] {
                Value::Array(a) => a
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => x["snippet"].as_str().unwrap_or_default().to_owned(),
            };
            Some(Hit {
                title: x["title"].as_str().unwrap_or_default().to_owned(),
                url: url.to_owned(),
                snippet,
                published: x["publish_date"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

pub(crate) async fn search(c: &Call<'_>, base: &str, r: &SearchReq) -> Result<Answer, Failure> {
    let (text, result) = call(c, base, "web_search", search_arguments(r)).await?;
    // Without text, the structured output is the answer: it needs a
    // `results` array (`[]` is a real empty result).
    if text.trim().is_empty() && !result["structuredContent"]["results"].is_array() {
        return Err(Failure::of(Outcome::BadResponse));
    }
    let mut results = structured_results(&result, &text);
    results.truncate(r.max_results as usize);
    Ok(Answer {
        results,
        text: (!text.trim().is_empty()).then_some(text),
        ..Answer::default()
    })
}

pub(crate) async fn fetch(c: &Call<'_>, base: &str, r: &FetchReq) -> Result<Answer, Failure> {
    let mut args = json!({"urls": r.urls, "full_content": r.focus.is_none()});
    if let Some(f) = &r.focus {
        args["objective"] = json!(f);
    }
    let (text, _) = call(c, base, "web_fetch", args).await?;
    if text.trim().is_empty() {
        return Err(Failure::of(Outcome::BadResponse));
    }
    Ok(Answer {
        text: Some(text),
        ..Answer::default()
    })
}
