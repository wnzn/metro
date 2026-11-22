//! What the library accepts: a closed list of formats, each checked three
//! ways (declared MIME type, filename extension and the bytes' own
//! signature), which must all agree.
//!
//! Nothing here interprets a file's contents beyond its signature, the
//! UTF-8 check for text, and for DOCX a bounded walk over the ZIP central
//! directory (names only; nothing is decompressed). Embedded scripts,
//! macros, links or URLs inside a file are never followed or run.

use serde::Serialize;

/// Longest asset name, in bytes.
pub const MAX_NAME_BYTES: usize = 255;
/// Most ZIP entries a DOCX may list.
const MAX_ZIP_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    Text,
    Markdown,
    Pdf,
    Docx,
    Png,
    Jpeg,
    Wav,
    Mp3,
    Mp4,
    Webm,
}

const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

impl AssetKind {
    pub const ALL: [AssetKind; 10] = [
        AssetKind::Text,
        AssetKind::Markdown,
        AssetKind::Pdf,
        AssetKind::Docx,
        AssetKind::Png,
        AssetKind::Jpeg,
        AssetKind::Wav,
        AssetKind::Mp3,
        AssetKind::Mp4,
        AssetKind::Webm,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AssetKind::Text => "text",
            AssetKind::Markdown => "markdown",
            AssetKind::Pdf => "pdf",
            AssetKind::Docx => "docx",
            AssetKind::Png => "png",
            AssetKind::Jpeg => "jpeg",
            AssetKind::Wav => "wav",
            AssetKind::Mp3 => "mp3",
            AssetKind::Mp4 => "mp4",
            AssetKind::Webm => "webm",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Valid UTF-8 text (TXT or Markdown).
    pub fn is_text(self) -> bool {
        matches!(self, AssetKind::Text | AssetKind::Markdown)
    }

    /// The canonical MIME type stored and served.
    pub fn mime(self) -> &'static str {
        match self {
            AssetKind::Text => "text/plain",
            AssetKind::Markdown => "text/markdown",
            AssetKind::Pdf => "application/pdf",
            AssetKind::Docx => DOCX_MIME,
            AssetKind::Png => "image/png",
            AssetKind::Jpeg => "image/jpeg",
            AssetKind::Wav => "audio/wav",
            AssetKind::Mp3 => "audio/mpeg",
            AssetKind::Mp4 => "video/mp4",
            AssetKind::Webm => "video/webm",
        }
    }

    /// The `Content-Type` of a download.
    pub fn content_type(self) -> &'static str {
        match self {
            AssetKind::Text => "text/plain; charset=utf-8",
            AssetKind::Markdown => "text/markdown; charset=utf-8",
            other => other.mime(),
        }
    }

    fn mimes(self) -> &'static [&'static str] {
        match self {
            AssetKind::Text => &["text/plain"],
            AssetKind::Markdown => &["text/markdown", "text/x-markdown"],
            AssetKind::Pdf => &["application/pdf"],
            AssetKind::Docx => &[DOCX_MIME],
            AssetKind::Png => &["image/png"],
            AssetKind::Jpeg => &["image/jpeg"],
            AssetKind::Wav => &["audio/wav", "audio/x-wav", "audio/wave", "audio/vnd.wave"],
            AssetKind::Mp3 => &["audio/mpeg", "audio/mp3"],
            AssetKind::Mp4 => &["video/mp4"],
            AssetKind::Webm => &["video/webm"],
        }
    }

    fn extensions(self) -> &'static [&'static str] {
        match self {
            AssetKind::Text => &["txt"],
            AssetKind::Markdown => &["md", "markdown"],
            AssetKind::Pdf => &["pdf"],
            AssetKind::Docx => &["docx"],
            AssetKind::Png => &["png"],
            AssetKind::Jpeg => &["jpg", "jpeg"],
            AssetKind::Wav => &["wav"],
            AssetKind::Mp3 => &["mp3"],
            AssetKind::Mp4 => &["mp4"],
            AssetKind::Webm => &["webm"],
        }
    }
}

