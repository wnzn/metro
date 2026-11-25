//! Test doubles (feature `testing`, enabled only from dev-dependencies).
//!
//! [`TestVerifier`] accepts only bundles made by [`test_bundle`]: a JSON
//! object naming the artifact's SHA-256, a commit and a time. It exists so
//! flow tests (desktop updater, server, runner self-update) can exercise
//! every non-crypto step with fixture releases. The real Sigstore path is
//! tested on the published 0.4.0 bundle. No shipped code constructs it, and
//! [`crate::sigstore::SigstoreVerifier`] refuses its bundles (tested).

use crate::error::{Error, ErrorCode};
use crate::release::{ReleaseVerifier, Signed, SignedFacts};
use crate::version::Version;
use crate::{hex, sha256};
use std::collections::BTreeMap;

pub const TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

pub struct TestVerifier;

pub fn test_bundle(artifact: &[u8], commit: &str, signed_at: i64) -> Vec<u8> {
    serde_json::json!({
        "metroTestBundle": 1,
        "sha256": hex(&sha256(artifact)),
        "commit": commit,
        "signedAt": signed_at,
    })
    .to_string()
    .into_bytes()
}

impl ReleaseVerifier for TestVerifier {
    fn verify(&self, artifact: Signed<'_>, bundle_json: &[u8]) -> Result<SignedFacts, Error> {
        let fail = |d: &str| Error::new(ErrorCode::NotVerified, d);
        let v: serde_json::Value =
            serde_json::from_slice(bundle_json).map_err(|_| fail("test bundle"))?;
        if v.get("metroTestBundle").and_then(|x| x.as_u64()) != Some(1) {
            return Err(fail("not a test bundle"));
        }
        let digest = match artifact {
            Signed::Bytes(b) => sha256(b),
            Signed::Sha256(d) => d,
        };
        if v.get("sha256").and_then(|x| x.as_str()) != Some(hex(&digest).as_str()) {
            return Err(fail("test signature doesn't match"));
        }
        let commit = v
            .get("commit")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string();
        if !crate::is_commit(&commit) {
            return Err(fail("test commit"));
        }
        Ok(SignedFacts {
            commit,
            signed_at: v.get("signedAt").and_then(|x| x.as_i64()).unwrap_or(0),
        })
    }
}

