//! From signed bytes to a `VerifiedRelease`.

use crate::error::{Error, ErrorCode, require};
use crate::manifest::{self, Manifest};
use crate::version::Version;
use std::collections::BTreeMap;

/// What was signed: the bytes themselves, or their SHA-256 (large archives,
/// hashed while downloading).
#[derive(Clone, Copy, Debug)]
pub enum Signed<'a> {
    Bytes(&'a [u8]),
    Sha256([u8; 32]),
}

/// Facts a verified signature establishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedFacts {
    /// The release commit from the certificate's claims.
    pub commit: String,
    /// Authenticated signing time (transparency log or TSA), Unix seconds.
    pub signed_at: i64,
}

/// Verifies one Sigstore bundle over one artifact against Metro's release
/// identity. The production implementation is [`crate::sigstore::SigstoreVerifier`];
/// tests inject a double (feature `testing`), which no shipped binary uses.
pub trait ReleaseVerifier: Send + Sync {
    fn verify(&self, artifact: Signed<'_>, bundle_json: &[u8]) -> Result<SignedFacts, Error>;
}

/// A release whose `SHA256SUMS` verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedRelease {
    /// The exact signed manifest and bundle that authenticated this release.
    pub evidence: Evidence,
    pub version: Version,
    pub commit: String,
    pub signed_at: i64,
    pub files: BTreeMap<String, [u8; 32]>,
    pub msi: String,
}

/// Persisted evidence is untrusted until verified against a release verifier.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub manifest: Vec<u8>,
    pub bundle: Vec<u8>,
}

impl Evidence {
    pub fn verify(&self, verifier: &dyn ReleaseVerifier) -> Result<VerifiedRelease, Error> {
        verify_manifest(verifier, &self.manifest, &self.bundle)
    }
}

impl VerifiedRelease {
    /// The signed SHA-256 of a release file.
    pub fn sha256_of(&self, name: &str) -> Result<[u8; 32], Error> {
        self.files
            .get(name)
            .copied()
            .ok_or_else(|| Error::new(ErrorCode::Inconsistent, "file isn't in the signed release"))
    }

    pub fn desktop_linux(&self) -> String {
        manifest::desktop_linux(&self.version)
    }
    pub fn desktop_windows_zip(&self) -> String {
        manifest::desktop_windows_zip(&self.version)
    }
    pub fn runner_linux(&self) -> String {
        manifest::runner_linux(&self.version)
    }

    /// Check bytes against their signed row.
    pub fn check_bytes(&self, name: &str, bytes: &[u8]) -> Result<(), Error> {
        self.check_digest(name, &crate::sha256(bytes))
    }

    pub fn check_digest(&self, name: &str, digest: &[u8; 32]) -> Result<(), Error> {
        require(
            self.sha256_of(name)? == *digest,
            ErrorCode::NotVerified,
            "a downloaded file doesn't match the signed SHA256SUMS",
        )
    }

    /// A Linux archive also carries its own bundle: verify it over the
    /// (already hashed) archive, by the same identity and the same commit.
    pub fn check_archive_signature(
        &self,
        verifier: &dyn ReleaseVerifier,
        name: &str,
        digest: &[u8; 32],
        bundle_json: &[u8],
    ) -> Result<(), Error> {
        self.check_digest(name, digest)?;
        let facts = verifier.verify(Signed::Sha256(*digest), bundle_json)?;
        require(
            facts.commit == self.commit,
            ErrorCode::Inconsistent,
            "archive signature is for another release commit",
        )
    }
}

/// Verify `SHA256SUMS` with its bundle, then parse it.
pub fn verify_manifest(
    verifier: &dyn ReleaseVerifier,
    manifest_bytes: &[u8],
    bundle_json: &[u8],
) -> Result<VerifiedRelease, Error> {
    require(
        manifest_bytes.len() <= crate::MAX_MANIFEST,
        ErrorCode::TooLarge,
        "SHA256SUMS is too large",
    )?;
    require(
        bundle_json.len() <= crate::MAX_BUNDLE,
        ErrorCode::TooLarge,
        "release bundle is too large",
    )?;
    let facts = verifier.verify(Signed::Bytes(manifest_bytes), bundle_json)?;
    let Manifest {
        version,
        files,
        msi,
    } = manifest::parse(manifest_bytes)?;
    Ok(VerifiedRelease {
        evidence: Evidence {
            manifest: manifest_bytes.to_vec(),
            bundle: bundle_json.to_vec(),
        },
        version,
        commit: facts.commit,
        signed_at: facts.signed_at,
        files,
        msi,
    })
}