/// Why an upload was refused; the message is safe to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected(pub String);

fn reject(m: impl Into<String>) -> Rejected {
    Rejected(m.into())
}

/// A usable basename: no path separators, no control or bidi-override
/// characters, not hidden, bounded, with an extension.
pub fn sanitize_name(raw: &str) -> Result<String, Rejected> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(reject("name is required"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(reject(format!(
            "name is longer than {MAX_NAME_BYTES} bytes"
        )));
    }
    if name.contains(['/', '\\', ':']) {
        return Err(reject("name must be a file name, not a path"));
    }
    let bad = |c: char| {
        c.is_control()
            || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
    };
    if name.chars().any(bad) {
        return Err(reject("name contains control or direction characters"));
    }
    if name.starts_with('.') {
        return Err(reject("name must not start with a dot"));
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.trim().is_empty() && !ext.is_empty() => Ok(name.to_owned()),
        _ => Err(reject("name needs a file extension")),
    }
}

/// The lowercase MIME essence of a `Content-Type`. A `charset` parameter is
/// allowed only on text types and only as UTF-8; other parameters refuse.
fn essence(content_type: &str) -> Result<(String, bool), Rejected> {
    let mut parts = content_type.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let mut charset = false;
    for param in parts {
        let param = param.trim();
        if param.is_empty() {
            continue;
        }
        let (k, v) = param
            .split_once('=')
            .ok_or_else(|| reject("malformed Content-Type"))?;
        let v = v.trim().trim_matches('"');
        if !k.trim().eq_ignore_ascii_case("charset") {
            return Err(reject("unsupported Content-Type parameter"));
        }
        if !v.eq_ignore_ascii_case("utf-8") && !v.eq_ignore_ascii_case("utf8") {
            return Err(reject("text must be UTF-8"));
        }
        charset = true;
    }
    Ok((mime, charset))
}

/// Check one upload: its sanitized `name`, declared `content_type` and
/// bytes. All three must name the same supported format.
pub fn classify(name: &str, content_type: &str, bytes: &[u8]) -> Result<AssetKind, Rejected> {
    if bytes.is_empty() {
        return Err(reject("file is empty"));
    }
    let (mime, charset) = essence(content_type)?;
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let by_ext = AssetKind::ALL
        .into_iter()
        .find(|k| k.extensions().contains(&ext.as_str()))
        .ok_or_else(|| {
            reject("unsupported file type: TXT, MD, PDF, DOCX, PNG, JPEG, WAV, MP3, MP4 or WebM")
        })?;
    if !by_ext.mimes().contains(&mime.as_str()) {
        return Err(reject(format!(
            "Content-Type {mime:?} does not match a .{ext} file (expected {})",
            by_ext.mime()
        )));
    }
    if charset && !by_ext.is_text() {
        return Err(reject("charset applies only to text files"));
    }
    signature(by_ext, bytes)?;
    Ok(by_ext)
}

fn signature(kind: AssetKind, b: &[u8]) -> Result<(), Rejected> {
    let ok = match kind {
        AssetKind::Text | AssetKind::Markdown => return text(b),
        AssetKind::Pdf => b.starts_with(b"%PDF-"),
        AssetKind::Docx => return docx(b),
        AssetKind::Png => b.starts_with(b"\x89PNG\r\n\x1a\n"),
        AssetKind::Jpeg => b.starts_with(&[0xFF, 0xD8, 0xFF]),
        AssetKind::Wav => b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE",
        AssetKind::Mp3 => mp3(b),
        AssetKind::Mp4 => mp4(b),
        AssetKind::Webm => webm(b),
    };
    if ok {
        Ok(())
    } else {
        Err(reject(format!(
            "the file's contents are not a valid {} file",
            kind.as_str().to_uppercase()
        )))
    }
}

fn text(b: &[u8]) -> Result<(), Rejected> {
    let s = std::str::from_utf8(b).map_err(|_| reject("text must be valid UTF-8"))?;
    if s.contains('\0') {
        return Err(reject("text must not contain NUL bytes"));
    }
    Ok(())
}

