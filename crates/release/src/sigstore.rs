//! The only module that talks to sigstore-verify (pinned `=0.17.0`).

use crate::error::{Error, ErrorCode};
use crate::release::{ReleaseVerifier, Signed, SignedFacts};
use crate::{ISSUER, MAX_BUNDLE, SIGNER, policy};
use sigstore_verify::trust_root::TrustedRoot;
use sigstore_verify::types::{Bundle, Sha256Hash};
use sigstore_verify::{VerificationPolicy, Verifier};

/// Sigstore public-good `trusted_root.json`, vendored (see `trust/README.md`).
/// A test pins its SHA-256, so changing it takes a reviewed change.
pub const VENDORED_TRUSTED_ROOT: &str = include_str!("../trust/sigstore-trusted-root.json");
/// Sigstore public-good TUF `root.json` (v16), the anchor for a trust-root
/// refresh from tuf-repo-cdn.sigstore.dev.
pub const VENDORED_TUF_ROOT: &[u8] = include_bytes!("../trust/sigstore-tuf-root.json");

pub const BUNDLE_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";

pub struct SigstoreVerifier {
    inner: Verifier,
}

impl std::fmt::Debug for SigstoreVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigstoreVerifier")
    }
}

impl SigstoreVerifier {
    /// The vendored trusted root.
    pub fn vendored() -> Result<Self, Error> {
        Self::from_trusted_root_json(VENDORED_TRUSTED_ROOT)
    }

    /// A trusted root obtained some other verified way (a TUF refresh).
    pub fn from_trusted_root_json(json: &str) -> Result<Self, Error> {
        let root = TrustedRoot::from_json(json)
            .map_err(|e| Error::new(ErrorCode::NotVerified, format!("trusted root: {e}")))?;
        let inner = Verifier::new(&root)
            .map_err(|e| Error::new(ErrorCode::NotVerified, format!("trusted root: {e}")))?;
        Ok(SigstoreVerifier { inner })
    }
}

impl ReleaseVerifier for SigstoreVerifier {
    fn verify(&self, artifact: Signed<'_>, bundle_json: &[u8]) -> Result<SignedFacts, Error> {
        let bundle = parse_bundle(bundle_json)?;
        let policy = VerificationPolicy::new(SIGNER, ISSUER);
        let result = match artifact {
            Signed::Bytes(bytes) => self.inner.verify(bytes, &bundle, &policy),
            Signed::Sha256(digest) => self.inner.verify(Sha256Hash::new(digest), &bundle, &policy),
        }
        .map_err(classify)?;
        // Defence in depth: the policy above never relaxes these, but a future
        // crate change must not silently weaken them.
        if !(result.certificate_verified()
            && result.sct_verified()
            && result.tlog_verified()
            && result.identity_policy_checked())
        {
            return Err(Error::new(
                ErrorCode::NotVerified,
                "verification evidence is incomplete",
            ));
        }
        let leaf = bundle.signing_certificate().ok_or_else(|| {
            Error::new(ErrorCode::NotVerified, "bundle has no signing certificate")
        })?;
        let commit = policy::check_github_claims(&policy::extensions(leaf.as_bytes())?)?;
        let signed_at = result
            .integrated_time()
            .or_else(|| result.verified_timestamps().iter().copied().min())
            .map(|t| t.as_second())
            .ok_or_else(|| Error::new(ErrorCode::NotVerified, "no authenticated signing time"))?;
        Ok(SignedFacts { commit, signed_at })
    }
}

/// `trust_root_stale` only when the failure says Metro's trust material lacks
/// something a newer trusted root could supply (Sigstore key rotation): an
/// unknown log or log key, no CA or TSA valid then, or a chain to an issuer
/// the root doesn't know. Anything else (a tampered signature, certificate,
/// log entry or timestamp) is `not_verified`. The texts are those of the
/// pinned sigstore-verify/webpki; tests/sigstore_real.rs and
/// tests/tuf_refresh.rs guard them.
pub(crate) fn is_stale(error: &sigstore_verify::Error) -> bool {
    use sigstore_verify::{CertificateError, Error as E, TimestampError, TransparencyLogError};
    let text = error.to_string();
    match error {
        E::TransparencyLog(TransparencyLogError::UnknownLog { .. }) => true,
        E::TransparencyLog(_) => text.contains("No log key valid"),
        E::Certificate(CertificateError::NoValidAuthority { .. }) => true,
        E::Certificate(CertificateError::ChainInvalid(_)) => text.contains("UnknownIssuer"),
        E::Timestamp(TimestampError::OutsideAuthorityValidity { .. }) => true,
        E::Timestamp(TimestampError::Invalid(_)) => {
            text.contains("no timestamp authorities configured") || text.contains("UnknownIssuer")
        }
        _ => false,
    }
}

