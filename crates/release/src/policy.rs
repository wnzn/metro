//! GitHub claims in the Fulcio leaf certificate, beyond issuer and SAN.
//!
//! sigstore-verify checks the chain, SCT, transparency log, the exact SAN and
//! the issuer. These checks mirror `release_signatures.py::check_cert_extensions`
//! and add the V2 issuer (`.8`) and runner environment (`.11`). Each extension
//! must be present exactly once as a DER UTF8String with the exact value.

use crate::error::{Error, ErrorCode};
use crate::{CALLER, ISSUER, REPO_ID, REPO_URL, SIGNER, SOURCE_REF, is_commit};
use der::Decode;
use der::asn1::{ObjectIdentifier, Utf8StringRef};

/// `1.3.6.1.4.1.57264.1.<n>`: Fulcio's extension arc.
pub fn fulcio_oid(n: u32) -> ObjectIdentifier {
    ObjectIdentifier::new(&format!("1.3.6.1.4.1.57264.1.{n}")).expect("valid Fulcio OID")
}

/// Extract `(oid, extnValue bytes)` from a DER certificate.
pub fn extensions(cert_der: &[u8]) -> Result<Vec<(ObjectIdentifier, Vec<u8>)>, Error> {
    let cert = x509_cert::Certificate::from_der(cert_der)
        .map_err(|_| Error::new(ErrorCode::NotVerified, "signing certificate is malformed"))?;
    Ok(cert
        .tbs_certificate
        .extensions
        .unwrap_or_default()
        .into_iter()
        .map(|e| (e.extn_id, e.extn_value.as_bytes().to_vec()))
        .collect())
}

/// Check the claims; return the release commit (`.10` == `.13` == `.19`).
pub fn check_github_claims(extensions: &[(ObjectIdentifier, Vec<u8>)]) -> Result<String, Error> {
    let value = |n: u32| -> Result<String, Error> {
        let oid = fulcio_oid(n);
        let mut found = extensions.iter().filter(|(id, _)| *id == oid);
        let fail = || {
            Error::new(
                ErrorCode::NotVerified,
                format!("missing or repeated Fulcio extension .{n}"),
            )
        };
        let (_, raw) = found.next().ok_or_else(fail)?;
        if found.next().is_some() {
            return Err(fail());
        }
        let text = Utf8StringRef::from_der(raw).map_err(|_| {
            Error::new(
                ErrorCode::NotVerified,
                format!("Fulcio extension .{n} is not a UTF8String"),
            )
        })?;
        Ok(text.as_str().to_string())
    };
    let exact = |n: u32, want: &str| -> Result<(), Error> {
        if value(n)? == want {
            Ok(())
        } else {
            Err(Error::new(
                ErrorCode::NotVerified,
                format!("wrong value for Fulcio extension .{n}"),
            ))
        }
    };
    exact(8, ISSUER)?;
    exact(9, SIGNER)?;
    exact(11, "github-hosted")?;
    exact(12, REPO_URL)?;
    exact(14, SOURCE_REF)?;
    exact(15, REPO_ID)?;
    exact(18, CALLER)?;
    exact(20, "push")?;
    let commit = value(10)?;
    if !is_commit(&commit) || value(13)? != commit || value(19)? != commit {
        return Err(Error::new(
            ErrorCode::NotVerified,
            "release commit claims disagree",
        ));
    }
    Ok(commit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use der::Encode;

    fn utf8(text: &str) -> Vec<u8> {
        Utf8StringRef::new(text).unwrap().to_der().unwrap()
    }

    const COMMIT: &str = "1a743e59d4ec6c1da85d09d92c99457561f64e55";

    fn good() -> Vec<(ObjectIdentifier, Vec<u8>)> {
        [
            (8, ISSUER),
            (9, SIGNER),
            (10, COMMIT),
            (11, "github-hosted"),
            (12, REPO_URL),
            (13, COMMIT),
            (14, SOURCE_REF),
            (15, REPO_ID),
            (18, CALLER),
            (19, COMMIT),
            (20, "push"),
        ]
        .into_iter()
        .map(|(n, v)| (fulcio_oid(n), utf8(v)))
        .collect()
    }

    fn set(
        mut exts: Vec<(ObjectIdentifier, Vec<u8>)>,
        n: u32,
        raw: Vec<u8>,
    ) -> Vec<(ObjectIdentifier, Vec<u8>)> {
        let oid = fulcio_oid(n);
        for e in exts.iter_mut() {
            if e.0 == oid {
                e.1 = raw.clone();
            }
        }
        exts
    }

    #[test]
    fn synthetic_good() {
        assert_eq!(check_github_claims(&good()).unwrap(), COMMIT);
    }

    #[test]
    fn synthetic_refusals() {
        let missing: Vec<_> = good()
            .into_iter()
            .filter(|(id, _)| *id != fulcio_oid(15))
            .collect();
        assert!(check_github_claims(&missing).is_err(), "missing .15");
        let mut dup = good();
        dup.push((fulcio_oid(15), utf8(REPO_ID)));
        assert!(check_github_claims(&dup).is_err(), "duplicated .15");
        let other_commit = "b".repeat(40);
        let cases = vec![
            ("tag ref", set(good(), 14, utf8("refs/tags/v0.4.0"))),
            ("dispatch", set(good(), 20, utf8("workflow_dispatch"))),
            (".10 != .13", set(good(), 10, utf8(&other_commit))),
            (".19 differs", set(good(), 19, utf8(&other_commit))),
            (
                "caller",
                set(
                    good(),
                    18,
                    utf8(
                        "https://github.com/wnzn/metro/.github/workflows/release.yml@refs/heads/main",
                    ),
                ),
            ),
            (
                "signer",
                set(
                    good(),
                    9,
                    utf8(
                        "https://github.com/wnzn/metro/.github/workflows/release.yml@refs/heads/main",
                    ),
                ),
            ),
            ("issuer", set(good(), 8, utf8("https://example.com"))),
            ("self-hosted", set(good(), 11, utf8("self-hosted"))),
            ("repo id", set(good(), 15, utf8("1467654869"))),
            ("trailing NUL", set(good(), 20, utf8("push\0"))),
            (
                "not UTF8String",
                set(good(), 20, {
                    let mut raw = utf8("push");
                    raw[0] = 0x13; // PrintableString
                    raw
                }),
            ),
            (
                "trailing bytes",
                set(good(), 20, {
                    let mut raw = utf8("push");
                    raw.push(0);
                    raw
                }),
            ),
            ("short commit", {
                let short = &COMMIT[..39];
                set(
                    set(set(good(), 10, utf8(short)), 13, utf8(short)),
                    19,
                    utf8(short),
                )
            }),
            ("uppercase commit", {
                let upper = COMMIT.to_uppercase();
                set(
                    set(set(good(), 10, utf8(&upper)), 13, utf8(&upper)),
                    19,
                    utf8(&upper),
                )
            }),
        ];
        for (label, exts) in cases {
            assert!(check_github_claims(&exts).is_err(), "{label}");
        }
    }

    #[test]
    fn real_040_leaf() {
        let der = crate::sigstore::leaf_der(include_bytes!(
            "../tests/fixtures/v0.4.0/SHA256SUMS.sigstore.json"
        ));
        assert_eq!(
            check_github_claims(&extensions(&der).unwrap()).unwrap(),
            COMMIT
        );
    }
}
