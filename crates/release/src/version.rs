//! Stable SemVer (`X.Y.Z` only) and the offer policy.

use crate::channel::Channel;
use crate::release::VerifiedRelease;
use std::fmt;

/// A stable release version: no prerelease, no build metadata, no leading
/// zeros, each part ≤ 2^32−1 (the release jobs never get near that).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// Parse `X.Y.Z` strictly; anything else is `None`.
    pub fn parse(text: &str) -> Option<Version> {
        if text.is_empty() || text.len() > 32 {
            return None;
        }
        let mut parts = text.split('.');
        let mut next = || -> Option<u32> {
            let part = parts.next()?;
            if part.is_empty()
                || part.len() > 10
                || !part.bytes().all(|c| c.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                return None;
            }
            part.parse().ok()
        };
        let version = Version {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(version)
    }

    /// This crate's own version (the workspace version of the build).
    pub fn this_build() -> Option<Version> {
        Version::parse(env!("CARGO_PKG_VERSION"))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl serde::Serialize for Version {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Version {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Version::parse(&text).ok_or_else(|| serde::de::Error::custom("not a stable X.Y.Z version"))
    }
}

/// What a verified release means for this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    /// Source or preview build: never offered.
    NotForThisBuild,
    /// Not newer than this build, or older than a release verified before.
    UpToDate,
    /// Newer and verified.
    Available(Version),
}

/// Decide whether `candidate` is offered to a build running `running`.
///
/// `highest_seen` is the highest version this installation ever verified. A
/// replayed older manifest can't be offered and can't hide a newer one.
pub fn offer(
    running: &Version,
    channel: Channel,
    candidate: &VerifiedRelease,
    highest_seen: Option<&Version>,
) -> Offer {
    if channel != Channel::Release {
        return Offer::NotForThisBuild;
    }
    if candidate.version <= *running {
        return Offer::UpToDate;
    }
    if highest_seen.is_some_and(|highest| candidate.version < *highest) {
        return Offer::UpToDate;
    }
    Offer::Available(candidate.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn release(v: &str) -> VerifiedRelease {
        VerifiedRelease {
            evidence: Default::default(),
            version: Version::parse(v).unwrap(),
            commit: "a".repeat(40),
            signed_at: 0,
            files: BTreeMap::new(),
            msi: String::new(),
        }
    }

    #[test]
    fn parse_is_strict() {
        assert_eq!(
            Version::parse("0.4.1"),
            Some(Version {
                major: 0,
                minor: 4,
                patch: 1
            })
        );
        assert_eq!(
            Version::parse("10.20.30").map(|v| v.to_string()),
            Some("10.20.30".into())
        );
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-rc.1",
            "1.0.0+meta",
            "v1.2.3",
            " 1.2.3",
            "1.2.3 ",
            "1..3",
            "1.2.x",
            "99999999999.0.0",
            "1.2.3\n",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert!(Version::parse("0.0.0").is_some());
    }

    #[test]
    fn ordering() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("0.4.10") > v("0.4.9"));
        assert!(v("1.0.0") > v("0.99.99"));
    }

    #[test]
    fn offer_policy() {
        let running = Version::parse("0.4.0").unwrap();
        let r = Channel::Release;
        assert_eq!(
            offer(&running, r, &release("0.4.1"), None),
            Offer::Available(Version::parse("0.4.1").unwrap())
        );
        assert_eq!(offer(&running, r, &release("0.4.0"), None), Offer::UpToDate);
        assert_eq!(offer(&running, r, &release("0.3.9"), None), Offer::UpToDate);
        let seen = Version::parse("0.5.0").unwrap();
        assert_eq!(
            offer(&running, r, &release("0.4.1"), Some(&seen)),
            Offer::UpToDate
        );
        assert_eq!(
            offer(&running, r, &release("0.5.0"), Some(&seen)),
            Offer::Available(seen)
        );
        assert_eq!(
            offer(&running, Channel::Preview, &release("0.4.1"), None),
            Offer::NotForThisBuild
        );
        assert_eq!(
            offer(&running, Channel::Source, &release("9.0.0"), None),
            Offer::NotForThisBuild
        );
    }
}
