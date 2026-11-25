//! Anonymous downloads from GitHub, with fixed URLs and strict limits.
//!
//! - URLs are built only from constants and a parsed version or a file name
//!   from a manifest ([`Source`]).
//! - Redirects are followed by hand: at most 5, HTTPS only, to
//!   [`ALLOWED_HOSTS`] (checked against a real request on 2026-11-25:
//!   `github.com` → `release-assets.githubusercontent.com`; the older
//!   `objects.githubusercontent.com` is kept). Anything else fails closed
//!   with `redirect_refused`.
//! - Headers: `User-Agent: Metro`, `Accept`, and for the API
//!   `X-GitHub-Api-Version`. No cookies, auth, referrer, version or ids.
//! - Every body has a size cap; a larger one is refused before it's read.
//! - 403-with-exhausted-quota and 429 become `rate_limited` with the
//!   server's `retry-after` / `x-ratelimit-reset`.

use crate::error::{Error, ErrorCode};
use crate::version::Version;
use sha2::Digest;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

pub const USER_AGENT: &str = "Metro";
pub const ALLOWED_HOSTS: [&str; 3] = [
    "github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
];
const MAX_REDIRECTS: usize = 5;
const API_VERSION: &str = "2022-11-28";

/// Where releases come from. Production is always [`Source::github`].
#[derive(Clone, Debug)]
pub struct Source {
    base: String,
    api: String,
    hosts: Vec<String>,
    https_only: bool,
}

impl Source {
    pub fn github() -> Source {
        Source {
            base: crate::REPO_URL.to_string(),
            api: "https://api.github.com/repos/wnzn/metro".to_string(),
            hosts: ALLOWED_HOSTS.iter().map(|h| h.to_string()).collect(),
            https_only: true,
        }
    }

    /// A loopback fixture server (`http://127.0.0.1:<port>`), for tests.
    /// Refuses anything that isn't loopback, so it can't point a build at
    /// another host. Nothing reads it from the environment or config.
    #[doc(hidden)]
    pub fn loopback_fixture(base: &str) -> Option<Source> {
        let rest = base.strip_prefix("http://127.0.0.1:")?;
        if rest.is_empty() || !rest.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        Some(Source {
            base: format!("{base}/wnzn/metro"),
            api: format!("{base}/api/repos/wnzn/metro"),
            hosts: vec!["127.0.0.1".into()],
            https_only: false,
        })
    }

    pub fn latest_url(&self, name: &str) -> String {
        format!("{}/releases/latest/download/{name}", self.base)
    }

    /// `name` must be a release file name (as `SHA256SUMS` allows).
    pub fn asset_url(&self, version: &Version, name: &str) -> Result<String, Error> {
        let ok = (1..=160).contains(&name.len())
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
        if !ok {
            return Err(Error::new(ErrorCode::Manifest, "bad release file name"));
        }
        Ok(format!("{}/releases/download/v{version}/{name}", self.base))
    }

    pub fn notes_url(&self, version: &Version) -> String {
        format!("{}/releases/tags/v{version}", self.api)
    }

    fn is_api(&self, url: &str) -> bool {
        url.starts_with(&self.api)
    }

    /// Check a redirect target (absolute URLs only).
    pub fn check_redirect(&self, location: &str) -> Result<String, Error> {
        let refuse = || {
            Error::new(
                ErrorCode::RedirectRefused,
                "redirect to a host Metro doesn't trust",
            )
        };
        let (scheme, rest) = location.split_once("://").ok_or_else(refuse)?;
        if !(scheme == "https" || (!self.https_only && scheme == "http")) {
            return Err(refuse());
        }
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            return Err(refuse());
        }
        let host = if self.https_only {
            authority.strip_suffix(":443").unwrap_or(authority)
        } else {
            authority.rsplit_once(':').map_or(authority, |(h, _)| h)
        };
        if !self.hosts.iter().any(|h| h.eq_ignore_ascii_case(host)) {
            return Err(refuse());
        }
        if location.bytes().any(|c| c <= b' ' || c == 0x7f) {
            return Err(refuse());
        }
        Ok(location.to_string())
    }
}

/// The result of a check against "latest".
#[derive(Clone, Debug)]
pub enum CheckOutcome {
    /// "Latest" (unverified: nothing is offered from it) isn't newer.
    NotNewer { latest: Version },
    /// A newer release whose `SHA256SUMS` verified.
    Newer(crate::release::VerifiedRelease),
}

