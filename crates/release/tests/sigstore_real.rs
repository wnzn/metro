//! Real Sigstore verification on the published Metro 0.4.0 release files
//! (public data, committed as fixtures), plus tamper cases. Offline.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use metro_release::release::verify_manifest;
use metro_release::sigstore::{SigstoreVerifier, VENDORED_TRUSTED_ROOT, VENDORED_TUF_ROOT};
use metro_release::{ErrorCode, ReleaseVerifier, Signed, hex, sha256};
use serde_json::Value;

const MANIFEST: &[u8] = include_bytes!("fixtures/v0.4.0/SHA256SUMS");
const BUNDLE: &[u8] = include_bytes!("fixtures/v0.4.0/SHA256SUMS.sigstore.json");
const RUNNER_BUNDLE: &[u8] =
    include_bytes!("fixtures/v0.4.0/Metro-runner-0.4.0-linux-x64.tar.gz.sigstore.json");
const COMMIT: &str = "1a743e59d4ec6c1da85d09d92c99457561f64e55";

fn verifier() -> SigstoreVerifier {
    SigstoreVerifier::vendored().expect("vendored root loads")
}

fn json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap()
}

fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap()
}

fn flip_b64(text: &str, at_from_end: usize) -> String {
    let mut raw = B64.decode(text).unwrap();
    let i = raw.len() - at_from_end;
    raw[i] ^= 0x01;
    B64.encode(raw)
}

#[test]
fn trust_files_are_pinned() {
    // Changing a vendored trust file is a reviewed change: update trust/README.md too.
    assert_eq!(
        hex(&sha256(VENDORED_TRUSTED_ROOT.as_bytes())),
        "6494e21ea73fa7ee769f85f57d5a3e6a08725eae1e38c755fc3517c9e6bc0b66"
    );
    assert_eq!(
        hex(&sha256(VENDORED_TUF_ROOT)),
        "27aea7e372aa7864065174f0661784fcf8b0e28b1dbfebeada31a0f1fb19167f"
    );
}

#[test]
fn real_040_release_verifies() {
    let release = verify_manifest(&verifier(), MANIFEST, BUNDLE).expect("0.4.0 verifies");
    assert_eq!(release.version.to_string(), "0.4.0");
    assert_eq!(release.commit, COMMIT);
    assert_eq!(release.signed_at, 1_795_589_872);
    assert_eq!(release.msi, "Metro-0.4.0-windows-x64.unsigned-preview.msi");
}

#[test]
fn real_040_runner_archive_signature_verifies_by_digest() {
    let v = verifier();
    let release = verify_manifest(&v, MANIFEST, BUNDLE).unwrap();
    let name = release.runner_linux();
    let digest = release.sha256_of(&name).unwrap();
    release
        .check_archive_signature(&v, &name, &digest, RUNNER_BUNDLE)
        .expect("archive bundle verifies");
    let mut wrong = digest;
    wrong[0] ^= 1;
    assert!(v.verify(Signed::Sha256(wrong), RUNNER_BUNDLE).is_err());
    // The archive's bundle doesn't sign SHA256SUMS.
    assert!(v.verify(Signed::Bytes(MANIFEST), RUNNER_BUNDLE).is_err());
}

fn refused(manifest: &[u8], bundle: &[u8]) -> ErrorCode {
    verify_manifest(&verifier(), manifest, bundle)
        .expect_err("must be refused")
        .code
}

#[test]
fn tampered_manifest_is_refused() {
    let mut m = MANIFEST.to_vec();
    m[3] = if m[3] == b'0' { b'1' } else { b'0' };
    assert_eq!(refused(&m, BUNDLE), ErrorCode::NotVerified);
    let mut m = MANIFEST.to_vec();
    m.extend_from_slice(b"\n");
    assert_eq!(refused(&m, BUNDLE), ErrorCode::NotVerified);
}

#[test]
fn tampered_signature_is_refused() {
    let mut b = json(BUNDLE);
    let sig = b["messageSignature"]["signature"]
        .as_str()
        .unwrap()
        .to_string();
    b["messageSignature"]["signature"] = flip_b64(&sig, 5).into();
    assert!(matches!(
        refused(MANIFEST, &bytes(&b)),
        ErrorCode::NotVerified | ErrorCode::TrustRootStale
    ));
}

