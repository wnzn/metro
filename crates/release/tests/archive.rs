//! Archive extraction rules on in-memory tar.gz and zip fixtures.

use metro_release::archive::{Expect, extract_tar_gz, extract_zip};
use metro_release::testing::{gzip, release_json, tar_gz, ustar_header};
use metro_release::{ErrorCode, Version};
use std::io::Write;

const TOP: &str = "Metro-runner-0.5.0-linux-x64";

fn v() -> Version {
    Version::parse("0.5.0").unwrap()
}
fn expect(v: &Version) -> Expect<'_> {
    Expect {
        kind: "runner",
        target: "linux-x64",
        version: v,
    }
}

fn payload() -> Vec<(&'static str, &'static [u8], u32)> {
    vec![
        ("metro-runner", b"\x7fELF fake", 0o755),
        ("run-runner", b"#!/bin/sh\n", 0o755),
        ("licenses/THIRD-PARTY-NOTICES.txt", b"notices\n", 0o644),
    ]
}

fn with_release(files: &[(&'static str, &'static [u8], u32)], rj: Vec<u8>) -> Vec<u8> {
    let rj: &'static [u8] = Box::leak(rj.into_boxed_slice());
    let mut all = vec![("RELEASE.json", rj, 0o644)];
    all.extend_from_slice(files);
    tar_gz(TOP, &all)
}

fn good_archive() -> Vec<u8> {
    with_release(
        &payload(),
        release_json("runner", "linux-x64", "0.5.0", &payload()),
    )
}

fn extract(bytes: &[u8]) -> Result<metro_release::archive::VerifiedTree, metro_release::Error> {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("stage");
    std::fs::create_dir(&dest).unwrap();
    let v = v();
    let r = extract_tar_gz(bytes, TOP, &dest, &expect(&v));
    // Keep the tempdir alive until here.
    drop(dir);
    r
}

#[test]
fn happy_path_keeps_modes() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("stage");
    std::fs::create_dir(&dest).unwrap();
    let v = v();
    let tree = extract_tar_gz(good_archive().as_slice(), TOP, &dest, &expect(&v)).unwrap();
    assert_eq!(tree.files.len(), 3);
    assert_eq!(
        std::fs::read(dest.join("licenses/THIRD-PARTY-NOTICES.txt")).unwrap(),
        b"notices\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &str| {
            std::fs::metadata(dest.join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("metro-runner"), 0o755);
        assert_eq!(mode("run-runner"), 0o755);
        assert_eq!(mode("RELEASE.json"), 0o644);
    }
}

fn refused(bytes: &[u8]) -> ErrorCode {
    extract(bytes).expect_err("must be refused").code
}

fn raw_tar(entries: &[([u8; 512], &[u8])]) -> Vec<u8> {
    let mut tar = Vec::new();
    for (h, data) in entries {
        tar.extend_from_slice(h);
        tar.extend_from_slice(data);
        tar.resize(tar.len().div_ceil(512) * 512, 0);
    }
    tar.extend_from_slice(&[0u8; 1024]);
    gzip(&tar)
}

#[test]
fn special_entries_are_refused() {
    for flag in *b"123456xgL" {
        let h = ustar_header(&format!("{TOP}/link"), 0, 0o644, flag);
        assert_eq!(
            refused(&raw_tar(&[(h, b"")])),
            ErrorCode::Archive,
            "typeflag {}",
            flag as char
        );
    }
}

#[test]
fn bad_paths_are_refused() {
    for name in [
        format!("{TOP}/../x"),
        format!("{TOP}/./x"),
        format!("{TOP}//x"),
        "/etc/passwd".to_string(),
        "other-top/x".to_string(),
        format!("{TOP}x/y"),
        format!("{TOP}/C:x"),
        format!("{TOP}/a\\b"),
        TOP.to_string(),
    ] {
        let h = ustar_header(&name, 1, 0o644, b'0');
        assert_eq!(
            refused(&raw_tar(&[(h, b"x")])),
            ErrorCode::Archive,
            "{name}"
        );
    }
}

