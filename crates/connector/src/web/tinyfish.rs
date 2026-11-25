//! TinyFish over REST. **Not keyless**: every request needs `X-API-Key`
//! from a free account, so without a saved key the provider is skipped and
//! nothing is sent (the engine never calls in here without a key).
//!
//! Search: `GET {search}?query=…`. Fetch: `POST {fetch}` with
//! `{urls, format:"markdown", per_url_timeout_ms}`; `errors[]` are per-URL
//! results. 429 with `details.reason = free_allowance_exhausted` → limited
//! until 00:00 UTC (a daily limit, not to be retried with short backoff);
//! other 429 → 60 s; 402 → out of credits until 00:00 UTC; 401 → key
//! rejected; 503 → unavailable. Source: docs.tinyfish.ai (search-api,
//! tinysearch and tinyfetch references).

use serde_json::{Value, json};

use super::mcp::auth_header;
use super::{
    Answer, Call, Failure, FetchReq, Hit, Outcome, Page, SearchReq, retry_after, str_of, transport,
    until_utc_midnight, vendor_json, vendor_ok,
};

async fn failure(c: &Call<'_>, resp: reqwest::Response) -> Failure {
    let status = resp.status().as_u16();
    let hint = retry_after(resp.headers(), c.now);
    let body = vendor_json(resp).await.unwrap_or(Value::Null);
    let reason = |v: &Value| {
        v["details"]["reason"]
            .as_str()
            .or(v["error"]["details"]["reason"].as_str())
            .map(str::to_owned)
    };
    match status {
        429 if reason(&body).as_deref() == Some("free_allowance_exhausted") => {
            Failure::for_secs(Outcome::Limited, until_utc_midnight(c.now))
        }
        429 => Failure::for_secs(Outcome::Limited, hint.unwrap_or(60)),
        402 => Failure::for_secs(Outcome::OutOfCredits, until_utc_midnight(c.now)),
        401 | 403 => Failure::of(Outcome::KeyRejected),
        _ => Failure::of(Outcome::Unavailable),
    }
}

fn key(c: &Call<'_>) -> Result<reqwest::header::HeaderMap, Failure> {
    let k = c.key.ok_or(Failure::of(Outcome::SkippedNoKey))?;
    auth_header("x-api-key", k.expose())
}

pub(crate) async fn search(c: &Call<'_>, base: &str, r: &SearchReq) -> Result<Answer, Failure> {
    let mut url = url::Url::parse(base).map_err(|_| Failure::of(Outcome::Unavailable))?;
    url.query_pairs_mut().append_pair("query", &r.query);
    let resp = c
        .http
        .get(url)
        .timeout(c.timeout)
        .headers(key(c)?)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| transport(&e))?;
    if !resp.status().is_success() {
        return Err(failure(c, resp).await);
    }
    let v = vendor_ok(c, resp).await?;
    let list = v["results"]
        .as_array()
        .ok_or(Failure::of(Outcome::BadResponse))?;
    Ok(Answer {
        results: list
            .iter()
            .filter_map(|x| {
                Some(Hit {
                    title: str_of(x, &["title"]).unwrap_or_default().to_owned(),
                    url: str_of(x, &["url"])?.to_owned(),
                    snippet: str_of(x, &["snippet", "description"])
                        .unwrap_or_default()
                        .to_owned(),
                    published: str_of(x, &["date", "published"]).map(str::to_owned),
                })
            })
            .collect(),
        ..Answer::default()
    })
}

pub(crate) async fn fetch(c: &Call<'_>, base: &str, r: &FetchReq) -> Result<Answer, Failure> {
    let resp = c
        .http
        .post(base)
        .timeout(c.timeout)
        .headers(key(c)?)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(
            json!({"urls": r.urls, "format": "markdown", "per_url_timeout_ms": 45000}).to_string(),
        )
        .send()
        .await
        .map_err(|e| transport(&e))?;
    if !resp.status().is_success() {
        return Err(failure(c, resp).await);
    }
    let v = vendor_ok(c, resp).await?;
    let list = v["results"]
        .as_array()
        .ok_or(Failure::of(Outcome::BadResponse))?;
    let mut out = Answer::default();
    for x in list {
        let Some(url) = str_of(x, &["url"]) else {
            continue;
        };
        out.pages.push(Page {
            url: url.to_owned(),
            final_url: str_of(x, &["final_url"]).map(str::to_owned),
            title: str_of(x, &["title"]).map(str::to_owned),
            text: str_of(x, &["text", "content", "markdown"])
                .unwrap_or_default()
                .to_owned(),
            truncated: false,
        });
    }
    for x in v["errors"].as_array().into_iter().flatten() {
        if let Some(url) = str_of(x, &["url"]) {
            let code = str_of(x, &["code", "error"]).unwrap_or_default();
            let reason = if code.contains("404") || code.eq_ignore_ascii_case("NOT_FOUND") {
                "page_not_found"
            } else if code.to_ascii_lowercase().contains("timeout") {
                "timeout"
            } else if code.to_ascii_lowercase().contains("block") {
                "blocked"
            } else {
                "failed"
            };
            out.failed_urls.push((url.to_owned(), reason));
        }
    }
    Ok(out)
}
