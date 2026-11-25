//! Fetchers against a loopback fixture "GitHub" (test 8 in the design).
#![cfg(all(feature = "blocking", feature = "async"))]

use metro_release::fetch::{AsyncFetcher, BlockingFetcher, CheckOutcome, Source};
use metro_release::testing::{FixtureRelease, TestVerifier};
use metro_release::{ErrorCode, Version};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Route = Box<dyn Fn(&str) -> Option<(u16, Vec<(String, String)>, Vec<u8>)> + Send + Sync>;

type Log = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;

struct Fixture {
    base: String,
    requests: Log,
}

fn serve(route: Route) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let mut headers = Vec::new();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
                if let Some((k, v)) = h.trim_end().split_once(':') {
                    headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
                }
            }
            log.lock().unwrap().push((path.clone(), headers));
            let (status, extra, body) = route(&path).unwrap_or((404, vec![], b"nope".to_vec()));
            let mut head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (k, v) in extra {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
            let mut sink = [0u8; 1];
            let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
            let _ = stream.read(&mut sink);
        }
    });
    Fixture { base, requests }
}

fn release_routes(rel: FixtureRelease, latest_redirect: bool) -> Route {
    let v = rel.version.to_string();
    let files = rel.files;
    Box::new(move |path: &str| {
        let prefix = "/wnzn/metro/releases/";
        let rest = path.strip_prefix(prefix)?;
        if let Some(name) = rest.strip_prefix("latest/download/") {
            if latest_redirect {
                return Some((
                    302,
                    vec![("Location".into(), format!("/redirect-target/{name}"))],
                    vec![],
                ));
            }
            return files.get(name).map(|b| (200, vec![], b.clone()));
        }
        let name = rest.strip_prefix(&format!("download/v{v}/"))?;
        files.get(name).map(|b| (200, vec![], b.clone()))
    })
}

fn source(f: &Fixture) -> Source {
    Source::loopback_fixture(&f.base).unwrap()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn check_finds_newer_and_sends_no_identifiers() {
    let f = serve(release_routes(
        FixtureRelease::new("0.5.1", BTreeMap::new()),
        false,
    ));
    let fetcher = AsyncFetcher::new(source(&f), Duration::from_secs(10)).unwrap();
    let running = Version::parse("0.5.0").unwrap();
    let out = rt()
        .block_on(fetcher.check(&TestVerifier, &running))
        .unwrap();
    match out {
        CheckOutcome::Newer(r) => assert_eq!(r.version.to_string(), "0.5.1"),
        other => panic!("{other:?}"),
    }
    let reqs = f.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2, "manifest + bundle only");
    assert_eq!(reqs[0].0, "/wnzn/metro/releases/latest/download/SHA256SUMS");
    assert_eq!(
        reqs[1].0,
        "/wnzn/metro/releases/download/v0.5.1/SHA256SUMS.sigstore.json"
    );
    for (_, headers) in reqs {
        let names: Vec<&str> = headers.iter().map(|(k, _)| k.as_str()).collect();
        let ua = headers
            .iter()
            .find(|(k, _)| k == "user-agent")
            .map(|(_, v)| v.as_str());
        assert_eq!(ua, Some("Metro"));
        for forbidden in ["cookie", "authorization", "referer"] {
            assert!(!names.contains(&forbidden), "{forbidden}");
        }
        for (_, v) in &headers {
            assert!(!v.contains("0.5.0"), "running version leaked: {v}");
        }
    }
}

#[test]
fn up_to_date_fetches_only_the_manifest() {
    let f = serve(release_routes(
        FixtureRelease::new("0.5.0", BTreeMap::new()),
        false,
    ));
    let fetcher = AsyncFetcher::new(source(&f), Duration::from_secs(10)).unwrap();
    let running = Version::parse("0.5.0").unwrap();
    let out = rt()
        .block_on(fetcher.check(&TestVerifier, &running))
        .unwrap();
    assert!(matches!(out, CheckOutcome::NotNewer { .. }));
    assert_eq!(f.requests.lock().unwrap().len(), 1);
}

