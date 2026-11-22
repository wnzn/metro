use super::validate::{AssetKind, classify, sanitize_name};
use super::*;

fn zip(names: &[&str]) -> Vec<u8> {
    // Minimal stored ZIP: local headers with empty data, central directory, EOCD.
    let mut out = Vec::new();
    let mut central = Vec::new();
    for name in names {
        let offset = out.len() as u32;
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&[0; 12]);
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(names.len() as u16).to_le_bytes());
    out.extend_from_slice(&(names.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

pub(crate) fn docx_bytes() -> Vec<u8> {
    zip(&["[Content_Types].xml", "_rels/.rels", "word/document.xml"])
}

const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

#[test]
fn accepts_each_supported_format() {
    let cases: Vec<(&str, &str, Vec<u8>, AssetKind)> = vec![
        (
            "a.txt",
            "text/plain; charset=utf-8",
            "héllo".into(),
            AssetKind::Text,
        ),
        (
            "a.md",
            "text/markdown",
            b"# hi".to_vec(),
            AssetKind::Markdown,
        ),
        (
            "a.pdf",
            "application/pdf",
            b"%PDF-1.7\n".to_vec(),
            AssetKind::Pdf,
        ),
        ("a.docx", DOCX, docx_bytes(), AssetKind::Docx),
        (
            "a.png",
            "image/png",
            b"\x89PNG\r\n\x1a\n....".to_vec(),
            AssetKind::Png,
        ),
        (
            "a.JPG",
            "image/jpeg",
            vec![0xFF, 0xD8, 0xFF, 0xE0],
            AssetKind::Jpeg,
        ),
        (
            "a.wav",
            "audio/x-wav",
            b"RIFF\0\0\0\0WAVEfmt ".to_vec(),
            AssetKind::Wav,
        ),
        (
            "a.mp3",
            "audio/mpeg",
            b"ID3\x04\0\0\0\0\0\0".to_vec(),
            AssetKind::Mp3,
        ),
        (
            "b.mp3",
            "audio/mpeg",
            vec![0xFF, 0xFB, 0x90, 0x00],
            AssetKind::Mp3,
        ),
        (
            "a.mp4",
            "video/mp4",
            b"\0\0\0\x18ftypisom\0\0\0\0".to_vec(),
            AssetKind::Mp4,
        ),
        (
            "a.webm",
            "video/webm",
            vec![
                0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x82, 0x84, b'w', b'e', b'b', b'm',
            ],
            AssetKind::Webm,
        ),
    ];
    for (name, mime, bytes, kind) in cases {
        assert_eq!(classify(name, mime, &bytes), Ok(kind), "{name}");
    }
}

#[test]
fn refuses_mismatches_and_unsupported() {
    // Extension, MIME and signature must agree.
    assert!(classify("a.png", "image/jpeg", b"\x89PNG\r\n\x1a\n").is_err());
    assert!(classify("a.png", "image/png", b"%PDF-1.7").is_err());
    assert!(classify("a.pdf", "application/octet-stream", b"%PDF-1.7").is_err());
    assert!(classify("a.html", "text/html", b"<script>").is_err());
    assert!(classify("a.svg", "image/svg+xml", b"<svg/>").is_err());
    assert!(classify("a.exe", "application/octet-stream", b"MZ").is_err());
    assert!(classify("a.txt", "text/plain", &[0xFF, 0xFE, 0x00]).is_err());
    assert!(classify("a.txt", "text/plain", b"a\0b").is_err());
    assert!(classify("a.txt", "text/plain; charset=latin1", b"a").is_err());
    assert!(classify("a.pdf", "application/pdf; charset=utf-8", b"%PDF-").is_err());
    assert!(classify("a.txt", "text/plain", b"").is_err());
    assert!(classify("a.mp4", "video/mp4", b"\0\0\0\x18ftypqt  \0\0").is_err());
    // A ZIP that is not a Word document, or carries macros or bad names.
    assert!(classify("a.docx", DOCX, &zip(&["[Content_Types].xml"])).is_err());
    assert!(
        classify(
            "a.docx",
            DOCX,
            &zip(&[
                "[Content_Types].xml",
                "word/document.xml",
                "word/vbaProject.bin"
            ])
        )
        .is_err()
    );
    assert!(
        classify(
            "a.docx",
            DOCX,
            &zip(&["[Content_Types].xml", "word/document.xml", "../evil"])
        )
        .is_err()
    );
    assert!(classify("a.docx", DOCX, b"PK\x03\x04garbage").is_err());
}

#[test]
fn names_are_basenames() {
    assert_eq!(sanitize_name(" report.pdf ").unwrap(), "report.pdf");
    assert_eq!(sanitize_name("résumé (1).docx").unwrap(), "résumé (1).docx");
    for bad in [
        "",
        "../x.txt",
        "a/b.txt",
        "a\\b.txt",
        "C:x.txt",
        ".env",
        "noext",
        "a.",
        "x\u{202E}txt.exe",
        "a\nb.txt",
    ] {
        assert!(sanitize_name(bad).is_err(), "{bad:?}");
    }
    assert!(sanitize_name(&format!("{}.txt", "a".repeat(300))).is_err());
}

#[test]
fn ranges() {
    let h = |s: &str| HeaderValue::from_str(s).unwrap();
    assert_eq!(wanted(None, 10), Wanted::Whole);
    assert_eq!(
        wanted(Some(&h("bytes=0-3")), 10),
        Wanted::Part { start: 0, end: 3 }
    );
    assert_eq!(
        wanted(Some(&h("bytes=5-")), 10),
        Wanted::Part { start: 5, end: 9 }
    );
    assert_eq!(
        wanted(Some(&h("bytes=-4")), 10),
        Wanted::Part { start: 6, end: 9 }
    );
    assert_eq!(
        wanted(Some(&h("bytes=-40")), 10),
        Wanted::Part { start: 0, end: 9 }
    );
    assert_eq!(
        wanted(Some(&h("bytes=2-99")), 10),
        Wanted::Part { start: 2, end: 9 }
    );
    assert_eq!(wanted(Some(&h("bytes=10-")), 10), Wanted::Unsatisfiable);
    assert_eq!(wanted(Some(&h("bytes=-0")), 10), Wanted::Unsatisfiable);
    assert_eq!(wanted(Some(&h("bytes=0-1,3-4")), 10), Wanted::Whole);
    assert_eq!(wanted(Some(&h("bytes=4-2")), 10), Wanted::Whole);
    assert_eq!(wanted(Some(&h("items=0-1")), 10), Wanted::Whole);
}

#[test]
fn disposition_is_quoted_safely() {
    assert_eq!(
        content_disposition("a\"b;c.txt"),
        "attachment; filename=\"a_b_c.txt\"; filename*=UTF-8''a%22b%3Bc.txt"
    );
    assert!(content_disposition("ñ.md").contains("filename*=UTF-8''%C3%B1.md"));
}

#[test]
fn limits_from_env_are_bounded() {
    let none = LibraryLimits::from_lookup(|_| None).unwrap();
    assert_eq!(none, LibraryLimits::default());
    let big = LibraryLimits::from_lookup(|k| {
        (k == "METRO_LIBRARY_MAX_FILE_BYTES").then(|| (HARD_MAX_FILE_BYTES + 1).to_string())
    });
    assert!(big.is_err());
    let small = LibraryLimits::from_lookup(|k| {
        (k == "METRO_LIBRARY_USER_QUOTA_BYTES").then(|| "10".to_owned())
    });
    assert!(small.is_err(), "quota below one file");
    let per_user = LibraryLimits::from_lookup(|k| match k {
        "METRO_LIBRARY_MAX_CONCURRENT_UPLOADS" => Some("2".to_owned()),
        "METRO_LIBRARY_MAX_UPLOADS_PER_USER" => Some("3".to_owned()),
        _ => None,
    });
    assert!(per_user.is_err(), "per-person cap above the global one");
    let one = LibraryLimits::from_lookup(|k| {
        (k == "METRO_LIBRARY_MAX_CONCURRENT_UPLOADS").then(|| "1".to_owned())
    })
    .unwrap();
    assert_eq!(
        one.max_uploads_per_user, 1,
        "default follows a lower global cap"
    );
    let idle = LibraryLimits::from_lookup(|k| match k {
        "METRO_LIBRARY_UPLOAD_TIMEOUT_SECS" => Some("5".to_owned()),
        "METRO_LIBRARY_UPLOAD_IDLE_TIMEOUT_SECS" => Some("6".to_owned()),
        _ => None,
    });
    assert!(idle.is_err(), "idle deadline longer than the total one");
}