/// A complete fixture release: file name → bytes, including `SHA256SUMS` and
/// its test bundle. Payloads not given are small placeholders.
pub struct FixtureRelease {
    pub version: Version,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl FixtureRelease {
    /// `overrides`: real payloads (archives, scripts) by name.
    pub fn new(version: &str, overrides: BTreeMap<String, Vec<u8>>) -> FixtureRelease {
        let version = Version::parse(version).expect("fixture version");
        let v = version.to_string();
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let binaries = [
            format!("Metro-{v}-linux-x64.tar.gz"),
            format!("Metro-{v}-windows-x64.zip"),
            format!("Metro-{v}-windows-x64.unsigned-preview.msi"),
            format!("Metro-runner-{v}-linux-x64.tar.gz"),
        ];
        for name in binaries.iter().chain(
            crate::manifest::INSTALLERS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .iter(),
        ) {
            let body = overrides
                .get(name)
                .cloned()
                .unwrap_or_else(|| format!("placeholder {name}\n").into_bytes());
            files.insert(name.clone(), body);
        }
        for name in &binaries {
            let sidecar = format!("{}  {name}\n", hex(&sha256(&files[name])));
            files.insert(format!("{name}.sha256"), sidecar.into_bytes());
        }
        let images = overrides
            .get("IMAGES.txt")
            .cloned()
            .unwrap_or_else(|| fixture_images(&version).into_bytes());
        files.insert("IMAGES.txt".into(), images);
        let manifest: String = files
            .iter()
            .map(|(n, b)| format!("{}  {n}\n", hex(&sha256(b))))
            .collect();
        files.insert("SHA256SUMS".into(), manifest.into_bytes());
        let bundle = test_bundle(&files["SHA256SUMS"], TEST_COMMIT, 1_800_000_000);
        files.insert("SHA256SUMS.sigstore.json".into(), bundle);
        for name in [&binaries[0], &binaries[3]] {
            let bundle = test_bundle(&files[name], TEST_COMMIT, 1_800_000_000);
            files.insert(format!("{name}.sigstore.json"), bundle);
        }
        FixtureRelease { version, files }
    }
}

pub fn fixture_images(version: &Version) -> String {
    let d = |c: char| c.to_string().repeat(64);
    format!(
        "# Metro container images v{version}\n# Source commit: {TEST_COMMIT}\n\n\
ghcr.io/wnzn/metro-runner:v{version}@sha256:{}  linux/amd64,linux/arm64\n\
ghcr.io/wnzn/metro-runner@sha256:{}  linux/amd64\n\
ghcr.io/wnzn/metro-runner@sha256:{}  linux/arm64\n\n\
ghcr.io/wnzn/metro-server:v{version}@sha256:{}  linux/amd64,linux/arm64\n\
ghcr.io/wnzn/metro-server@sha256:{}  linux/amd64\n\
ghcr.io/wnzn/metro-server@sha256:{}  linux/arm64\n",
        d('a'),
        d('b'),
        d('c'),
        d('d'),
        d('e'),
        d('f')
    )
}

/// Build a USTAR tar.gz like `package.py` (regular files under `top/`).
pub fn tar_gz(top: &str, files: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (name, data, mode) in files {
        tar.extend_from_slice(&ustar_header(
            &format!("{top}/{name}"),
            data.len() as u64,
            *mode,
            b'0',
        ));
        tar.extend_from_slice(data);
        tar.resize(tar.len().div_ceil(512) * 512, 0);
    }
    tar.extend_from_slice(&[0u8; 1024]);
    gzip(&tar)
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(data).expect("gzip");
    gz.finish().expect("gzip")
}

/// One raw USTAR header (for tests that need odd entries).
pub fn ustar_header(name: &str, size: u64, mode: u32, typeflag: u8) -> [u8; 512] {
    let mut h = [0u8; 512];
    let (prefix, base) = if name.len() > 100 {
        let cut = name[..name.len() - 100].len() + name[name.len() - 100..].find('/').unwrap_or(0);
        (&name[..cut], &name[cut + 1..])
    } else {
        ("", name)
    };
    h[..base.len()].copy_from_slice(base.as_bytes());
    h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[156] = typeflag;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

/// `RELEASE.json` bytes for a file set.
pub fn release_json(
    kind: &str,
    target: &str,
    version: &str,
    files: &[(&str, &[u8], u32)],
) -> Vec<u8> {
    let listed: BTreeMap<String, String> = files
        .iter()
        .map(|(n, d, _)| (n.to_string(), hex(&sha256(d))))
        .collect();
    serde_json::to_vec_pretty(&serde_json::json!({
        "kind": kind, "target": target, "version": version, "files": listed,
    }))
    .expect("json")
}

/// A complete runner archive (`Metro-runner-<v>-linux-x64.tar.gz`).
pub fn runner_archive(version: &str, metro_runner: &[u8]) -> Vec<u8> {
    let run = b"#!/bin/sh\nexec \"$(dirname \"$0\")/metro-runner\" \"$@\"\n".to_vec();
    let start = b"Start the runner with run-runner.\n".to_vec();
    let payload: Vec<(&str, &[u8], u32)> = vec![
        ("START.txt", &start, 0o644),
        ("metro-runner", metro_runner, 0o755),
        ("run-runner", &run, 0o755),
    ];
    let rj = release_json("runner", "linux-x64", version, &payload);
    let mut all = vec![("RELEASE.json", rj.as_slice(), 0o644)];
    all.extend(payload);
    tar_gz(&format!("Metro-runner-{version}-linux-x64"), &all)
}
