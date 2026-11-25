//! `SHA256SUMS`: the signed list of every file in a release.
//!
//! The format and the file set follow
//! `packaging/release-signatures/release_signatures.py` (`manifest_bytes`,
//! `verify_file`): sorted `"<sha256>  <name>\n"` rows, LF only, and exactly
//! the release's archives, MSI, installer scripts, `.sha256` sidecars and
//! `IMAGES.txt`. The only extras accepted are the macOS files that script
//! already anticipates (`MACOS_TARGETS`, `install-metro-macos.sh`), so a
//! future release with macOS archives doesn't lock every installed updater
//! out. Anything else is refused.

use crate::error::{Error, ErrorCode, require};
use crate::version::Version;
use crate::{MAX_MANIFEST, parse_hex32};
use std::collections::BTreeMap;

pub const MANIFEST: &str = "SHA256SUMS";
pub const BUNDLE_SUFFIX: &str = ".sigstore.json";
pub const IMAGES: &str = "IMAGES.txt";

pub const INSTALLERS: [&str; 5] = [
    "install-metro-linux.sh",
    "install-metro-runner-linux.sh",
    "install-metro-windows.ps1",
    "metro-runner-container.ps1",
    "metro-runner-container.sh",
];
const OPTIONAL_INSTALLERS: [&str; 1] = ["install-metro-macos.sh"];
const MACOS_TARGETS: [&str; 2] = ["macos-arm64", "macos-x64"];

/// A parsed (not yet trusted by itself) manifest. Only
/// [`crate::release::verify_manifest`] turns it into a `VerifiedRelease`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub version: Version,
    pub files: BTreeMap<String, [u8; 32]>,
    /// The MSI's exact name (`.msi` when signed, `.unsigned-preview.msi`).
    pub msi: String,
}

pub fn desktop_linux(v: &Version) -> String {
    format!("Metro-{v}-linux-x64.tar.gz")
}
pub fn desktop_windows_zip(v: &Version) -> String {
    format!("Metro-{v}-windows-x64.zip")
}
pub fn runner_linux(v: &Version) -> String {
    format!("Metro-runner-{v}-linux-x64.tar.gz")
}
/// The top folder inside an archive (`Metro-1.2.3-linux-x64`).
pub fn archive_top(archive_name: &str) -> Option<&str> {
    archive_name
        .strip_suffix(".tar.gz")
        .or_else(|| archive_name.strip_suffix(".zip"))
}