#[test]
fn release_json_must_match() {
    // Extra unlisted file.
    let mut more = payload();
    more.push(("extra", b"x", 0o644));
    assert_eq!(
        refused(&with_release(
            &more,
            release_json("runner", "linux-x64", "0.5.0", &payload())
        )),
        ErrorCode::Archive
    );
    // Missing listed file.
    let fewer: Vec<_> = payload().into_iter().skip(1).collect();
    assert_eq!(
        refused(&with_release(
            &fewer,
            release_json("runner", "linux-x64", "0.5.0", &payload())
        )),
        ErrorCode::Archive
    );
    // Hash mismatch.
    let mut changed = payload();
    changed[0].1 = b"\x7fELF other";
    assert_eq!(
        refused(&with_release(
            &changed,
            release_json("runner", "linux-x64", "0.5.0", &payload())
        )),
        ErrorCode::NotVerified
    );
    // Kind, target, version.
    for (k, t, ver) in [
        ("desktop", "linux-x64", "0.5.0"),
        ("runner", "windows-x64", "0.5.0"),
        ("runner", "linux-x64", "0.5.1"),
    ] {
        assert_eq!(
            refused(&with_release(
                &payload(),
                release_json(k, t, ver, &payload())
            )),
            ErrorCode::Archive,
            "{k} {t} {ver}"
        );
    }
    // No RELEASE.json.
    assert_eq!(refused(&tar_gz(TOP, &payload())), ErrorCode::Archive);
    // RELEASE.json not JSON.
    assert_eq!(
        refused(&with_release(&payload(), b"nope".to_vec())),
        ErrorCode::Archive
    );
}

#[test]
fn duplicates_and_corruption_are_refused() {
    let h = ustar_header(&format!("{TOP}/a"), 1, 0o644, b'0');
    assert_eq!(
        refused(&raw_tar(&[(h, b"x"), (h, b"y")])),
        ErrorCode::Archive
    );
    let mut bad = ustar_header(&format!("{TOP}/a"), 1, 0o644, b'0');
    bad[0] ^= 1; // checksum no longer matches
    assert_eq!(refused(&raw_tar(&[(bad, b"x")])), ErrorCode::Archive);
    let mut gnu = ustar_header(&format!("{TOP}/a"), 1, 0o644, b'0');
    gnu[257..265].copy_from_slice(b"ustar  \0");
    assert_eq!(refused(&raw_tar(&[(gnu, b"x")])), ErrorCode::Archive);
    let good = good_archive();
    assert_eq!(refused(&good[..good.len() / 2]), ErrorCode::Archive);
    assert_eq!(refused(b"not gzip"), ErrorCode::Archive);
}

#[test]
fn size_caps() {
    // A header claiming more than the cap.
    let mut h = ustar_header(&format!("{TOP}/big"), 0, 0o644, b'0');
    h[124..136].copy_from_slice(b"77777777777\0");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    assert_eq!(refused(&raw_tar(&[(h, b"")])), ErrorCode::TooLarge);
}

#[test]
fn non_empty_destination_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("x"), b"").unwrap();
    let v = v();
    assert!(extract_tar_gz(good_archive().as_slice(), TOP, dir.path(), &expect(&v)).is_err());
}

// ---- zip ----

#[derive(Clone, Copy)]
struct ZipEntry<'a> {
    name: &'a str,
    data: &'a [u8],
    mode: u32,
    deflate: bool,
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

