//! Bounded HTTP GET for discovery documents and JWKS.

use std::time::Duration;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Discovery documents and JWKS are small; anything bigger is refused.
pub(crate) const MAX_BODY_BYTES: usize = 256 * 1024;

pub(crate) fn client(https_only: bool) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        // Redirects are refused: the configured issuer and the jwks_uri it
        // advertises are the only places keys may come from.
        .redirect(reqwest::redirect::Policy::none())
        .https_only(https_only)
        .user_agent(concat!("metro-auth/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("building HTTP client: {e}"))
}

/// GET `url` and return the body, enforcing status 200 and a size cap.
pub(crate) async fn get_bounded(
    client: &reqwest::Client,
    url: &url::Url,
) -> Result<Vec<u8>, String> {
    let mut resp = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| format!("GET {url}: {}", error_chain(&e)))?;
    let status = resp.status();
    if status != reqwest::StatusCode::OK {
        return Err(format!("GET {url}: HTTP {status}"));
    }
    if resp
        .content_length()
        .is_some_and(|len| len > MAX_BODY_BYTES as u64)
    {
        return Err(format!(
            "GET {url}: body larger than {MAX_BODY_BYTES} bytes"
        ));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("GET {url}: {}", error_chain(&e)))?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(format!(
                "GET {url}: body larger than {MAX_BODY_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn error_chain(e: &reqwest::Error) -> String {
    let mut s = String::new();
    let mut src: Option<&dyn std::error::Error> = Some(e);
    // Walk the source chain for a useful message (e.g. TLS / DNS cause).
    let mut first = true;
    while let Some(err) = src {
        if !first {
            s.push_str(": ");
        }
        first = false;
        s.push_str(&err.to_string());
        src = err.source();
    }
    s
}