fn classify(error: sigstore_verify::Error) -> Error {
    let code = if is_stale(&error) {
        ErrorCode::TrustRootStale
    } else {
        ErrorCode::NotVerified
    };
    Error::new(code, format!("signature: {error}"))
}

/// Parse a bundle under Metro's extra rules: ≤ 64 KiB, no duplicate JSON keys,
/// exactly the v0.3 media type, one leaf certificate (not a chain or key).
pub fn parse_bundle(bundle_json: &[u8]) -> Result<Bundle, Error> {
    let bad = |d: &str| Error::new(ErrorCode::NotVerified, d);
    if bundle_json.len() > MAX_BUNDLE {
        return Err(Error::new(
            ErrorCode::TooLarge,
            "signature bundle is too large",
        ));
    }
    let text = std::str::from_utf8(bundle_json).map_err(|_| bad("bundle is not UTF-8"))?;
    let value: UniqueKeys =
        serde_json::from_str(text).map_err(|_| bad("bundle is not strict JSON"))?;
    let value = value.0;
    if value.get("mediaType").and_then(|m| m.as_str()) != Some(BUNDLE_MEDIA_TYPE) {
        return Err(bad("expected a Sigstore v0.3 bundle"));
    }
    let material = value
        .get("verificationMaterial")
        .and_then(|m| m.as_object());
    if !material.is_some_and(|m| {
        m.get("certificate").is_some_and(|c| c.is_object())
            && !m.contains_key("x509CertificateChain")
            && !m.contains_key("publicKey")
    }) {
        return Err(bad("bundle must carry exactly one leaf certificate"));
    }
    Bundle::from_json(text).map_err(|e| bad(&format!("bundle: {e}")))
}

/// A JSON value that refuses duplicate object keys at any depth (serde_json
/// keeps the last one silently; `release_signatures.py` refuses them too).
struct UniqueKeys(serde_json::Value);

impl<'de> serde::Deserialize<'de> for UniqueKeys {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(UniqueVisitor).map(UniqueKeys)
    }
}

struct UniqueVisitor;

impl<'de> serde::de::Visitor<'de> for UniqueVisitor {
    type Value = serde_json::Value;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("JSON")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
        Ok(v.into())
    }
    fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
        Ok(v.into())
    }
    fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
        Ok(serde_json::Number::from_f64(v).map_or(serde_json::Value::Null, Into::into))
    }
    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E> {
        Ok(v.into())
    }
    fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
        Ok(v.into())
    }
    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut out = Vec::new();
        while let Some(UniqueKeys(v)) = seq.next_element()? {
            out.push(v);
        }
        Ok(out.into())
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut out = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let UniqueKeys(v) = map.next_value()?;
            if out.insert(key, v).is_some() {
                return Err(serde::de::Error::custom("duplicate key"));
            }
        }
        Ok(out.into())
    }
}

#[cfg(test)]
pub(crate) fn leaf_der(bundle_json: &[u8]) -> Vec<u8> {
    parse_bundle(bundle_json)
        .unwrap()
        .signing_certificate()
        .unwrap()
        .as_bytes()
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &[u8] = include_bytes!("../tests/fixtures/v0.4.0/SHA256SUMS");
    const BUNDLE: &[u8] = include_bytes!("../tests/fixtures/v0.4.0/SHA256SUMS.sigstore.json");

    fn with_policy(policy: VerificationPolicy) -> bool {
        let v = SigstoreVerifier::vendored().unwrap();
        let bundle = parse_bundle(BUNDLE).unwrap();
        v.inner.verify(MANIFEST, &bundle, &policy).is_ok()
    }

    #[test]
    fn identity_and_issuer_are_exact() {
        assert!(with_policy(VerificationPolicy::new(SIGNER, ISSUER)));
        let release_yml =
            "https://github.com/wnzn/metro/.github/workflows/release.yml@refs/heads/main";
        assert!(!with_policy(VerificationPolicy::new(release_yml, ISSUER)));
        assert!(!with_policy(VerificationPolicy::new(
            SIGNER,
            "https://accounts.google.com"
        )));
        let tag = "https://github.com/wnzn/metro/.github/workflows/sign-release-assets.yml@refs/tags/v0.4.0";
        assert!(!with_policy(VerificationPolicy::new(tag, ISSUER)));
    }
}