fn zip(entries: &[ZipEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for e in entries {
        let body = if e.deflate {
            let mut d = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
            d.write_all(e.data).unwrap();
            d.finish().unwrap()
        } else {
            e.data.to_vec()
        };
        let method: u16 = if e.deflate { 8 } else { 0 };
        let offset = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&crc32(e.data).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(&body);
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&[20, 3]); // made by: unix
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&method.to_le_bytes());
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&crc32(e.data).to_le_bytes());
        central.extend_from_slice(&(body.len() as u32).to_le_bytes());
        central.extend_from_slice(&(e.data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 8]); // extra, comment, disk, internal
        central.extend_from_slice(&(e.mode << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(e.name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

const ZTOP: &str = "Metro-0.5.0-windows-x64";

fn zip_extract(bytes: &[u8]) -> Result<metro_release::archive::VerifiedTree, metro_release::Error> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.zip");
    std::fs::write(&path, bytes).unwrap();
    let dest = dir.path().join("stage");
    std::fs::create_dir(&dest).unwrap();
    let v = v();
    let mut file = std::fs::File::open(&path).unwrap();
    extract_zip(
        &mut file,
        ZTOP,
        &dest,
        &Expect {
            kind: "desktop",
            target: "windows-x64",
            version: &v,
        },
    )
}

type Files = Vec<(&'static str, &'static [u8], u32)>;

fn zip_files() -> (Vec<u8>, Files) {
    let files: Vec<(&'static str, &'static [u8], u32)> = vec![
        ("Metro.exe", b"MZ launcher", 0o755),
        ("ui/app.js", b"console.log(1)", 0o644),
    ];
    (
        release_json("desktop", "windows-x64", "0.5.0", &files),
        files,
    )
}

fn names(files: &[(&'static str, &'static [u8], u32)]) -> Vec<String> {
    files
        .iter()
        .map(|(n, _, _)| format!("{ZTOP}/{n}"))
        .collect()
}

#[test]
fn zip_happy_path_and_refusals() {
    let (rj, files) = zip_files();
    let n = names(&files);
    let rj_name = format!("{ZTOP}/RELEASE.json");
    let mut entries = vec![ZipEntry {
        name: &rj_name,
        data: &rj,
        mode: 0o100644,
        deflate: true,
    }];
    for (i, (_, d, m)) in files.iter().enumerate() {
        entries.push(ZipEntry {
            name: &n[i],
            data: d,
            mode: 0o100000 | m,
            deflate: i % 2 == 1,
        });
    }
    let good = zip(&entries);
    let tree = zip_extract(&good).unwrap();
    assert_eq!(tree.files.len(), 2);

    // A symlink entry.
    let mut bad_entries: Vec<ZipEntry> = entries.to_vec();
    bad_entries[1].mode = 0o120777;
    assert_eq!(
        zip_extract(&zip(&bad_entries)).unwrap_err().code,
        ErrorCode::Archive
    );
    // A path escaping the top folder.
    let mut bad_entries: Vec<ZipEntry> = entries.to_vec();
    let escape = format!("{ZTOP}/../evil.exe");
    bad_entries[1].name = &escape;
    assert_eq!(
        zip_extract(&zip(&bad_entries)).unwrap_err().code,
        ErrorCode::Archive
    );
    // A data byte flipped (CRC).
    let mut corrupt = good.clone();
    let at = corrupt
        .windows(11)
        .position(|w| w == b"MZ launcher")
        .unwrap();
    corrupt[at] ^= 1;
    assert!(zip_extract(&corrupt).is_err());
    // Truncated.
    assert!(zip_extract(&good[..good.len() - 5]).is_err());
}

#[test]
fn windows_aliases_are_refused() {
    for name in [
        "CON", "nul.txt", "Aux.json", "com1", "LPT9.log", "COM¹", "conin$", "file.", "file ",
        "dir./x", "a /b",
    ] {
        let h = ustar_header(&format!("{TOP}/{name}"), 1, 0o644, b'0');
        assert_eq!(
            refused(&raw_tar(&[(h, b"x")])),
            ErrorCode::Archive,
            "{name}"
        );
    }
    // Look-alikes that are fine.
    for name in ["CONFIG", "console.txt", "COM10", "LPT", "nul_x"] {
        let h = ustar_header(&format!("{TOP}/{name}"), 1, 0o644, b'0');
        // Not refused for the name: it fails later only because RELEASE.json is missing.
        let e = extract(&raw_tar(&[(h, b"x")])).unwrap_err();
        assert!(e.detail.contains("RELEASE.json"), "{name}: {e}");
    }
}

#[test]
fn trailing_padding_is_bounded() {
    let raw = |pad: usize| {
        let mut files = payload();
        let rj = release_json("runner", "linux-x64", "0.5.0", &payload());
        let rj: &'static [u8] = Box::leak(rj.into_boxed_slice());
        files.insert(0, ("RELEASE.json", rj, 0o644));
        let mut tar = Vec::new();
        for (n, d, m) in &files {
            tar.extend_from_slice(&ustar_header(
                &format!("{TOP}/{n}"),
                d.len() as u64,
                *m,
                b'0',
            ));
            tar.extend_from_slice(d);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
        tar.extend_from_slice(&[0u8; 1024]);
        tar.resize(tar.len() + pad, 0);
        gzip(&tar)
    };
    assert!(extract(&raw(10 * 1024)).is_ok(), "Python's record padding");
    assert_eq!(refused(&raw(512 * 1024)), ErrorCode::Archive);
}

#[test]
fn verified_extraction_is_bound_to_the_bytes_read() {
    let dir = tempfile::tempdir().unwrap();
    let archive = good_archive();
    let path = dir.path().join("a.tar.gz");
    std::fs::write(&path, &archive).unwrap();
    let v = v();
    let good = metro_release::sha256(&archive);
    let dest = dir.path().join("ok");
    std::fs::create_dir(&dest).unwrap();
    metro_release::archive::extract_tar_gz_verified(
        std::fs::File::open(&path).unwrap(),
        &good,
        TOP,
        &dest,
        &expect(&v),
    )
    .unwrap();
    let mut wrong = good;
    wrong[0] ^= 1;
    let dest = dir.path().join("bad");
    std::fs::create_dir(&dest).unwrap();
    let e = metro_release::archive::extract_tar_gz_verified(
        std::fs::File::open(&path).unwrap(),
        &wrong,
        TOP,
        &dest,
        &expect(&v),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::NotVerified);
    // Private copies too.
    let mut src = std::fs::File::open(&path).unwrap();
    let mut copy =
        metro_release::archive::private_copy_verified(&mut src, &good, &dir.path().join("copy"))
            .unwrap();
    let mut back = Vec::new();
    std::io::Read::read_to_end(&mut copy, &mut back).unwrap();
    assert_eq!(back, archive);
    assert!(
        metro_release::archive::private_copy_verified(&mut src, &wrong, &dir.path().join("copy2"))
            .is_err()
    );
    assert!(
        metro_release::archive::private_copy_verified(&mut src, &good, &dir.path().join("copy"))
            .is_err(),
        "never replaces"
    );
}

#[test]
fn staged_trees_are_checked_against_the_authentic_archive() {
    use metro_release::archive::{inspect_tar_gz, verify_tree};
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("stage");
    std::fs::create_dir(&dest).unwrap();
    let v = v();
    let archive = good_archive();
    let extracted = extract_tar_gz(archive.as_slice(), TOP, &dest, &expect(&v)).unwrap();
    let authentic = inspect_tar_gz(archive.as_slice(), TOP, &expect(&v)).unwrap();
    assert_eq!(extracted.files, authentic.files);
    assert_eq!(extracted.release_json_sha256, authentic.release_json_sha256);
    verify_tree(&dest, &authentic, &["enable-local-runner"]).unwrap();
    std::fs::write(dest.join("enable-local-runner"), b"").unwrap();
    verify_tree(&dest, &authentic, &["enable-local-runner"]).unwrap();
    std::fs::write(dest.join("enable-local-runner"), b"x").unwrap();
    assert!(
        verify_tree(&dest, &authentic, &["enable-local-runner"]).is_err(),
        "marker with content"
    );
    std::fs::remove_file(dest.join("enable-local-runner")).unwrap();
    for change in ["edit", "extra", "missing", "link", "release"] {
        let d = dir.path().join(change);
        std::fs::create_dir(&d).unwrap();
        extract_tar_gz(archive.as_slice(), TOP, &d, &expect(&v)).unwrap();
        match change {
            "edit" => std::fs::write(d.join("run-runner"), b"#!/bin/sh\nevil\n").unwrap(),
            "extra" => std::fs::write(d.join("evil.so"), b"x").unwrap(),
            "missing" => std::fs::remove_file(d.join("run-runner")).unwrap(),
            "link" => {
                std::fs::remove_file(d.join("run-runner")).unwrap();
                std::os::unix::fs::symlink("/bin/sh", d.join("run-runner")).unwrap();
            }
            _ => std::fs::write(d.join("RELEASE.json"), b"{}").unwrap(),
        }
        assert_eq!(
            verify_tree(&d, &authentic, &[]).unwrap_err().code,
            ErrorCode::NotVerified,
            "{change}"
        );
    }
}

#[test]
fn zip_central_directory_is_capped_before_it_is_read() {
    // A file whose end record claims a 5 MiB central directory.
    let size: u32 = 5 * 1024 * 1024;
    let mut bytes = vec![0u8; size as usize];
    bytes.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    let e = zip_extract(&bytes).unwrap_err();
    assert_eq!(e.code, ErrorCode::TooLarge, "{e}");
}