#[test]
fn tampered_certificate_is_refused() {
    for at in [40, 200, 900] {
        let mut b = json(BUNDLE);
        let cert = b["verificationMaterial"]["certificate"]["rawBytes"]
            .as_str()
            .unwrap()
            .to_string();
        b["verificationMaterial"]["certificate"]["rawBytes"] = flip_b64(&cert, at).into();
        let code = refused(MANIFEST, &bytes(&b));
        assert!(
            matches!(code, ErrorCode::NotVerified | ErrorCode::TrustRootStale),
            "{at}: {code:?}"
        );
    }
}

#[test]
fn tampered_tlog_is_refused() {
    // Another entry's body.
    let mut b = json(BUNDLE);
    let other =
        json(RUNNER_BUNDLE)["verificationMaterial"]["tlogEntries"][0]["canonicalizedBody"].clone();
    b["verificationMaterial"]["tlogEntries"][0]["canonicalizedBody"] = other;
    assert!(verify_manifest(&verifier(), MANIFEST, &bytes(&b)).is_err());
    // A truncated inclusion proof.
    let mut b = json(BUNDLE);
    b["verificationMaterial"]["tlogEntries"][0]["inclusionProof"]["hashes"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(verify_manifest(&verifier(), MANIFEST, &bytes(&b)).is_err());
    // A moved integrated time (covered by the SET).
    let mut b = json(BUNDLE);
    b["verificationMaterial"]["tlogEntries"][0]["integratedTime"] = "1795589873".into();
    assert!(verify_manifest(&verifier(), MANIFEST, &bytes(&b)).is_err());
    // No tlog entry at all.
    let mut b = json(BUNDLE);
    b["verificationMaterial"]["tlogEntries"] = Value::Array(vec![]);
    assert!(verify_manifest(&verifier(), MANIFEST, &bytes(&b)).is_err());
}

#[test]
fn bundle_shape_is_strict() {
    // Duplicate key.
    let text = std::str::from_utf8(BUNDLE).unwrap();
    let dup = text.replacen(
        "\"mediaType\"",
        "\"mediaType\": \"application/vnd.dev.sigstore.bundle.v0.3+json\", \"mediaType\"",
        1,
    );
    assert_ne!(dup, text);
    assert_eq!(refused(MANIFEST, dup.as_bytes()), ErrorCode::NotVerified);
    // Wrong media type.
    let mut b = json(BUNDLE);
    b["mediaType"] = "application/vnd.dev.sigstore.bundle+json;version=0.2".into();
    assert_eq!(refused(MANIFEST, &bytes(&b)), ErrorCode::NotVerified);
    // Too large.
    let mut big = BUNDLE.to_vec();
    big.resize(64 * 1024 + 1, b' ');
    assert_eq!(refused(MANIFEST, &big), ErrorCode::TooLarge);
    // Not JSON.
    assert_eq!(refused(MANIFEST, b"not json"), ErrorCode::NotVerified);
    // A chain instead of one leaf.
    let mut b = json(BUNDLE);
    let cert = b["verificationMaterial"]["certificate"].clone();
    b["verificationMaterial"]["x509CertificateChain"] = serde_json::json!({"certificates": [cert]});
    assert_eq!(refused(MANIFEST, &bytes(&b)), ErrorCode::NotVerified);
}

#[test]
fn other_trust_roots_are_refused() {
    // A different (real) Sigstore instance's root: GitHub's.
    let github = SigstoreVerifier::from_trusted_root_json(
        sigstore_verify::trust_root::SIGSTORE_GITHUB_TRUSTED_ROOT,
    )
    .unwrap();
    assert!(verify_manifest(&github, MANIFEST, BUNDLE).is_err());
    assert!(SigstoreVerifier::from_trusted_root_json("{}").is_err());
}

#[test]
fn test_bundles_are_refused_by_the_real_verifier() {
    let fake = metro_release::testing::test_bundle(MANIFEST, COMMIT, 1);
    assert!(verify_manifest(&verifier(), MANIFEST, &fake).is_err());
}