fn name_ok(name: &str) -> bool {
    (1..=128).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// The version string a `Metro-runner-<v>-linux-x64.tar.gz` row names.
fn runner_version(name: &str) -> Option<&str> {
    name.strip_prefix("Metro-runner-")?
        .strip_suffix("-linux-x64.tar.gz")
}

/// Parse and check `SHA256SUMS` bytes.
pub fn parse(bytes: &[u8]) -> Result<Manifest, Error> {
    let bad = |detail: &str| Error::new(ErrorCode::Manifest, detail);
    if bytes.len() > MAX_MANIFEST {
        return Err(Error::new(ErrorCode::TooLarge, "SHA256SUMS is too large"));
    }
    require(
        !bytes.is_empty() && bytes.is_ascii(),
        ErrorCode::Manifest,
        "SHA256SUMS is empty or not ASCII",
    )?;
    require(
        bytes.ends_with(b"\n") && !bytes.contains(&b'\r'),
        ErrorCode::Manifest,
        "SHA256SUMS must use LF rows",
    )?;
    let text = std::str::from_utf8(bytes).map_err(|_| bad("SHA256SUMS is not text"))?;

    let mut files = BTreeMap::new();
    let mut previous: Option<&str> = None;
    for line in text[..text.len() - 1].split('\n') {
        let (digest, name) = line
            .split_once("  ")
            .ok_or_else(|| bad("SHA256SUMS row is not '<sha256>  <name>'"))?;
        let digest = parse_hex32(digest).ok_or_else(|| bad("SHA256SUMS row has a bad digest"))?;
        require(
            name_ok(name),
            ErrorCode::Manifest,
            "SHA256SUMS row has a bad file name",
        )?;
        require(
            previous.is_none_or(|p| p < name),
            ErrorCode::Manifest,
            "SHA256SUMS rows are not sorted or repeat a name",
        )?;
        previous = Some(name);
        files.insert(name.to_string(), digest);
    }

    let runners: Vec<&str> = files.keys().filter_map(|n| runner_version(n)).collect();
    require(
        runners.len() == 1,
        ErrorCode::Manifest,
        "SHA256SUMS must name exactly one runner archive",
    )?;
    let version_text = runners[0];
    let version = match Version::parse(version_text) {
        Some(v) => v,
        None => {
            // A prerelease or build metadata is a release we never offer.
            return Err(Error::new(
                ErrorCode::NotOffered,
                "not a stable release version",
            ));
        }
    };

    let v = version.to_string();
    let msi_signed = format!("Metro-{v}-windows-x64.msi");
    let msi_unsigned = format!("Metro-{v}-windows-x64.unsigned-preview.msi");
    let msi = match (
        files.contains_key(&msi_signed),
        files.contains_key(&msi_unsigned),
    ) {
        (true, false) => msi_signed,
        (false, true) => msi_unsigned,
        _ => return Err(bad("SHA256SUMS must name exactly one Windows MSI")),
    };
    let binaries = [
        desktop_linux(&version),
        desktop_windows_zip(&version),
        runner_linux(&version),
        msi.clone(),
    ];
    let mut required: Vec<String> = binaries.to_vec();
    required.extend(binaries.iter().map(|b| format!("{b}.sha256")));
    required.extend(INSTALLERS.iter().map(|s| s.to_string()));
    required.push(IMAGES.to_string());
    for name in &required {
        require(
            files.contains_key(name),
            ErrorCode::Manifest,
            "SHA256SUMS lacks a release file",
        )?;
    }
    let mut optional: Vec<String> = OPTIONAL_INSTALLERS.iter().map(|s| s.to_string()).collect();
    for target in MACOS_TARGETS {
        let archive = format!("Metro-{v}-{target}.tar.gz");
        optional.push(format!("{archive}.sha256"));
        optional.push(archive);
    }
    for name in files.keys() {
        require(
            required.contains(name) || optional.contains(name),
            ErrorCode::Manifest,
            "SHA256SUMS names a file that isn't part of a release",
        )?;
    }
    Ok(Manifest {
        version,
        files,
        msi,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &[u8] = include_bytes!("../tests/fixtures/v0.4.0/SHA256SUMS");

    fn rows(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }
    fn join(rows: &[String]) -> Vec<u8> {
        let mut out = rows.join("\n");
        out.push('\n');
        out.into_bytes()
    }
    fn real_rows() -> Vec<String> {
        rows(std::str::from_utf8(REAL).unwrap())
    }

    #[test]
    fn real_040_manifest() {
        let m = parse(REAL).unwrap();
        assert_eq!(m.version.to_string(), "0.4.0");
        assert_eq!(m.msi, "Metro-0.4.0-windows-x64.unsigned-preview.msi");
        assert_eq!(m.files.len(), 14);
        assert_eq!(
            crate::hex(&m.files["Metro-runner-0.4.0-linux-x64.tar.gz"]),
            "0b854daa06511982f6ce846b8b669a05954623dc7897960cff37ac14ca0f5ae1"
        );
    }

    fn refused(bytes: &[u8]) -> ErrorCode {
        parse(bytes).unwrap_err().code
    }

    #[test]
    fn format_refusals() {
        let text = std::str::from_utf8(REAL).unwrap();
        assert_eq!(
            refused(text.replace('\n', "\r\n").as_bytes()),
            ErrorCode::Manifest
        );
        assert_eq!(refused(text.trim_end().as_bytes()), ErrorCode::Manifest);
        assert_eq!(
            refused(text.replacen("  IMAGES", " *IMAGES", 1).as_bytes()),
            ErrorCode::Manifest
        );
        assert_eq!(
            refused(text.replacen("  IMAGES", "  *IMAGES", 1).as_bytes()),
            ErrorCode::Manifest
        );
        assert_eq!(
            refused(text.replacen("f422", "F422", 1).as_bytes()),
            ErrorCode::Manifest
        );
        assert_eq!(refused(b""), ErrorCode::Manifest);
        assert_eq!(refused(&vec![b'a'; MAX_MANIFEST + 1]), ErrorCode::TooLarge);
        let mut r = real_rows();
        r.swap(0, 1);
        assert_eq!(refused(&join(&r)), ErrorCode::Manifest, "unsorted");
        let mut r = real_rows();
        let dup = r[3].clone();
        r.insert(3, dup);
        assert_eq!(refused(&join(&r)), ErrorCode::Manifest, "duplicate");
        let mut r = real_rows();
        r[0] = format!("{}  ../IMAGES.txt", "0".repeat(64));
        assert_eq!(refused(&join(&r)), ErrorCode::Manifest, "path");
    }

    #[test]
    fn set_refusals() {
        // A missing MSI.
        let r: Vec<String> = real_rows()
            .into_iter()
            .filter(|l| !l.ends_with(".msi"))
            .collect();
        assert_eq!(refused(&join(&r)), ErrorCode::Manifest);
        // Both MSI names.
        let mut r = real_rows();
        r.push(format!("{}  Metro-0.4.0-windows-x64.msi", "1".repeat(64)));
        r.sort_by(|a, b| a[66..].cmp(&b[66..]));
        assert_eq!(refused(&join(&r)), ErrorCode::Manifest);
        // A second version.
        let mut r = real_rows();
        r.push(format!(
            "{}  Metro-runner-0.4.1-linux-x64.tar.gz",
            "1".repeat(64)
        ));
        r.sort_by(|a, b| a[66..].cmp(&b[66..]));
        assert_eq!(refused(&join(&r)), ErrorCode::Manifest);
        // An unknown extra file.
        let mut r = real_rows();
        r.push(format!("{}  notes.txt", "1".repeat(64)));
        r.sort_by(|a, b| a[66..].cmp(&b[66..]));
        assert_eq!(refused(&join(&r)), ErrorCode::Manifest);
        // Another version's desktop archive next to this runner.
        let r: Vec<String> = real_rows()
            .into_iter()
            .map(|l| {
                l.replace(
                    "Metro-0.4.0-linux-x64.tar.gz",
                    "Metro-0.4.1-linux-x64.tar.gz",
                )
            })
            .collect();
        assert_eq!(refused(&join(&r)), ErrorCode::Manifest);
        // A missing installer.
        let r: Vec<String> = real_rows()
            .into_iter()
            .filter(|l| !l.ends_with("install-metro-linux.sh"))
            .collect();
        assert_eq!(refused(&join(&r)), ErrorCode::Manifest);
    }

    #[test]
    fn prerelease_not_offered() {
        let r: Vec<String> = real_rows()
            .into_iter()
            .map(|l| l.replace("0.4.0", "0.5.0-rc.1"))
            .collect();
        assert_eq!(refused(&join(&r)), ErrorCode::NotOffered);
        let r: Vec<String> = real_rows()
            .into_iter()
            .map(|l| l.replace("0.4.0", "0.5.0-ci"))
            .collect();
        assert_eq!(refused(&join(&r)), ErrorCode::NotOffered);
    }

    #[test]
    fn macos_extras_accepted() {
        let mut r = real_rows();
        r.push(format!(
            "{}  Metro-0.4.0-macos-arm64.tar.gz",
            "1".repeat(64)
        ));
        r.push(format!(
            "{}  Metro-0.4.0-macos-arm64.tar.gz.sha256",
            "2".repeat(64)
        ));
        r.push(format!("{}  install-metro-macos.sh", "3".repeat(64)));
        r.sort_by(|a, b| a[66..].cmp(&b[66..]));
        assert!(parse(&join(&r)).is_ok());
    }

    #[test]
    fn signed_msi_name() {
        let r: Vec<String> = real_rows()
            .into_iter()
            .map(|l| l.replace(".unsigned-preview.msi", ".msi"))
            .collect();
        let mut r = r;
        r.sort_by(|a, b| a[66..].cmp(&b[66..]));
        assert_eq!(parse(&join(&r)).unwrap().msi, "Metro-0.4.0-windows-x64.msi");
    }
}