#[test]
fn foreign_redirect_is_refused() {
    let f = serve(Box::new(|path: &str| {
        path.ends_with("SHA256SUMS").then(|| {
            (
                302,
                vec![("Location".into(), "https://evil.example/SHA256SUMS".into())],
                vec![],
            )
        })
    }));
    let fetcher = AsyncFetcher::new(source(&f), Duration::from_secs(10)).unwrap();
    let e = rt()
        .block_on(fetcher.check(&TestVerifier, &Version::parse("0.1.0").unwrap()))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::RedirectRefused);
    let b = BlockingFetcher::new(source(&f), Duration::from_secs(10));
    let url = source(&f).latest_url("SHA256SUMS");
    assert_eq!(
        b.get(&url, 1000).unwrap_err().code,
        ErrorCode::RedirectRefused
    );
}

#[test]
fn oversized_manifest_is_refused() {
    let f = serve(Box::new(|_: &str| {
        Some((200, vec![], vec![b'a'; 2 * 1024 * 1024]))
    }));
    let fetcher = AsyncFetcher::new(source(&f), Duration::from_secs(10)).unwrap();
    let e = rt()
        .block_on(fetcher.check(&TestVerifier, &Version::parse("0.1.0").unwrap()))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::TooLarge);
}

#[test]
fn rate_limit_is_reported_with_its_wait() {
    let f = serve(Box::new(|_: &str| {
        Some((429, vec![("Retry-After".into(), "600".into())], vec![]))
    }));
    let fetcher = AsyncFetcher::new(source(&f), Duration::from_secs(10)).unwrap();
    let e = rt()
        .block_on(fetcher.check(&TestVerifier, &Version::parse("0.1.0").unwrap()))
        .unwrap_err();
    assert_eq!(
        (e.code, e.retry_after_secs),
        (ErrorCode::RateLimited, Some(600))
    );
}

#[test]
fn offline_is_offline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let fetcher = AsyncFetcher::new(
        Source::loopback_fixture(&base).unwrap(),
        Duration::from_secs(5),
    )
    .unwrap();
    let e = rt()
        .block_on(fetcher.check(&TestVerifier, &Version::parse("0.1.0").unwrap()))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Offline);
}

#[test]
fn race_between_manifest_and_bundle_uses_the_versions_own_manifest() {
    // "latest" serves 0.5.1's manifest but with a byte changed (as if a new
    // publication landed between the two requests); the explicit v0.5.1
    // manifest is intact.
    let rel = FixtureRelease::new("0.5.1", BTreeMap::new());
    let good = rel.files["SHA256SUMS"].clone();
    let mut stale = good.clone();
    stale[0] = if stale[0] == b'0' { b'1' } else { b'0' };
    let files = rel.files.clone();
    let f = serve(Box::new(move |path: &str| {
        if path == "/wnzn/metro/releases/latest/download/SHA256SUMS" {
            return Some((200, vec![], stale.clone()));
        }
        let name = path.strip_prefix("/wnzn/metro/releases/download/v0.5.1/")?;
        files.get(name).map(|b| (200, vec![], b.clone()))
    }));
    let fetcher = AsyncFetcher::new(source(&f), Duration::from_secs(10)).unwrap();
    let out = rt()
        .block_on(fetcher.check(&TestVerifier, &Version::parse("0.5.0").unwrap()))
        .unwrap();
    assert!(matches!(out, CheckOutcome::Newer(r) if r.version.to_string() == "0.5.1"));
    assert_eq!(f.requests.lock().unwrap().len(), 3);
}

#[test]
fn blocking_fetch_release_by_version() {
    let f = serve(release_routes(
        FixtureRelease::new("0.5.1", BTreeMap::new()),
        true,
    ));
    let b = BlockingFetcher::new(source(&f), Duration::from_secs(10));
    let r = b
        .fetch_release(&TestVerifier, &Version::parse("0.5.1").unwrap())
        .unwrap();
    assert_eq!(r.version.to_string(), "0.5.1");
    // Asking for another version: its files don't exist.
    assert!(
        b.fetch_release(&TestVerifier, &Version::parse("0.5.2").unwrap())
            .is_err()
    );
    // Download and digest.
    let dir = tempfile::tempdir().unwrap();
    let name = r.runner_linux();
    let url = b.source.asset_url(&r.version, &name).unwrap();
    let (digest, part) = b.download(&url, &dir.path().join(&name), 1 << 20).unwrap();
    r.check_digest(&name, &digest).unwrap();
    assert!(part.ends_with(format!("{name}.part")));
}
