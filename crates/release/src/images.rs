//! `IMAGES.txt`: the release's container images, pinned by digest.
//!
//! Parsed only after its SHA-256 matched the signed `SHA256SUMS` row. Rules as
//! `release_signatures.py::image_evidence`.

use crate::error::{Error, ErrorCode, require};
use crate::version::Version;
use crate::{MAX_IMAGES, is_commit};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Images {
    /// `ghcr.io/wnzn/metro-runner:vX.Y.Z@sha256:…` (the tagged multi-platform index).
    pub runner: String,
    pub server: String,
    pub embeddinggemma2: Option<String>,
    pub commit: String,
}

const IMAGE_PLATFORMS: [(&str, &[&str]); 3] = [
    ("metro-runner", &["linux/amd64", "linux/arm64"]),
    ("metro-server", &["linux/amd64", "linux/arm64"]),
    ("metro-embeddinggemma2", &["linux/amd64"]),
];

pub fn parse(raw: &[u8], version: &Version) -> Result<Images, Error> {
    let bad = |d: &str| Error::new(ErrorCode::Manifest, d);
    require(
        raw.len() <= MAX_IMAGES,
        ErrorCode::TooLarge,
        "IMAGES.txt is too large",
    )?;
    require(
        !raw.is_empty() && raw.is_ascii(),
        ErrorCode::Manifest,
        "IMAGES.txt is empty or not ASCII",
    )?;
    let text = std::str::from_utf8(raw).map_err(|_| bad("IMAGES.txt is not text"))?;
    let lines: Vec<&str> = text.lines().collect();
    require(
        lines.len() >= 2 && lines[0] == format!("# Metro container images v{version}"),
        ErrorCode::Manifest,
        "IMAGES.txt is for another version",
    )?;
    let commit = lines[1]
        .strip_prefix("# Source commit: ")
        .filter(|c| is_commit(c))
        .ok_or_else(|| bad("IMAGES.txt must name the source commit"))?;

    let tag = format!("v{version}");
    let mut records: BTreeSet<(String, bool, String)> = BTreeSet::new();
    let mut tagged: Vec<(String, String)> = Vec::new();
    for line in &lines[2..] {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (reference, platforms) = line.split_once("  ").ok_or_else(|| bad("IMAGES.txt row"))?;
        require(
            !platforms.is_empty()
                && platforms.bytes().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'/' || c == b','
                }),
            ErrorCode::Manifest,
            "IMAGES.txt platforms",
        )?;
        let rest = reference
            .strip_prefix("ghcr.io/wnzn/")
            .ok_or_else(|| bad("IMAGES.txt registry"))?;
        let (name_tag, digest) = rest
            .split_once("@sha256:")
            .ok_or_else(|| bad("IMAGES.txt digest"))?;
        require(
            crate::parse_hex32(digest).is_some(),
            ErrorCode::Manifest,
            "IMAGES.txt digest",
        )?;
        let (name, image_tag) = match name_tag.split_once(':') {
            Some((n, t)) => (n, Some(t)),
            None => (name_tag, None),
        };
        require(
            IMAGE_PLATFORMS.iter().any(|(n, _)| *n == name),
            ErrorCode::Manifest,
            "IMAGES.txt names an unexpected image",
        )?;
        require(
            image_tag.is_none_or(|t| t == tag),
            ErrorCode::Manifest,
            "IMAGES.txt contains a wrong tag",
        )?;
        let entry = (name.to_string(), image_tag.is_some(), platforms.to_string());
        require(
            records.insert(entry),
            ErrorCode::Manifest,
            "IMAGES.txt repeats an image reference",
        )?;
        if image_tag.is_some() {
            tagged.push((name.to_string(), reference.to_string()));
        }
    }

    let mut expected = BTreeSet::new();
    for (image, platforms) in IMAGE_PLATFORMS {
        if image == "metro-embeddinggemma2" && !records.iter().any(|r| r.0 == image) {
            continue;
        }
        expected.insert((image.to_string(), true, platforms.join(",")));
        for platform in platforms {
            expected.insert((image.to_string(), false, platform.to_string()));
        }
    }
    require(
        records == expected,
        ErrorCode::Manifest,
        "IMAGES.txt has missing or unexpected image platforms",
    )?;
    let find = |n: &str| {
        tagged
            .iter()
            .find(|(name, _)| name == n)
            .map(|(_, r)| r.clone())
    };
    Ok(Images {
        runner: find("metro-runner").ok_or_else(|| bad("IMAGES.txt lacks the runner"))?,
        server: find("metro-server").ok_or_else(|| bad("IMAGES.txt lacks the server"))?,
        embeddinggemma2: find("metro-embeddinggemma2"),
        commit: commit.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &[u8] = include_bytes!("../tests/fixtures/v0.4.0/IMAGES.txt");
    fn v040() -> Version {
        Version::parse("0.4.0").unwrap()
    }

    #[test]
    fn real_040() {
        let images = parse(REAL, &v040()).unwrap();
        assert_eq!(
            images.runner,
            "ghcr.io/wnzn/metro-runner:v0.4.0@sha256:a3f12f00eea1bea5061eaa370da49108b756465f88fe9eea0f50c6681772d0b6"
        );
        assert!(
            images
                .server
                .starts_with("ghcr.io/wnzn/metro-server:v0.4.0@sha256:2bc9dd1b")
        );
        assert_eq!(images.commit, "1a743e59d4ec6c1da85d09d92c99457561f64e55");
    }

    #[test]
    fn refusals() {
        let text = std::str::from_utf8(REAL).unwrap();
        assert!(parse(REAL, &Version::parse("0.4.1").unwrap()).is_err());
        for (from, to) in [
            ("metro-runner:v0.4.0", "metro-runner:v0.4.1"),
            ("ghcr.io/wnzn/metro-server@", "ghcr.io/evil/metro-server@"),
            ("linux/amd64,linux/arm64", "linux/amd64"),
            ("# Source commit: 1a743e59", "# Source commit: 1A743e59"),
            ("@sha256:a3f12f00", "@sha256:a3f12f0"),
        ] {
            let changed = text.replacen(from, to, 1);
            assert_ne!(changed, text, "{from}");
            assert!(
                parse(changed.as_bytes(), &v040()).is_err(),
                "{from} -> {to}"
            );
        }
        let doubled = format!(
            "{text}ghcr.io/wnzn/metro-runner@sha256:{}  linux/amd64\n",
            "c".repeat(64)
        );
        assert!(parse(doubled.as_bytes(), &v040()).is_err());
    }
}
