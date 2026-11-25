//! Tavily over REST: `POST /search` and `POST /extract`.
//!
//! Keyless: header `X-Tavily-Access-Mode: keyless` and **no**
//! `Authorization`. With a key: `Authorization: Bearer` and no keyless
//! header (never both, so the mode in activity is unambiguous).
//! 429 → limited (honouring `Retry-After`); 432/433 → out of credits or
//! plan limit (6 h); 434 → unavailable; 401/403 with a key → key rejected.
//! Source: docs.tavily.com (keyless; search endpoint error codes).

use serde_json::{Value, json};

use super::{
    Answer, Call, Failure, FetchReq, Hit, Outcome, Page, SearchReq, retry_after, str_of, transport,
    vendor_json,
};

fn request(c: &Call<'_>, url: String, body: Value) -> reqwest::RequestBuilder {
    let rb = c
        .http
        .post(url)
        .timeout(c.timeout)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(body.to_string());
    match c.key {
        Some(k) => rb.bearer_auth(k.expose()),
        None => rb.header("X-Tavily-Access-Mode", "keyless"),
    }
}

fn failure(c: &Call<'_>, status: u16, headers: &reqwest::header::HeaderMap) -> Failure {
    match status {
        429 => Failure {
            outcome: Outcome::Limited,
            cooldown: retry_after(headers, c.now),
        },
        432 | 433 => Failure::for_secs(Outcome::OutOfCredits, 6 * 3600),
        401 | 403 if c.key.is_some() => Failure::of(Outcome::KeyRejected),
        _ => Failure::of(Outcome::Unavailable),
    }
}

async fn send(c: &Call<'_>, url: String, body: Value) -> Result<Value, Failure> {
    let resp = request(c, url, body)
        .send()
        .await
        .map_err(|e| transport(&e))?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(failure(c, status, resp.headers()));
    }
    vendor_json(resp).await
}

pub(crate) async fn search(c: &Call<'_>, base: &str, r: &SearchReq) -> Result<Answer, Failure> {
    let v = send(
        c,
        format!("{base}/search"),
        json!({"query": r.query, "max_results": r.max_results, "search_depth": "basic"}),
    )
    .await?;
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
                    snippet: str_of(x, &["content"]).unwrap_or_default().to_owned(),
                    published: str_of(x, &["published_date"]).map(str::to_owned),
                })
            })
            .collect(),
        ..Answer::default()
    })
}

pub(crate) async fn fetch(c: &Call<'_>, base: &str, r: &FetchReq) -> Result<Answer, Failure> {
    let mut body = json!({"urls": r.urls, "format": "markdown"});
    if let Some(f) = &r.focus {
        body["query"] = json!(f);
    }
    let v = send(c, format!("{base}/extract"), body).await?;
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
            final_url: None,
            title: str_of(x, &["title"]).map(str::to_owned),
            text: str_of(x, &["raw_content", "content"])
                .unwrap_or_default()
                .to_owned(),
            truncated: false,
        });
    }
    for x in v["failed_results"].as_array().into_iter().flatten() {
        if let Some(url) = str_of(x, &["url"]) {
            out.failed_urls.push((url.to_owned(), "failed"));
        }
    }
    Ok(out)
}