#[cfg(feature = "async")]
fn unverified_version(manifest: &[u8]) -> Result<Version, Error> {
    crate::manifest::parse(manifest).map(|m| m.version)
}

/// Release notes as shown in the UI (plain Markdown text, bounded).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Notes {
    pub body: String,
    pub truncated: bool,
    pub html_url: String,
}

const MAX_NOTES_BODY: usize = 32 * 1024;

/// Parse the GitHub release JSON: only `body` and `html_url` are read.
pub fn parse_notes(json: &[u8], version: &Version) -> Option<Notes> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let html_url = v.get("html_url")?.as_str()?;
    let expected = crate::release_url(version);
    let html_url = if html_url == expected {
        html_url.to_string()
    } else {
        expected
    };
    let mut body = v
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or_default()
        .to_string();
    let truncated = body.len() > MAX_NOTES_BODY;
    if truncated {
        let mut cut = MAX_NOTES_BODY;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
    }
    Some(Notes {
        body,
        truncated,
        html_url,
    })
}

fn rate_limit(
    status: u16,
    header: &dyn Fn(&str) -> Option<String>,
    now_secs: u64,
) -> Option<Error> {
    let exhausted = header("x-ratelimit-remaining").is_some_and(|r| r.trim() == "0");
    if !(status == 429 || (status == 403 && (exhausted || header("retry-after").is_some()))) {
        return None;
    }
    let wait = header("retry-after")
        .and_then(|r| r.trim().parse::<u64>().ok())
        .or_else(|| {
            header("x-ratelimit-reset")
                .and_then(|r| r.trim().parse::<u64>().ok())
                .map(|reset| reset.saturating_sub(now_secs))
        })
        .unwrap_or(3600)
        .clamp(60, 24 * 3600);
    let mut e = Error::new(ErrorCode::RateLimited, "GitHub asked Metro to wait");
    e.retry_after_secs = Some(wait);
    Some(e)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn status_error(status: u16) -> Error {
    Error::new(ErrorCode::Http, format!("HTTP {status}"))
}

fn too_large() -> Error {
    Error::new(ErrorCode::TooLarge, "a download is larger than allowed")
}

fn temp_path(dest: &Path) -> std::path::PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

#[cfg(feature = "blocking")]
pub use blocking::BlockingFetcher;

#[cfg(feature = "blocking")]
mod blocking {
    use super::*;
    use std::io::Read;

    /// ureq-based fetcher (the runner: no async runtime).
    pub struct BlockingFetcher {
        agent: ureq::Agent,
        pub source: Source,
    }

    impl BlockingFetcher {
        pub fn new(source: Source, timeout: Duration) -> BlockingFetcher {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .max_redirects(0)
                .http_status_as_error(false)
                .user_agent(USER_AGENT)
                .build()
                .into();
            BlockingFetcher { agent, source }
        }

        fn open(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, Error> {
            let mut url = url.to_string();
            for _ in 0..=MAX_REDIRECTS {
                let mut req = self
                    .agent
                    .get(&url)
                    .header("Accept", "application/octet-stream, application/json");
                if self.source.is_api(&url) {
                    req = req
                        .header("Accept", "application/vnd.github+json")
                        .header("X-GitHub-Api-Version", API_VERSION);
                }
                let resp = req.call().map_err(|e| {
                    Error::new(
                        ErrorCode::Offline,
                        format!("request failed: {}", short_transport(&e.to_string())),
                    )
                })?;
                let status = resp.status().as_u16();
                let header = |name: &str| {
                    resp.headers()
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                };
                if (300..400).contains(&status) {
                    let location = header("location").ok_or_else(|| status_error(status))?;
                    url = self.source.check_redirect(&location)?;
                    continue;
                }
                if let Some(e) = rate_limit(status, &header, now_secs()) {
                    return Err(e);
                }
                if status != 200 {
                    return Err(status_error(status));
                }
                return Ok(resp);
            }
            Err(Error::new(ErrorCode::RedirectRefused, "too many redirects"))
        }

        /// GET a small body (at most `cap` bytes).
        pub fn get(&self, url: &str, cap: usize) -> Result<Vec<u8>, Error> {
            let mut resp = self.open(url)?;
            if resp.body().content_length().is_some_and(|l| l > cap as u64) {
                return Err(too_large());
            }
            let mut out = Vec::new();
            resp.body_mut()
                .as_reader()
                .take(cap as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|_| Error::new(ErrorCode::Offline, "download interrupted"))?;
            if out.len() > cap {
                return Err(too_large());
            }
            Ok(out)
        }

        /// Stream a file to `dest` (via `dest.part`), returning its SHA-256.
        /// The final name appears only after the caller checked the digest:
        /// this leaves the bytes in `<dest>.part` and returns its path.
        pub fn download(
            &self,
            url: &str,
            dest: &Path,
            cap: u64,
        ) -> Result<([u8; 32], std::path::PathBuf), Error> {
            let mut resp = self.open(url)?;
            if resp.body().content_length().is_some_and(|l| l > cap) {
                return Err(too_large());
            }
            let part = temp_path(dest);
            let mut file = std::fs::File::create(&part)
                .map_err(|e| Error::new(ErrorCode::Io, format!("download: {}", e.kind())))?;
            let mut reader = resp.body_mut().with_config().limit(cap + 1).reader();
            let mut hasher = sha2::Sha256::new();
            let mut total = 0u64;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = reader
                    .read(&mut buf)
                    .map_err(|_| Error::new(ErrorCode::Offline, "download interrupted"))?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                if total > cap {
                    drop(file);
                    let _ = std::fs::remove_file(&part);
                    return Err(too_large());
                }
                hasher.update(&buf[..n]);
                file.write_all(&buf[..n])
                    .map_err(|e| Error::new(ErrorCode::Io, format!("download: {}", e.kind())))?;
            }
            file.sync_all()
                .map_err(|e| Error::new(ErrorCode::Io, format!("download: {}", e.kind())))?;
            Ok((hasher.finalize().into(), part))
        }

        /// Fetch and verify the `SHA256SUMS` of exactly `version` (the
        /// runner's path: the server named a version, never a URL).
        pub fn fetch_release(
            &self,
            verifier: &dyn crate::ReleaseVerifier,
            version: &Version,
        ) -> Result<crate::VerifiedRelease, Error> {
            let manifest = self.get(
                &self.source.asset_url(version, "SHA256SUMS")?,
                crate::MAX_MANIFEST,
            )?;
            let bundle = self.get(
                &self.source.asset_url(version, "SHA256SUMS.sigstore.json")?,
                crate::MAX_BUNDLE,
            )?;
            let release = crate::release::verify_manifest(verifier, &manifest, &bundle)?;
            if release.version != *version {
                return Err(Error::new(
                    ErrorCode::Inconsistent,
                    "signed release is for another version",
                ));
            }
            Ok(release)
        }
    }
}

#[cfg(feature = "async")]
pub use nonblocking::AsyncFetcher;

#[cfg(feature = "async")]
mod nonblocking {
    use super::*;

    /// reqwest-based fetcher (desktop launcher and server).
    pub struct AsyncFetcher {
        client: reqwest::Client,
        pub source: Source,
    }

    impl AsyncFetcher {
        pub fn new(source: Source, timeout: Duration) -> Result<AsyncFetcher, Error> {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15).min(timeout))
                .timeout(timeout)
                .user_agent(USER_AGENT)
                .referer(false)
                .build()
                .map_err(|_| Error::new(ErrorCode::Io, "HTTP client"))?;
            Ok(AsyncFetcher { client, source })
        }

        async fn open(&self, url: &str) -> Result<reqwest::Response, Error> {
            let mut url = url.to_string();
            for _ in 0..=MAX_REDIRECTS {
                let mut req = self.client.get(&url);
                req = if self.source.is_api(&url) {
                    req.header("Accept", "application/vnd.github+json")
                        .header("X-GitHub-Api-Version", API_VERSION)
                } else {
                    req.header("Accept", "application/octet-stream, application/json")
                };
                let resp = req.send().await.map_err(|e| {
                    Error::new(
                        ErrorCode::Offline,
                        format!(
                            "request failed: {}",
                            short_transport(&e.without_url().to_string())
                        ),
                    )
                })?;
                let status = resp.status().as_u16();
                let header = |name: &str| {
                    resp.headers()
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                };
                if (300..400).contains(&status) {
                    let location = header("location").ok_or_else(|| status_error(status))?;
                    url = self.source.check_redirect(&location)?;
                    continue;
                }
                if let Some(e) = rate_limit(status, &header, now_secs()) {
                    return Err(e);
                }
                if status != 200 {
                    return Err(status_error(status));
                }
                return Ok(resp);
            }
            Err(Error::new(ErrorCode::RedirectRefused, "too many redirects"))
        }

        pub async fn get(&self, url: &str, cap: usize) -> Result<Vec<u8>, Error> {
            let mut resp = self.open(url).await?;
            if resp.content_length().is_some_and(|l| l > cap as u64) {
                return Err(too_large());
            }
            let mut out = Vec::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|_| Error::new(ErrorCode::Offline, "download interrupted"))?
            {
                if out.len() + chunk.len() > cap {
                    return Err(too_large());
                }
                out.extend_from_slice(&chunk);
            }
            Ok(out)
        }

        /// Stream to `<dest>.part`; returns the digest and that path.
        /// `progress(done, total)` is called as bytes arrive.
        pub async fn download(
            &self,
            url: &str,
            dest: &Path,
            cap: u64,
            progress: &(dyn Fn(u64, u64) + Send + Sync),
        ) -> Result<([u8; 32], std::path::PathBuf), Error> {
            let mut resp = self.open(url).await?;
            let total_hint = resp.content_length().unwrap_or(0);
            if total_hint > cap {
                return Err(too_large());
            }
            let part = temp_path(dest);
            let io =
                |e: std::io::Error| Error::new(ErrorCode::Io, format!("download: {}", e.kind()));
            let mut file = std::fs::File::create(&part).map_err(io)?;
            let mut hasher = sha2::Sha256::new();
            let mut total = 0u64;
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|_| Error::new(ErrorCode::Offline, "download interrupted"))?
            {
                total += chunk.len() as u64;
                if total > cap {
                    drop(file);
                    let _ = std::fs::remove_file(&part);
                    return Err(too_large());
                }
                hasher.update(&chunk);
                file.write_all(&chunk).map_err(io)?;
                progress(total, total_hint.max(total));
            }
            file.sync_all().map_err(io)?;
            Ok((hasher.finalize().into(), part))
        }

        /// One check: the latest `SHA256SUMS`; only when it names a version
        /// newer than `running` are the bundle (by explicit version, so a
        /// publication between requests can't mix two releases) and, on a
        /// mismatch, that version's own `SHA256SUMS` fetched and verified.
        pub async fn check(
            &self,
            verifier: &dyn crate::ReleaseVerifier,
            running: &Version,
        ) -> Result<CheckOutcome, Error> {
            let manifest = self
                .get(&self.source.latest_url("SHA256SUMS"), crate::MAX_MANIFEST)
                .await?;
            let latest = unverified_version(&manifest)?;
            if latest <= *running {
                return Ok(CheckOutcome::NotNewer { latest });
            }
            let bundle = self
                .get(
                    &self.source.asset_url(&latest, "SHA256SUMS.sigstore.json")?,
                    crate::MAX_BUNDLE,
                )
                .await?;
            let release = match crate::release::verify_manifest(verifier, &manifest, &bundle) {
                Ok(r) => r,
                Err(first) if first.code == ErrorCode::NotVerified => {
                    let again = self
                        .get(
                            &self.source.asset_url(&latest, "SHA256SUMS")?,
                            crate::MAX_MANIFEST,
                        )
                        .await?;
                    if again == manifest {
                        return Err(first);
                    }
                    crate::release::verify_manifest(verifier, &again, &bundle)?
                }
                Err(e) => return Err(e),
            };
            if release.version != latest {
                return Err(Error::new(
                    ErrorCode::Inconsistent,
                    "signed release is for another version",
                ));
            }
            Ok(CheckOutcome::Newer(release))
        }

        /// Fetch and verify the `SHA256SUMS` of exactly `version`.
        pub async fn fetch_release(
            &self,
            verifier: &dyn crate::ReleaseVerifier,
            version: &Version,
        ) -> Result<crate::VerifiedRelease, Error> {
            let manifest = self
                .get(
                    &self.source.asset_url(version, "SHA256SUMS")?,
                    crate::MAX_MANIFEST,
                )
                .await?;
            let bundle = self
                .get(
                    &self.source.asset_url(version, "SHA256SUMS.sigstore.json")?,
                    crate::MAX_BUNDLE,
                )
                .await?;
            let release = crate::release::verify_manifest(verifier, &manifest, &bundle)?;
            if release.version != *version {
                return Err(Error::new(
                    ErrorCode::Inconsistent,
                    "signed release is for another version",
                ));
            }
            Ok(release)
        }

        /// GitHub's release notes for `version` (`None` when unavailable,
        /// including when rate limited: the UI links the release page).
        pub async fn notes(&self, version: &Version) -> Option<Notes> {
            let json = self
                .get(&self.source.notes_url(version), crate::MAX_NOTES_JSON)
                .await
                .ok()?;
            parse_notes(&json, version)
        }
    }
}

