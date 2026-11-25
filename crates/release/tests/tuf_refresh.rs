//! Sigstore trusted-root refresh over TUF: real Sigstore TUF metadata
//! (captured from tuf-repo-cdn.sigstore.dev on 2026-11-25) and a synthetic
//! Ed25519 repository for threshold and rollback cases. Offline.
#![cfg(feature = "async")]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
use metro_release::fetch::Source;
use metro_release::sigstore::{SigstoreVerifier, VENDORED_TRUSTED_ROOT, VENDORED_TUF_ROOT};
use metro_release::trust_refresh::{LayeredVerifier, REFRESH_INTERVAL_SECS, TrustCache};
use metro_release::{ErrorCode, ReleaseVerifier, Signed, hex, sha256};
use serde_json::{Value, json};

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tuf");
const TARGET_SHA: &str = "6494e21ea73fa7ee769f85f57d5a3e6a08725eae1e38c755fc3517c9e6bc0b66";
const MANIFEST: &[u8] = include_bytes!("fixtures/v0.4.0/SHA256SUMS");
const BUNDLE: &[u8] = include_bytes!("fixtures/v0.4.0/SHA256SUMS.sigstore.json");

type Files = Arc<Mutex<HashMap<String, Vec<u8>>>>;

struct Repo {
    base: String,
    files: Files,
    log: Arc<Mutex<Vec<String>>>,
}

fn serve(files: HashMap<String, Vec<u8>>) -> Repo {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let files = Arc::new(Mutex::new(files));
    let log = Arc::new(Mutex::new(Vec::new()));
    let (shared, l) = (files.clone(), log.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
            }
            let path = line
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .trim_start_matches('/')
                .to_string();
            l.lock().unwrap().push(path.clone());
            let body = shared.lock().unwrap().get(&path).cloned();
            let (status, body) = body.map_or((404, b"no".to_vec()), |b| (200, b));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    Repo { base, files, log }
}

fn real_files() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    for name in [
        "14.root.json",
        "15.root.json",
        "16.root.json",
        "timestamp.json",
        "166.snapshot.json",
        "14.targets.json",
    ] {
        m.insert(
            name.to_string(),
            std::fs::read(format!("{FIX}/{name}")).unwrap(),
        );
    }
    let t = format!("targets/{TARGET_SHA}.trusted_root.json");
    m.insert(t.clone(), std::fs::read(format!("{FIX}/{t}")).unwrap());
    m
}

fn now() -> jiff::Timestamp {
    "2026-11-25T10:30:00Z".parse().unwrap()
}