fn mp3(b: &[u8]) -> bool {
    if b.starts_with(b"ID3") {
        return b.len() >= 10;
    }
    // MPEG audio frame sync, Layer III.
    b.len() >= 4 && b[0] == 0xFF && (b[1] & 0xE0) == 0xE0 && ((b[1] >> 1) & 0x03) == 0x01
}

fn mp4(b: &[u8]) -> bool {
    const BRANDS: [&[u8; 4]; 13] = [
        b"isom", b"iso2", b"iso3", b"iso4", b"iso5", b"iso6", b"mp41", b"mp42", b"avc1", b"dash",
        b"M4V ", b"MSNV", b"mmp4",
    ];
    b.len() >= 12 && &b[4..8] == b"ftyp" && BRANDS.iter().any(|brand| &b[8..12] == *brand)
}

fn webm(b: &[u8]) -> bool {
    if !b.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return false;
    }
    // EBML DocType element (0x4282), length 4, "webm", inside the header.
    let head = &b[..b.len().min(64)];
    head.windows(7)
        .any(|w| w == [0x42, 0x82, 0x84, b'w', b'e', b'b', b'm'])
}

fn u16_at(b: &[u8], at: usize) -> Option<usize> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]) as usize)
}

fn u32_at(b: &[u8], at: usize) -> Option<usize> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
}

/// A Word document: a ZIP whose central directory lists
/// `[Content_Types].xml` and `word/document.xml`, with no macro project,
/// no unsafe entry names and no encrypted entries. Only names are read.
fn docx(b: &[u8]) -> Result<(), Rejected> {
    let bad = || reject("the file's contents are not a valid DOCX file");
    if !b.starts_with(b"PK\x03\x04") || b.len() < 22 {
        return Err(bad());
    }
    // End of central directory: the last signature within the comment window.
    let window = b.len().saturating_sub(22 + 0xFFFF);
    let eocd = (window..=b.len() - 22)
        .rev()
        .find(|&i| b[i..i + 4] == *b"PK\x05\x06")
        .ok_or_else(bad)?;
    let entries = u16_at(b, eocd + 10).ok_or_else(bad)?;
    let cd_size = u32_at(b, eocd + 12).ok_or_else(bad)?;
    let cd_offset = u32_at(b, eocd + 16).ok_or_else(bad)?;
    if entries == 0xFFFF || cd_offset == 0xFFFF_FFFF || entries > MAX_ZIP_ENTRIES {
        return Err(reject("ZIP64 or oversized DOCX archives are not supported"));
    }
    if cd_offset.checked_add(cd_size).is_none_or(|end| end > eocd) {
        return Err(bad());
    }
    let (mut at, mut content_types, mut document) = (cd_offset, false, false);
    for _ in 0..entries {
        if b.get(at..at + 4) != Some(b"PK\x01\x02") {
            return Err(bad());
        }
        let flags = u16_at(b, at + 8).ok_or_else(bad)?;
        let name_len = u16_at(b, at + 28).ok_or_else(bad)?;
        let extra_len = u16_at(b, at + 30).ok_or_else(bad)?;
        let comment_len = u16_at(b, at + 32).ok_or_else(bad)?;
        let name = b.get(at + 46..at + 46 + name_len).ok_or_else(bad)?;
        let name = std::str::from_utf8(name).map_err(|_| bad())?;
        if flags & 0x1 != 0 {
            return Err(reject("encrypted DOCX files are not supported"));
        }
        if name.starts_with('/')
            || name.contains('\\')
            || name.split('/').any(|p| p == "..")
            || name.chars().any(char::is_control)
        {
            return Err(bad());
        }
        if name.eq_ignore_ascii_case("word/vbaProject.bin") {
            return Err(reject("documents with macros are not accepted"));
        }
        content_types |= name == "[Content_Types].xml";
        document |= name == "word/document.xml";
        at += 46 + name_len + extra_len + comment_len;
        if at > eocd {
            return Err(bad());
        }
    }
    if content_types && document {
        Ok(())
    } else {
        Err(bad())
    }
}
