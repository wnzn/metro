//! Firecrawl over REST: `POST /v2/search` and `POST /v2/scrape` (one URL
//! per request, in turn).
//!
//! Keyless: the `Authorization` header is omitted entirely. With a key:
//! `Authorization: Bearer`. Keyless caps are per IP and per day, so a
//! keyless 429 (or code `INDEX_DAILY_LIMIT`) cools down until `Retry-After`
//! or the next 00:00 UTC; 402 → out of credits until 00:00 UTC; 401 with a
//! key → key rejected. A page that can't be scraped is a per-URL result,
//! never a reason to rotate. Source: firecrawl docs (rate limits, search,
//! scrape) and the keyless launch post.

use serde_json::{Value, json};

use super::{
    Answer, Call, Failure, FetchReq, Hit, Outcome, Page, SearchReq, retry_after, str_of, transport,
    until_utc_midnight, vendor_json, vendor_ok,
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
        None => rb,
    }
}

/// A provider-level failure, or `None` for a per-URL one.
async fn failure(c: &Call<'_>, resp: reqwest::Response) -> Option<Failure> {
    let status = resp.status().as_u16();
    let hint = retry_after(resp.headers(), c.now);
    match status {
        429 => {
            let body = vendor_json(resp).await.unwrap_or(Value::Null);
            let daily = c.key.is_none() || body["code"] == "INDEX_DAILY_LIMIT";
            Some(Failure {
                outcome: Outcome::Limited,
                cooldown: hint.or(daily.then(|| until_utc_midnight(c.now))),
            })
        }
        402 => Some(Failure::for_secs(
            Outcome::OutOfCredits,
            until_utc_midnight(c.now),
        )),
        401 | 403 if c.key.is_some() => Some(Failure::of(Outcome::KeyRejected)),
        401 | 403 => Some(Failure::of(Outcome::Blocked)),
        _ => None,
    }
}

pub(crate) async fn search(c: &Call<'_>, base: &str, r: &SearchReq) -> Result<Answer, Failure> {
    let resp = request(
        c,
        format!("{base}/search"),
        json!({"query": r.query, "limit": r.max_results}),
    )
    .send()
    .await
    .map_err(|e| transport(&e))?;
    if !resp.status().is_success() {
        return Err(failure(c, resp)
            .await
            .unwrap_or(Failure::of(Outcome::Unavailable)));
    }
    let v = vendor_ok(c, resp).await?;
    let list = v["data"]["web"]
        .as_array()
        .or(v["data"].as_array())
        .ok_or(Failure::of(Outcome::BadResponse))?;
    Ok(Answer {
        results: list
            .iter()
            .filter_map(|x| {
                Some(Hit {
                    title: str_of(x, &["title"]).unwrap_or_default().to_owned(),
                    url: str_of(x, &["url"])?.to_owned(),
                    snippet: str_of(x, &["description", "snippet"])
                        .unwrap_or_default()
                        .to_owned(),
                    published: None,
                })
            })
            .collect(),
        ..Answer::default()
    })
}

pub(crate) async fn fetch(c: &Call<'_>, base: &str, r: &FetchReq) -> Result<Answer, Failure> {
    let mut out = Answer::default();
    for url in &r.urls {
        let resp = request(
            c,
            format!("{base}/scrape"),
            json!({"url": url, "formats": ["markdown"], "onlyMainContent": true, "timeout": 45000}),
        )
        .send()
        .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) if out.pages.is_empty() && out.failed_urls.is_empty() => {
                return Err(transport(&e));
            }
            Err(e) => {
                out.failed_urls.push((
                    url.clone(),
                    if e.is_timeout() { "timeout" } else { "failed" },
                ));
                continue;
            }
        };
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            if let Some(f) = failure(c, resp).await {
                return Err(f);
            }
            out.failed_urls.push((
                url.clone(),
                match status {
                    404 => "page_not_found",
                    408 => "timeout",
                    _ => "failed",
                },
            ));
            continue;
        }
        let v = vendor_ok(c, resp).await?;
        let data = &v["data"];
        let code = data["metadata"]["statusCode"].as_u64().unwrap_or(200);
        if code == 404 {
            out.failed_urls.push((url.clone(), "page_not_found"));
            continue;
        }
        if code >= 400 {
            out.failed_urls.push((url.clone(), "blocked"));
            continue;
        }
        let Some(text) = str_of(data, &["markdown"]) else {
            out.failed_urls.push((url.clone(), "failed"));
            continue;
        };
        out.pages.push(Page {
            url: url.clone(),
            final_url: str_of(&data["metadata"], &["url", "sourceURL"]).map(str::to_owned),
            title: str_of(&data["metadata"], &["title"]).map(str::to_owned),
            text: text.to_owned(),
            truncated: false,
        });
    }
    Ok(out)
}