fn cache(repo: &Repo, dir: &std::path::Path, root: &[u8]) -> TrustCache {
    TrustCache::with_source(dir, Source::loopback_tuf_fixture(&repo.base).unwrap(), root)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn real_sigstore_metadata_refreshes_and_verifies_the_release() {
    let repo = serve(real_files());
    let dir = tempfile::tempdir().unwrap();
    let c = cache(&repo, dir.path(), VENDORED_TUF_ROOT);
    assert!(c.refresh_due(now()));
    let v = rt().block_on(c.refresh(now())).expect("refresh");
    // The refreshed root verifies the published 0.4.0 release.
    let facts = v.verify(Signed::Bytes(MANIFEST), BUNDLE).unwrap();
    assert_eq!(facts.commit, "1a743e59d4ec6c1da85d09d92c99457561f64e55");
    assert_eq!(
        hex(&sha256(
            &std::fs::read(dir.path().join("trusted_root.json")).unwrap()
        )),
        TARGET_SHA
    );
    assert!(c.cached_verifier().is_some());
    // At most weekly.
    assert!(!c.refresh_due(now()));
    let later = now()
        .checked_add(jiff::SignedDuration::from_secs(REFRESH_INTERVAL_SECS))
        .unwrap();
    assert!(c.refresh_due(later));
    let log = repo.log.lock().unwrap().clone();
    assert_eq!(
        log,
        [
            "17.root.json",
            "timestamp.json",
            "166.snapshot.json",
            "14.targets.json",
            &format!("targets/{TARGET_SHA}.trusted_root.json")
        ]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn real_root_rotation_from_v14_walks_the_signed_chain() {
    let repo = serve(real_files());
    let dir = tempfile::tempdir().unwrap();
    let root14 = std::fs::read(format!("{FIX}/14.root.json")).unwrap();
    rt().block_on(cache(&repo, dir.path(), &root14).refresh(now()))
        .expect("rotation 14 → 16");
    let log = repo.log.lock().unwrap().clone();
    assert_eq!(&log[..3], ["15.root.json", "16.root.json", "17.root.json"]);

    // A tampered intermediate root breaks the chain.
    let repo = serve(real_files());
    let mut r15: Value =
        serde_json::from_slice(&repo.files.lock().unwrap()["15.root.json"]).unwrap();
    r15["signed"]["expires"] = "2099-01-01T00:00:00Z".into();
    repo.files
        .lock()
        .unwrap()
        .insert("15.root.json".into(), serde_json::to_vec(&r15).unwrap());
    let dir = tempfile::tempdir().unwrap();
    assert!(
        rt().block_on(cache(&repo, dir.path(), &root14).refresh(now()))
            .is_err()
    );
}

#[test]
fn tampered_real_metadata_and_target_are_refused() {
    type Change = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, Change)> = vec![
        (
            "timestamp.json",
            Box::new(|v: &mut Value| v["signed"]["version"] = 845.into()),
        ),
        (
            "166.snapshot.json",
            Box::new(|v: &mut Value| v["signed"]["expires"] = "2099-01-01T00:00:00Z".into()),
        ),
        (
            "14.targets.json",
            Box::new(|v: &mut Value| {
                v["signed"]["targets"]["trusted_root.json"]["length"] = 6788.into()
            }),
        ),
        (
            "timestamp.json",
            Box::new(|v: &mut Value| {
                let s = v["signatures"][0]["sig"].as_str().unwrap().to_string();
                let rest = &s[1..];
                let flipped = if s.starts_with('3') {
                    format!("4{rest}")
                } else {
                    format!("3{rest}")
                };
                v["signatures"][0]["sig"] = flipped.into();
            }),
        ),
    ];
    for (name, change) in cases {
        let repo = serve(real_files());
        let mut v: Value = serde_json::from_slice(&repo.files.lock().unwrap()[name]).unwrap();
        change(&mut v);
        repo.files
            .lock()
            .unwrap()
            .insert(name.into(), serde_json::to_vec(&v).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let r = rt().block_on(cache(&repo, dir.path(), VENDORED_TUF_ROOT).refresh(now()));
        assert!(r.is_err(), "{name}");
        assert!(!dir.path().join("trusted_root.json").exists(), "{name}");
    }
    // The target itself.
    let repo = serve(real_files());
    let t = format!("targets/{TARGET_SHA}.trusted_root.json");
    let mut bytes = repo.files.lock().unwrap()[&t].clone();
    bytes[100] ^= 1;
    repo.files.lock().unwrap().insert(t, bytes);
    let dir = tempfile::tempdir().unwrap();
    assert!(
        rt().block_on(cache(&repo, dir.path(), VENDORED_TUF_ROOT).refresh(now()))
            .is_err()
    );
}

#[test]
fn expired_metadata_is_refused() {
    for when in ["2026-12-03T00:00:00Z", "2027-05-01T00:00:00Z"] {
        let repo = serve(real_files());
        let dir = tempfile::tempdir().unwrap();
        let t: jiff::Timestamp = when.parse().unwrap();
        let e = rt()
            .block_on(cache(&repo, dir.path(), VENDORED_TUF_ROOT).refresh(t))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotVerified, "{when}");
    }
}

#[test]
fn only_the_tuf_host_is_allowed() {
    let s = Source::sigstore_tuf();
    assert_eq!(
        s.tuf_url("16.root.json").unwrap(),
        "https://tuf-repo-cdn.sigstore.dev/16.root.json"
    );
    assert!(
        s.tuf_url("../x").is_err()
            && s.tuf_url("/etc/passwd").is_err()
            && s.tuf_url("a b").is_err()
    );
    assert!(
        s.check_redirect("https://tuf-repo-cdn.sigstore.dev/targets/x")
            .is_ok()
    );
    for bad in [
        "https://github.com/x",
        "https://release-assets.githubusercontent.com/x",
        "http://tuf-repo-cdn.sigstore.dev/x",
    ] {
        assert_eq!(
            s.check_redirect(bad).unwrap_err().code,
            ErrorCode::RedirectRefused,
            "{bad}"
        );
    }
    // And GitHub release downloads never go to the TUF host.
    assert!(
        Source::github()
            .check_redirect("https://tuf-repo-cdn.sigstore.dev/x")
            .is_err()
    );
}

#[test]
fn layered_verifier_uses_the_refreshed_root_only_when_the_vendored_one_is_stale() {
    // A "vendored" root without the Rekor v1 log is stale for 0.4.0.
    let mut stale: Value = serde_json::from_str(VENDORED_TRUSTED_ROOT).unwrap();
    stale["tlogs"]
        .as_array_mut()
        .unwrap()
        .retain(|t| t["baseUrl"] != "https://rekor.sigstore.dev");
    let stale = SigstoreVerifier::from_trusted_root_json(&stale.to_string()).unwrap();
    let e = stale.verify(Signed::Bytes(MANIFEST), BUNDLE).unwrap_err();
    assert_eq!(e.code, ErrorCode::TrustRootStale, "{e}");
    let layered = LayeredVerifier::new(stale, None);
    assert_eq!(
        layered
            .verify(Signed::Bytes(MANIFEST), BUNDLE)
            .unwrap_err()
            .code,
        ErrorCode::TrustRootStale
    );
    layered.set_refreshed(SigstoreVerifier::vendored().unwrap());
    assert!(layered.verify(Signed::Bytes(MANIFEST), BUNDLE).is_ok());
    // Never a different signer: a tampered manifest still fails with either root.
    let mut m = MANIFEST.to_vec();
    m[0] ^= 1;
    assert!(layered.verify(Signed::Bytes(&m), BUNDLE).is_err());
}

// ---- synthetic repository (threshold, rotation, rollback) ----

struct Key {
    kp: Ed25519KeyPair,
    id: String,
}

fn key() -> Key {
    let kp = Ed25519KeyPair::generate().unwrap();
    let id = hex(&sha256(kp.public_key().as_ref()));
    Key { kp, id }
}

fn public(k: &Key) -> Value {
    let pubhex: String =
        k.kp.public_key()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
    json!({"keytype": "ed25519", "scheme": "ed25519", "keyval": {"public": pubhex}})
}

fn sign(signed: Value, keys: &[&Key]) -> Vec<u8> {
    let canonical = sigstore_tuf::canonical_json::to_canonical_bytes(&signed).unwrap();
    let sigs: Vec<Value> = keys
        .iter()
        .map(|k| {
            let sig: String =
                k.kp.sign(&canonical)
                    .as_ref()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
            json!({"keyid": k.id, "sig": sig})
        })
        .collect();
    serde_json::to_vec(&json!({"signed": signed, "signatures": sigs})).unwrap()
}

const FAR: &str = "2030-01-01T00:00:00Z";

fn root(version: u64, root_keys: &[&Key], threshold: usize, role_key: &Key) -> Value {
    let mut keys = serde_json::Map::new();
    for k in root_keys.iter().chain([&role_key]) {
        keys.insert(k.id.clone(), public(k));
    }
    let ids: Vec<&str> = root_keys.iter().map(|k| k.id.as_str()).collect();
    let role = json!({"keyids": [role_key.id], "threshold": 1});
    json!({"_type": "root", "spec_version": "1.0.31", "version": version, "expires": FAR, "consistent_snapshot": true,
        "keys": keys, "roles": {"root": {"keyids": ids, "threshold": threshold},
        "timestamp": role, "snapshot": role, "targets": role}})
}

/// timestamp v`ts` → snapshot v`v` → targets v`v` → trusted_root.json.
fn lower(files: &mut HashMap<String, Vec<u8>>, ts: u64, v: u64, k: &Key) {
    let target = VENDORED_TRUSTED_ROOT.as_bytes();
    let h = hex(&sha256(target));
    files.insert(format!("targets/{h}.trusted_root.json"), target.to_vec());
    let targets = json!({"_type": "targets", "spec_version": "1.0.31", "version": v, "expires": FAR,
        "targets": {"trusted_root.json": {"length": target.len(), "hashes": {"sha256": h}}}});
    files.insert(format!("{v}.targets.json"), sign(targets, &[k]));
    let snapshot = json!({"_type": "snapshot", "spec_version": "1.0.31", "version": v, "expires": FAR,
        "meta": {"targets.json": {"version": v}}});
    files.insert(format!("{v}.snapshot.json"), sign(snapshot, &[k]));
    let timestamp = json!({"_type": "timestamp", "spec_version": "1.0.31", "version": ts, "expires": FAR,
        "meta": {"snapshot.json": {"version": v}}});
    files.insert("timestamp.json".into(), sign(timestamp, &[k]));
}

#[test]
fn synthetic_threshold_rotation_and_rollback() {
    let (a, b, c, role) = (key(), key(), key(), key());
    // Root v1: threshold 2 of {a, b}.
    let r1 = sign(root(1, &[&a, &b], 2, &role), &[&a, &b]);
    let mut files = HashMap::new();
    lower(&mut files, 5, 5, &role);
    let repo = serve(files);
    let dir = tempfile::tempdir().unwrap();
    rt().block_on(cache(&repo, dir.path(), &r1).refresh(now()))
        .expect("v1 repository");

    // A rotation to v2 signed by only one of the old root's keys: refused.
    let r2_bad = sign(root(2, &[&a, &c], 2, &role), &[&a, &c]);
    repo.files
        .lock()
        .unwrap()
        .insert("2.root.json".into(), r2_bad);
    let dir2 = tempfile::tempdir().unwrap();
    assert!(
        rt().block_on(cache(&repo, dir2.path(), &r1).refresh(now()))
            .is_err(),
        "threshold of the old root"
    );
    // Signed by both old keys but not by the new root's own threshold: refused.
    let r2_half = sign(root(2, &[&a, &c], 2, &role), &[&a, &b]);
    repo.files
        .lock()
        .unwrap()
        .insert("2.root.json".into(), r2_half);
    assert!(
        rt().block_on(cache(&repo, dir2.path(), &r1).refresh(now()))
            .is_err(),
        "threshold of the new root"
    );
    // Properly cross-signed: accepted.
    let r2 = sign(root(2, &[&a, &c], 2, &role), &[&a, &b, &c]);
    repo.files.lock().unwrap().insert("2.root.json".into(), r2);
    rt().block_on(cache(&repo, dir2.path(), &r1).refresh(now()))
        .expect("rotation");

    // Rollback: the cache remembers timestamp v5; a validly signed v4 later is refused.
    let mut older = HashMap::new();
    lower(&mut older, 4, 4, &role);
    for (k, v) in older {
        repo.files.lock().unwrap().insert(k, v);
    }
    repo.files.lock().unwrap().remove("2.root.json");
    let e = rt()
        .block_on(cache(&repo, dir.path(), &r1).refresh(now()))
        .unwrap_err();
    assert!(
        e.detail.to_lowercase().contains("rollback") || e.detail.contains("version"),
        "{e}"
    );
    // Without the cached floor the same v4 set would be accepted: the floor did it.
    let fresh = tempfile::tempdir().unwrap();
    assert!(
        rt().block_on(cache(&repo, fresh.path(), &r1).refresh(now()))
            .is_ok()
    );
}