/// Keep transport errors short and free of URLs (they could carry query
/// strings from a CDN redirect).
fn short_transport(text: &str) -> String {
    let cleaned: String = text
        .split_whitespace()
        .filter(|w| !w.contains("://"))
        .collect::<Vec<_>>()
        .join(" ");
    cleaned.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_redirects() {
        let s = Source::github();
        assert_eq!(
            s.latest_url("SHA256SUMS"),
            "https://github.com/wnzn/metro/releases/latest/download/SHA256SUMS"
        );
        let v = Version::parse("0.5.1").unwrap();
        assert_eq!(
            s.asset_url(&v, "SHA256SUMS.sigstore.json").unwrap(),
            "https://github.com/wnzn/metro/releases/download/v0.5.1/SHA256SUMS.sigstore.json"
        );
        assert!(s.asset_url(&v, "../x").is_err());
        assert!(s.asset_url(&v, "a/b").is_err());
        assert_eq!(
            s.notes_url(&v),
            "https://api.github.com/repos/wnzn/metro/releases/tags/v0.5.1"
        );
        assert!(
            s.check_redirect("https://release-assets.githubusercontent.com/x?sig=1")
                .is_ok()
        );
        assert!(
            s.check_redirect("https://github.com/wnzn/metro/releases/download/v0.4.0/SHA256SUMS")
                .is_ok()
        );
        for bad in [
            "http://github.com/x",
            "https://evil.example/x",
            "https://github.com.evil.example/x",
            "https://user@github.com/x",
            "/relative",
            "https://github.com:8443/x",
            "ftp://github.com/x",
        ] {
            assert_eq!(
                s.check_redirect(bad).unwrap_err().code,
                ErrorCode::RedirectRefused,
                "{bad}"
            );
        }
        assert!(Source::loopback_fixture("http://127.0.0.1:8080").is_some());
        assert!(Source::loopback_fixture("http://10.0.0.1:8080").is_none());
        assert!(Source::loopback_fixture("https://github.com").is_none());
    }

    #[test]
    fn rate_limits() {
        let h = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            }
        };
        let e = rate_limit(429, &h(&[("retry-after", "120")]), 0).unwrap();
        assert_eq!(
            (e.code, e.retry_after_secs),
            (ErrorCode::RateLimited, Some(120))
        );
        let e = rate_limit(
            403,
            &h(&[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1000"),
            ]),
            100,
        )
        .unwrap();
        assert_eq!(e.retry_after_secs, Some(900));
        assert!(rate_limit(403, &h(&[]), 0).is_none());
        assert!(rate_limit(200, &h(&[("retry-after", "5")]), 0).is_none());
        assert_eq!(
            rate_limit(429, &h(&[("retry-after", "1")]), 0)
                .unwrap()
                .retry_after_secs,
            Some(60)
        );
    }

    #[test]
    fn notes() {
        let v = Version::parse("0.5.0").unwrap();
        let n = parse_notes(
            br#"{"body":"Hi","html_url":"https://github.com/wnzn/metro/releases/tag/v0.5.0"}"#,
            &v,
        )
        .unwrap();
        assert_eq!(n.body, "Hi");
        let n = parse_notes(br#"{"body":"Hi","html_url":"https://evil.example/"}"#, &v).unwrap();
        assert_eq!(
            n.html_url,
            "https://github.com/wnzn/metro/releases/tag/v0.5.0"
        );
        let long = format!(r#"{{"body":"{}","html_url":"x"}}"#, "a".repeat(40_000));
        let n = parse_notes(long.as_bytes(), &v).unwrap();
        assert!(n.truncated && n.body.len() == 32 * 1024);
    }
}
