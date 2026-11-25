//! Library search tests against a media model fixture that speaks the
//! real protocol (`protocol.rs`): it checks the exact request shape,
//! decodes `data_base64` and embeds what the bytes contain — page text,
//! pixel colours, per-second audio tones, video frames — never the file
//! name. Every request it receives is recorded with its destination, so
//! the tests assert what reached the "upstream" as much as what came back.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::engine::{self, Engine};
use super::protocol::{self, Kind, LocatorKind};
use super::*;
use crate::db::Db;

// ---- The fixture media model ----

/// Concept axes of the fixture's embedding space.
const AXES: [&str; 7] = ["deploy", "invoice", "red", "blue", "piano", "drums", "dog"];

fn axis_of_word(word: &str) -> Option<usize> {
    Some(match word {
        "deploy" | "deployment" | "rollout" => 0,
        "invoice" | "invoices" | "billing" => 1,
        "red" | "sunset" => 2,
        "blue" | "ocean" => 3,
        "piano" => 4,
        "drum" | "drums" => 5,
        "dog" | "puppy" => 6,
        _ => return None,
    })
}

/// Unnormalized on purpose, so ranking works only if Metro normalizes.
fn text_vector(text: &str, scale: f32) -> Vec<f32> {
    let mut v = vec![0.0; AXES.len() + 1];
    for w in text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if let Some(i) = axis_of_word(w) {
            v[i] += scale;
        }
    }
    v[AXES.len()] = 0.05 * scale;
    v
}

fn axis_vector(axis: usize, scale: f32) -> Vec<f32> {
    let mut v = vec![0.0; AXES.len() + 1];
    v[axis] = scale;
    v[AXES.len()] = 0.05 * scale;
    v
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
const DOCX: &str = protocol::DOCX_MIME;
const WAV: &[u8] = b"RIFF\0\0\0\0WAVE";
const MP4: &[u8] = b"\0\0\0\x18ftypmp42";

fn png(pixels: &[(u8, u8, u8)]) -> Vec<u8> {
    let mut b = PNG.to_vec();
    for (r, g, bl) in pixels {
        b.extend([*r, *g, *bl]);
    }
    b
}

/// One tone code per second: `P` piano, `D` drums, `_` silence.
fn wav(seconds: &str) -> Vec<u8> {
    let mut b = WAV.to_vec();
    b.extend(seconds.bytes());
    b
}

/// One frame code per 500 ms: `G` dog, `R` red, `_` nothing.
fn mp4(frames: &str) -> Vec<u8> {
    let mut b = MP4.to_vec();
    b.extend(frames.bytes());
    b
}

// ---- Genuine PDF and DOCX payloads ----

/// A minimal but well-formed PDF: catalog, page tree, one content stream
/// per page, a font, a correct xref table and trailer. `None` is a scanned
/// page: it only draws an image, so it has no extractable text.
fn pdf(pages: &[Option<&str>]) -> Vec<u8> {
    let n = pages.len();
    // Objects: 1 catalog, 2 pages, 3 font, then (page, contents) pairs.
    let mut objects: Vec<String> = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {n} >>",
            (0..n)
                .map(|i| format!("{} 0 R", 4 + 2 * i))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    for (i, page) in pages.iter().enumerate() {
        let content = match page {
            Some(text) => {
                assert!(
                    !text.contains(['(', ')', '\\']),
                    "keep fixture text unescaped"
                );
                format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET")
            }
            None => "q 400 0 0 300 72 400 cm /Im1 Do Q".to_owned(),
        };
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + 2 * i
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .bytes(),
    );
    out
}

/// The fixture service's PDF preparation: each page's extracted text, or
/// `None` for a page without text (which the service renders instead).
fn pdf_pages(bytes: &[u8]) -> Result<Vec<Option<String>>, MediaError> {
    let unreadable = || MediaError::transient("the model service could not read the PDF");
    // The fixture's PDFs are ASCII after the binary comment line.
    let body = bytes.get(15..).ok_or_else(unreadable)?;
    let text = std::str::from_utf8(body).map_err(|_| unreadable())?;
    if !bytes.starts_with(b"%PDF-") || !text.trim_end().ends_with("%%EOF") {
        return Err(unreadable());
    }
    let count: usize = text
        .split("/Type /Pages")
        .nth(1)
        .and_then(|t| t.split("/Count ").nth(1))
        .and_then(|t| t.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .ok_or_else(unreadable)?;
    let pages: Vec<Option<String>> = text
        .split(">>\nstream\n")
        .skip(1)
        .filter_map(|t| t.split("\nendstream").next())
        .map(|stream| {
            let words: Vec<&str> = stream
                .split('(')
                .skip(1)
                .filter_map(|t| t.split(") Tj").next())
                .collect();
            (!words.is_empty()).then(|| words.join(" "))
        })
        .collect();
    if pages.len() != count {
        return Err(unreadable());
    }
    Ok(pages)
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in data {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A real (stored, uncompressed) zip archive.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in entries {
        let (crc, at) = (crc32(data), out.len() as u32);
        let header = |sig: u32, central: bool| {
            let mut h = sig.to_le_bytes().to_vec();
            if central {
                h.extend(20u16.to_le_bytes()); // made by
            }
            h.extend(20u16.to_le_bytes()); // version needed
            h.extend(0u16.to_le_bytes()); // flags
            h.extend(0u16.to_le_bytes()); // stored
            h.extend([0, 0, 0x21, 0]); // time, date
            h.extend(crc.to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes()); // extra
            if central {
                h.extend([0u8; 8]); // comment, disk, internal attrs
                h.extend(0u32.to_le_bytes()); // external attrs
                h.extend(at.to_le_bytes());
            }
            h.extend(name.bytes());
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend(*data);
        central.extend(header(0x0201_4b50, true));
    }
    let cd_at = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_at.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// A real DOCX: content types plus `word/document.xml`, one `w:p` per
/// paragraph.
fn docx(paragraphs: &[&str]) -> Vec<u8> {
    let body: String = paragraphs
        .iter()
        .map(|p| format!("<w:p><w:r><w:t>{p}</w:t></w:r></w:p>"))
        .collect();
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{body}</w:body></w:document>"
    );
    let types = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
        <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
        </Types>";
    zip(&[
        ("[Content_Types].xml", types.as_bytes()),
        ("word/document.xml", document.as_bytes()),
    ])
}

/// The fixture service's DOCX preparation: the paragraphs of
/// `word/document.xml`, read from the zip's local headers (CRC checked).
fn docx_paragraphs(bytes: &[u8]) -> Result<Vec<String>, MediaError> {
    let bad = || MediaError::transient("DOCX is not a valid zip container");
    let u16_at = |i: usize| {
        bytes
            .get(i..i + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]) as usize)
    };
    let u32_at = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let mut at = 0;
    while u32_at(at) == Some(0x0403_4b50) {
        let (method, crc) = (
            u16_at(at + 8).ok_or_else(bad)?,
            u32_at(at + 14).ok_or_else(bad)?,
        );
        let size = u32_at(at + 18).ok_or_else(bad)? as usize;
        let (name_len, extra) = (
            u16_at(at + 26).ok_or_else(bad)?,
            u16_at(at + 28).ok_or_else(bad)?,
        );
        let name = bytes.get(at + 30..at + 30 + name_len).ok_or_else(bad)?;
        let start = at + 30 + name_len + extra;
        let data = bytes.get(start..start + size).ok_or_else(bad)?;
        if method != 0 || crc32(data) != crc {
            return Err(bad());
        }
        if name == b"word/document.xml" {
            let xml = std::str::from_utf8(data).map_err(|_| bad())?;
            return Ok(xml
                .split("<w:p>")
                .skip(1)
                .map(|p| {
                    p.split("<w:t>")
                        .skip(1)
                        .filter_map(|t| t.split("</w:t>").next())
                        .collect::<String>()
                })
                .filter(|p| !p.is_empty())
                .collect());
        }
        at = start + size;
    }
    Err(MediaError::transient("DOCX has no word/document.xml"))
}

/// `text` split into sentences, as `(char start, char end, sentence)`.
fn sentences(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut start = 0;
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        let last = i + 1 == chars.len();
        if *c == '.' || last {
            let end = i + 1;
            let s: String = chars[start..end].iter().collect();
            let lead = s.len() - s.trim_start().len();
            if !s.trim().is_empty() {
                out.push((start + lead, end, s.trim().to_owned()));
            }
            start = end;
        }
    }
    out
}

fn char_slice(text: &str, start: u64, end: u64) -> String {
    text.chars()
        .skip(start as usize)
        .take((end - start) as usize)
        .collect()
}

fn item(index: usize, embedding: Vec<f32>, locator: Value, excerpt: Option<&str>) -> Value {
    let mut it = json!({ "index": index, "embedding": embedding, "locator": locator });
    if let Some(e) = excerpt {
        it["excerpt"] = json!(e);
    }
    it
}

/// Runs of equal codes as `(code, first, last)` slot indices.
fn runs(codes: &[u8]) -> Vec<(u8, usize, usize)> {
    let mut out: Vec<(u8, usize, usize)> = Vec::new();
    for (i, c) in codes.iter().enumerate() {
        match out.last_mut() {
            Some((code, _, last)) if code == c => *last = i,
            _ => out.push((*c, i, i)),
        }
    }
    out
}

/// The fixture service: `POST /v1/media/embeddings` as a function.
fn fixture_service(
    model: &str,
    request: &Value,
    pad: usize,
    fabricate: bool,
) -> Result<Value, MediaError> {
    let obj = request.as_object().expect("request is an object");
    assert_eq!(obj.len(), 1, "exactly one of query and asset: {request}");
    let mut data = Vec::new();
    if let Some(q) = obj.get("query") {
        let q = q.as_str().expect("query is a string");
        data.push(item(0, text_vector(q, 3.0), json!({"kind": "query"}), None));
    } else {
        let asset = obj["asset"].as_object().expect("asset object");
        let mut keys: Vec<&str> = asset.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["data_base64", "mime_type", "name"]);
        let mime = asset["mime_type"].as_str().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(asset["data_base64"].as_str().unwrap())
            .expect("valid base64");
        match mime {
            // Like the reference service: UTF-8 (BOM stripped), newlines
            // normalized, control characters dropped; code-point offsets
            // into that text; whitespace-collapsed excerpts.
            "text/plain" | "text/markdown" => {
                let text = String::from_utf8(bytes).expect("utf-8 document");
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                let text: String = text
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .chars()
                    .filter(|c| matches!(c, '\n' | '\t') || !c.is_control())
                    .collect();
                for (i, (a, b, chunk)) in sentences(&text).into_iter().enumerate() {
                    let excerpt = if fabricate {
                        "FAKE invoice summary".to_owned()
                    } else {
                        chunk.split_whitespace().collect::<Vec<_>>().join(" ")
                    };
                    data.push(item(
                        i,
                        text_vector(&chunk, 2.0),
                        json!({"kind": "text", "start": a, "end": b}),
                        Some(&excerpt),
                    ));
                }
            }
            "application/pdf" => {
                let mut i = 0;
                for (n, page) in pdf_pages(&bytes)?.into_iter().enumerate() {
                    match page {
                        Some(text) => {
                            for (a, b, chunk) in sentences(&text) {
                                data.push(item(
                                    i,
                                    text_vector(&chunk, 1.5),
                                    json!({"kind": "page", "page": n + 1, "start": a, "end": b}),
                                    Some(&chunk),
                                ));
                                i += 1;
                            }
                        }
                        // Rendered as an image: no text, so no excerpt.
                        None => {
                            data.push(item(
                                i,
                                axis_vector(3, 5.0),
                                json!({"kind": "page", "page": n + 1}),
                                None,
                            ));
                            i += 1;
                        }
                    }
                }
            }
            DOCX => {
                let text = docx_paragraphs(&bytes)?.join("\n");
                for (i, (a, b, chunk)) in sentences(&text).into_iter().enumerate() {
                    data.push(item(
                        i,
                        text_vector(&chunk, 2.5),
                        json!({"kind": "text", "start": a, "end": b}),
                        Some(&chunk),
                    ));
                }
            }
            "image/png" => {
                assert!(bytes.starts_with(PNG), "not a PNG");
                let px = &bytes[PNG.len()..];
                let (mut red, mut blue) = (0usize, 0usize);
                for p in px.as_chunks::<3>().0 {
                    if p[0] > p[2] {
                        red += 1;
                    } else if p[2] > p[0] {
                        blue += 1;
                    }
                }
                let mut v = vec![0.0; AXES.len() + 1];
                v[2] = red as f32 * 10.0;
                v[3] = blue as f32 * 10.0;
                v[AXES.len()] = 0.5;
                // A captioning model might describe it; Metro must drop it.
                data.push(item(0, v, json!({"kind": "image"}), Some("a caption")));
            }
            "audio/wav" => {
                assert!(bytes.starts_with(WAV), "not a WAV");
                for (i, (code, a, b)) in runs(&bytes[WAV.len()..]).into_iter().enumerate() {
                    let axis = match code {
                        b'P' => 4,
                        b'D' => 5,
                        _ => 0,
                    };
                    let mut v = axis_vector(axis, 4.0);
                    if code == b'_' {
                        v = vec![0.0; AXES.len() + 1];
                        v[AXES.len()] = 1.0;
                    }
                    data.push(item(
                        i,
                        v,
                        json!({"kind": "time", "start_ms": a * 1000, "end_ms": (b + 1) * 1000}),
                        // A transcript: kept as an excerpt, never reranked.
                        (code == b'P').then_some("piano solo"),
                    ));
                }
            }
            "video/mp4" => {
                assert!(bytes.starts_with(MP4), "not an MP4");
                for (i, (code, a, b)) in runs(&bytes[MP4.len()..]).into_iter().enumerate() {
                    let v = match code {
                        b'G' => axis_vector(6, 2.0),
                        b'R' => axis_vector(2, 2.0),
                        _ => {
                            let mut v = vec![0.0; AXES.len() + 1];
                            v[AXES.len()] = 1.0;
                            v
                        }
                    };
                    data.push(item(
                        i,
                        v,
                        json!({"kind": "time", "start_ms": a * 500, "end_ms": (b + 1) * 500}),
                        None,
                    ));
                }
            }
            other => {
                return Err(MediaError::transient(format!(
                    "the media embedding model does not support {other} files"
                )));
            }
        }
    }
    for it in &mut data {
        let e = it["embedding"].as_array_mut().unwrap();
        e.extend(std::iter::repeat_n(json!(1.0), pad));
    }
    Ok(json!({ "model": model, "data": data, "partial": false }))
}

#[derive(Debug, Clone)]
struct Sent {
    dest: Option<String>,
    /// The query, or the asset's name.
    what: String,
    /// SHA-256 of the decoded asset bytes.
    bytes_sha: Option<String>,
    mime: Option<String>,
}

type EmbedHook = Box<dyn Fn() -> Option<MediaError> + Send + Sync>;

struct Fixture {
    sent: Mutex<Vec<Sent>>,
    decided: Mutex<Vec<(Option<String>, String)>>,
    /// Fingerprint per destination (`personal` or the Space id); missing
    /// means the connection may not be used there.
    fingerprints: std::sync::Arc<Mutex<HashMap<String, String>>>,
    /// Connections that embed media.
    media: Mutex<HashMap<String, bool>>,
    pad: Mutex<usize>,
    nouls: Mutex<HashMap<String, f64>>,
    /// Runs while an asset request is "in flight".
    on_embed: Mutex<Option<EmbedHook>>,
    /// Runs while a query request is "in flight".
    on_query: Mutex<Option<EmbedHook>>,
    /// Destinations whose connection is temporarily unavailable.
    outage: std::sync::Arc<Mutex<std::collections::HashSet<String>>>,
    /// Make the text model return words that aren't in the file.
    fabricate: Mutex<bool>,
}

fn dest_key(dest: Option<&str>) -> String {
    dest.unwrap_or("personal").to_owned()
}

static RUN: AtomicUsize = AtomicUsize::new(0);

impl Fixture {
    fn new() -> Self {
        let f = Self {
            sent: Mutex::default(),
            decided: Mutex::default(),
            fingerprints: std::sync::Arc::default(),
            media: Mutex::default(),
            pad: Mutex::new(0),
            nouls: Mutex::default(),
            on_embed: Mutex::new(None),
            on_query: Mutex::new(None),
            outage: std::sync::Arc::default(),
            fabricate: Mutex::new(false),
        };
        f.media.lock().unwrap().insert("media".into(), true);
        f.media.lock().unwrap().insert("media2".into(), true);
        f.media.lock().unwrap().insert("text-only".into(), false);
        f
    }

    /// A unique fingerprint (the query cache is process-wide).
    fn allow(&self, dest: Option<&str>) {
        let n = RUN.fetch_add(1, Ordering::SeqCst);
        self.fingerprints
            .lock()
            .unwrap()
            .insert(dest_key(dest), format!("fp-{n}"));
    }

    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }

    fn decided(&self) -> Vec<(Option<String>, String)> {
        self.decided.lock().unwrap().clone()
    }
}

impl MediaModels for Fixture {
    fn media_capable<'a>(&'a self, dest: Option<&'a str>, c: &'a str) -> ModelFuture<'a, bool> {
        let usable = self
            .fingerprints
            .lock()
            .unwrap()
            .contains_key(&dest_key(dest));
        let media = self.media.lock().unwrap().get(c).copied();
        Box::pin(async move {
            match (usable, media) {
                (true, Some(m)) => Ok(m),
                (false, Some(_)) => Err(MediaError::gone(
                    "this connection is not shared with that Space",
                )),
                _ => Err(MediaError::gone("connection not found")),
            }
        })
    }

    fn fingerprint<'a>(&'a self, dest: Option<&'a str>, _c: &'a str) -> ModelFuture<'a, String> {
        let fp = self
            .fingerprints
            .lock()
            .unwrap()
            .get(&dest_key(dest))
            .cloned();
        let outage = self.outage.lock().unwrap().contains(&dest_key(dest));
        Box::pin(async move {
            if outage {
                return Err(MediaError::transient("the device is offline"));
            }
            fp.ok_or_else(|| MediaError::gone("this connection is not shared with that Space"))
        })
    }

    fn embed<'a>(
        &'a self,
        dest: Option<&'a str>,
        c: &'a str,
        input: Value,
    ) -> ModelFuture<'a, Value> {
        let sent = match (input.get("query"), input.get("asset")) {
            (Some(q), None) => Sent {
                dest: dest.map(str::to_owned),
                what: q.as_str().unwrap().to_owned(),
                bytes_sha: None,
                mime: None,
            },
            (None, Some(a)) => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(a["data_base64"].as_str().unwrap())
                    .unwrap();
                Sent {
                    dest: dest.map(str::to_owned),
                    what: a["name"].as_str().unwrap().to_owned(),
                    bytes_sha: Some(hex(&bytes)),
                    mime: a["mime_type"].as_str().map(str::to_owned),
                }
            }
            _ => panic!("malformed request {input}"),
        };
        let is_asset = sent.bytes_sha.is_some();
        self.sent.lock().unwrap().push(sent);
        let hook = if is_asset {
            &self.on_embed
        } else {
            &self.on_query
        };
        if let Some(hook) = hook.lock().unwrap().as_ref()
            && let Some(error) = hook()
        {
            return Box::pin(async move { Err(error) });
        }
        let fabricate = *self.fabricate.lock().unwrap();
        let media = self.media.lock().unwrap().get(c).copied() == Some(true);
        let pad = *self.pad.lock().unwrap();
        let out = if media {
            // The provider adds the model and checks both directions with
            // the shared protocol crate; so does the fixture.
            let mut request = input.clone();
            request["model"] = json!("google/embeddinggemma-2");
            let checked = metro_model_protocol::check_media_request(&request)
                .expect("Metro sent a protocol-legal request");
            fixture_service("google/embeddinggemma-2", &input, pad, fabricate).map(|answer| {
                metro_model_protocol::check_media_response(&checked, &answer, 4096)
                    .expect("the fixture answers protocol-legal responses")
            })
        } else {
            Err(MediaError::gone("this connection only embeds text"))
        };
        Box::pin(async move { out })
    }

    fn decide<'a>(
        &'a self,
        dest: Option<&'a str>,
        _c: &'a str,
        state: Value,
        questions: Value,
    ) -> ModelFuture<'a, Value> {
        assert_eq!(questions[engine::RERANK_QUESTION]["type"], "noul");
        let obj = state.as_object().unwrap();
        assert_eq!(obj.len(), 2, "one excerpt per request: {state}");
        let text = state["candidate_excerpt"].as_str().unwrap().to_owned();
        self.decided
            .lock()
            .unwrap()
            .push((dest.map(str::to_owned), text.clone()));
        let noul = self.nouls.lock().unwrap().get(&text).copied();
        Box::pin(async move {
            match noul {
                Some(n) => Ok(json!({
                    "model": "jev-latest",
                    "answers": { engine::RERANK_QUESTION: { "type": "noul", "noul": n } },
                })),
                None => Err(MediaError::transient("System One returned 529")),
            }
        })
    }

    /// The fixture's grant: the destination's current fingerprint.
    fn grant(&self) -> GrantCheck {
        let fps = self.fingerprints.clone();
        std::sync::Arc::new(move |_conn, dest, _id| {
            fps.lock()
                .unwrap()
                .get(&dest_key(dest))
                .cloned()
                .ok_or_else(|| MediaError::gone("this connection is not shared with that Space"))
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---- The library ----

/// The real library tables and access checks (`crate::library`), with a
/// switch that simulates a broken library ignoring them.
struct TestLibrary {
    real: upstream::DbLibrary,
    leaky: Mutex<bool>,
}

impl TestLibrary {
    /// Any asset by id, ignoring access: only for the leaky mode.
    fn raw(&self, id: &str) -> Option<(AssetInfo, Vec<u8>)> {
        use rusqlite::OptionalExtension;
        self.real
            .db
            .lock()
            .query_row(
                "SELECT a.id, a.uploader_id, a.space_id, a.name, a.mime_type, a.size_bytes,
                        a.sha256, b.bytes
                 FROM library_assets a JOIN library_blobs b ON b.asset_id = a.id
                 WHERE a.id = ?1",
                [id],
                |r| {
                    Ok((
                        AssetInfo {
                            id: r.get(0)?,
                            owner_id: r.get(1)?,
                            space_id: r.get(2)?,
                            name: r.get(3)?,
                            mime_type: r.get(4)?,
                            size: r.get::<_, i64>(5)? as u64,
                            sha256: r.get(6)?,
                        },
                        r.get(7)?,
                    ))
                },
            )
            .optional()
            .unwrap()
    }
}

impl Library for TestLibrary {
    fn visible<'a>(&'a self, user: &'a str, id: &'a str) -> LibFuture<'a, Option<AssetInfo>> {
        if *self.leaky.lock().unwrap() {
            let out = self.raw(id).map(|(a, _)| a);
            return Box::pin(async move { Ok(out) });
        }
        self.real.visible(user, id)
    }

    fn bytes<'a>(
        &'a self,
        user: &'a str,
        id: &'a str,
    ) -> LibFuture<'a, Option<(AssetInfo, Vec<u8>)>> {
        if *self.leaky.lock().unwrap() {
            let out = self.raw(id);
            return Box::pin(async move { Ok(out) });
        }
        self.real.bytes(user, id)
    }
}

fn library_kind(mime: &str) -> &'static str {
    match mime {
        "text/plain" => "text",
        "text/markdown" => "markdown",
        "application/pdf" => "pdf",
        DOCX => "docx",
        "image/png" => "png",
        "audio/wav" => "wav",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        other => panic!("the library does not accept {other}"),
    }
}

// ---- World ----

struct World {
    db: Db,
    lib: TestLibrary,
    fx: Fixture,
}

impl World {
    fn new() -> Self {
        engine::clear_cache();
        // `Db::init` creates the library and library search tables.
        let db = Db::open_in_memory().unwrap();
        let fx = Fixture::new();
        fx.allow(None);
        Self {
            lib: TestLibrary {
                real: upstream::DbLibrary { db: db.clone() },
                leaky: Mutex::new(false),
            },
            db,
            fx,
        }
    }

    fn space(&self, owner: &str, members: &[&str]) -> String {
        let s = self.db.create_space(owner, "space", 10).unwrap();
        for m in members {
            self.db.add_member(&s.id, m, 10).unwrap();
        }
        self.fx.allow(Some(&s.id));
        s.id
    }

    /// Store a file the way the library does (its tables, its access
    /// rules); uploading the same id again replaces its bytes in place.
    fn upload(
        &self,
        id: &str,
        owner: &str,
        space: Option<&str>,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) {
        let c = self.db.lock();
        let (size, sha) = (bytes.len() as i64, hex(&bytes));
        let updated = c
            .execute(
                "UPDATE library_assets SET size_bytes = ?2, sha256 = ?3 WHERE id = ?1",
                rusqlite::params![id, size, sha],
            )
            .unwrap();
        if updated == 1 {
            c.execute(
                "UPDATE library_blobs SET bytes = ?2 WHERE asset_id = ?1",
                rusqlite::params![id, bytes],
            )
            .unwrap();
            return;
        }
        c.execute(
            "INSERT INTO library_assets
                 (id, uploader_id, space_id, name, kind, mime_type, size_bytes, sha256, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            rusqlite::params![id, owner, space, name, library_kind(mime), mime, size, sha],
        )
        .unwrap();
        c.execute(
            "INSERT INTO library_blobs (asset_id, bytes) VALUES (?1, ?2)",
            rusqlite::params![id, bytes],
        )
        .unwrap();
    }

    fn delete(&self, id: &str) {
        self.db
            .lock()
            .execute("DELETE FROM library_assets WHERE id = ?1", [id])
            .unwrap();
    }

    fn routing(classifier: bool) -> Routing {
        Routing {
            embedding: Some("media".into()),
            classifier: classifier.then(|| "s1".to_owned()),
        }
    }

    /// An engine for `user` on the `media` connection; their setting names
    /// it unless they already chose something.
    fn engine<'a>(&'a self, user: &'a str, classifier: bool) -> Engine<'a> {
        let mut c = self.db.lock();
        if store::embedding_id(&c, user).unwrap().is_none() {
            store::set_embedding_id(&mut c, user, Some("media"), None).unwrap();
        }
        drop(c);
        Engine::new(
            &self.db,
            user,
            &self.lib,
            &self.fx,
            Self::routing(classifier),
        )
    }

    async fn index(&self, user: &str, id: &str) -> Result<IndexResponse, LibrarySearchError> {
        self.engine(user, false)
            .index(&[id.to_owned()], false)
            .await
    }

    async fn search(
        &self,
        user: &str,
        classifier: bool,
        scope: Scope,
        space: Option<&str>,
        query: &str,
        kind: Option<Kind>,
    ) -> Result<SearchResponse, LibrarySearchError> {
        let (sources, left) =
            resolve_sources(&self.db, user, scope, space.map(str::to_owned)).await?;
        self.engine(user, classifier)
            .search(query, &sources, left, 10, kind)
            .await
    }

    fn segments(&self, user: &str) -> usize {
        store::segment_count(&self.db.lock(), user).unwrap()
    }
}

const INVOICE_DOC: &str =
    "Deploy notes for the rollout.\nThe invoice total and billing terms.\nLunch menu.";

// ---- Protocol ----

#[test]
fn mime_kinds() {
    assert_eq!(
        Kind::of_mime("text/plain; charset=utf-8"),
        Some(Kind::Document)
    );
    assert_eq!(Kind::of_mime("text/markdown"), Some(Kind::Document));
    assert_eq!(Kind::of_mime("IMAGE/PNG"), Some(Kind::Image));
    assert_eq!(Kind::of_mime("audio/mpeg"), Some(Kind::Audio));
    assert_eq!(Kind::of_mime("video/webm"), Some(Kind::Video));
    // Documents the model service prepares; Metro sends their bytes.
    assert_eq!(Kind::of_mime("application/pdf"), Some(Kind::Document));
    assert_eq!(Kind::of_mime(DOCX), Some(Kind::Document));
    assert!(protocol::unsupported_reason("application/zip").contains("application/zip"));
    assert_eq!(Kind::of_mime("application/zip"), None);
    assert_eq!(Kind::of_mime("image/svg+xml"), None);
    // What is sent is the bare type the protocol accepts.
    let r = protocol::asset_request("a.txt", "text/plain; charset=utf-8", b"x");
    assert_eq!(r["asset"]["mime_type"], "text/plain");
    assert!(
        metro_model_protocol::check_media_request(&{
            let mut r = r.clone();
            r["model"] = json!("google/embeddinggemma-2");
            r
        })
        .is_ok()
    );
    assert!(
        metro_model_protocol::check_media_request(&json!({
            "model": "m", "query": "q"
        }))
        .is_ok()
    );
}

#[test]
fn answers_that_do_not_fit_are_refused() {
    let ok = |loc: Value| json!({"model": "m", "data": [{"index": 0, "embedding": [1.0, 0.0], "locator": loc}], "partial": false});
    // Locators must fit the file kind.
    assert!(
        protocol::read_asset(&ok(json!({"kind": "page", "page": 1})), Kind::Image, None).is_err()
    );
    assert!(protocol::read_asset(&ok(json!({"kind": "image"})), Kind::Document, None).is_err());
    assert!(protocol::read_asset(&ok(json!({"kind": "query"})), Kind::Audio, None).is_err());
    assert!(
        protocol::read_asset(
            &ok(json!({"kind": "time", "start_ms": 5, "end_ms": 1})),
            Kind::Audio,
            None
        )
        .is_err()
    );
    assert!(
        protocol::read_asset(
            &ok(json!({"kind": "page", "page": 0})),
            Kind::Document,
            None
        )
        .is_err()
    );
    assert!(
        protocol::read_asset(&ok(json!({"kind": "image", "page": 2})), Kind::Image, None).is_err()
    );
    // A page may carry character offsets into its extracted text.
    let doc = |loc: Value| protocol::read_asset(&ok(loc), Kind::Document, None).is_ok();
    assert!(doc(
        json!({"kind": "page", "page": 1, "start": 0, "end": 2})
    ));
    assert!(!doc(json!({"kind": "page", "page": 1, "start": 4})));
    assert!(!doc(
        json!({"kind": "page", "page": 1, "start": 4, "end": 2})
    ));
    let audio = |loc: Value| protocol::read_asset(&ok(loc), Kind::Audio, None).is_ok();
    assert!(audio(json!({"kind": "time", "start_ms": 3, "end_ms": 3})));
    assert!(!audio(json!({"kind": "time", "start_ms": 3})));
    assert!(protocol::read_asset(&ok(json!({"kind": "text"})), Kind::Document, None).is_ok());
    // `partial` is required.
    let mut no_partial = ok(json!({"kind": "image"}));
    no_partial.as_object_mut().unwrap().remove("partial");
    assert!(protocol::read_asset(&no_partial, Kind::Image, None).is_err());
    // A text range outside an uploaded text file keeps its vector but
    // gets no excerpt; inside, the excerpt is the file's own text.
    let text = ok(json!({"kind": "text", "start": 0, "end": 50}));
    let short = protocol::read_asset(&text, Kind::Document, Some("short")).unwrap();
    assert_eq!(short.segments[0].excerpt, None);
    let long = "a".repeat(50);
    let long = protocol::read_asset(&text, Kind::Document, Some(&long)).unwrap();
    assert_eq!(
        long.segments[0].excerpt.as_deref(),
        Some("a".repeat(50).as_str())
    );
    assert!(
        protocol::read_asset(
            &ok(json!({"kind": "time", "start_ms": 0, "end_ms": 9})),
            Kind::Video,
            None
        )
        .is_ok()
    );
    // Mixed dimensions, repeated or missing indices, empty data, no model.
    let mixed = json!({"model": "m", "data": [
        {"index": 0, "embedding": [1.0, 0.0], "locator": {"kind": "page", "page": 1}},
        {"index": 1, "embedding": [1.0], "locator": {"kind": "page", "page": 2}}], "partial": false});
    assert!(protocol::read_asset(&mixed, Kind::Document, None).is_err());
    let dup = json!({"model": "m", "data": [
        {"index": 0, "embedding": [1.0], "locator": {"kind": "page", "page": 1}},
        {"index": 0, "embedding": [1.0], "locator": {"kind": "page", "page": 2}}], "partial": false});
    assert!(protocol::read_asset(&dup, Kind::Document, None).is_err());
    assert!(
        protocol::read_asset(&json!({"model": "m", "data": []}), Kind::Document, None).is_err()
    );
    assert!(protocol::read_asset(&json!({"model": "", "data": []}), Kind::Document, None).is_err());
    let zero = ok(json!({"kind": "image"}));
    let zero = json!({"model": "m", "data": [{"index": 0, "embedding": [0.0, 0.0], "locator": zero["data"][0]["locator"]}], "partial": false});
    assert!(protocol::read_asset(&zero, Kind::Image, None).is_err());
    // A query answer is one item with a query locator.
    assert!(protocol::read_query(&ok(json!({"kind": "query"}))).is_ok());
    assert!(protocol::read_query(&ok(json!({"kind": "page", "page": 1}))).is_err());
}

#[test]
fn index_body_rules() {
    let b = |v: Value| check_index_body(serde_json::from_value(v).unwrap());
    assert_eq!(
        b(json!({"asset_id": "a"})).unwrap(),
        (vec!["a".to_owned()], false)
    );
    assert!(b(json!({})).is_err());
    assert!(b(json!({"asset_id": "a", "asset_ids": ["b"]})).is_err());
    assert!(b(json!({"asset_ids": []})).is_err());
    assert!(b(json!({"asset_ids": ["a", "a"]})).is_err());
    assert!(
        b(json!({"asset_ids": (0..=MAX_BULK).map(|i| i.to_string()).collect::<Vec<_>>()})).is_err()
    );
    assert!(serde_json::from_value::<IndexBody>(json!({"asset_id": "a", "path": "/etc"})).is_err());
}

// ---- Indexing and search ----

#[tokio::test]
async fn text_document_finds_the_right_passage() {
    let w = World::new();
    let bytes = INVOICE_DOC.as_bytes().to_vec();
    w.upload(
        "a1",
        "alice",
        None,
        "notes.txt",
        "text/plain",
        bytes.clone(),
    );
    let r = w.index("alice", "a1").await.unwrap();
    assert_eq!(r.indexed_segments, 3);
    assert!(!r.partial);
    assert_eq!(r.items[0].kind, Some(Kind::Document));
    // The model received exactly the file's own bytes, for the personal
    // library.
    let sent = w.fx.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].dest, None);
    assert_eq!(sent[0].bytes_sha.as_deref(), Some(hex(&bytes).as_str()));

    let s = w
        .search(
            "alice",
            false,
            Scope::Personal,
            None,
            "billing invoice",
            None,
        )
        .await
        .unwrap();
    let top = &s.results[0];
    assert_eq!(top.asset_id, "a1");
    assert_eq!(top.name, "notes.txt");
    assert_eq!(top.kind, Kind::Document);
    assert_eq!(top.locator.kind, LocatorKind::Text);
    let excerpt = top.excerpt.as_deref().unwrap();
    assert_eq!(excerpt, "The invoice total and billing terms.");
    assert_eq!(
        char_slice(
            INVOICE_DOC,
            top.locator.start.unwrap(),
            top.locator.end.unwrap()
        ),
        excerpt
    );
    assert_eq!(top.score_kind, ScoreKind::Cosine);
    assert!(top.score > 0.9 && top.score <= 1.0);
    assert_eq!(s.ranking, Ranking::Embeddings);
    assert!(!s.partial);
    // Over three results from one file are never shown.
    assert!(s.results.len() <= MAX_PER_ASSET);
}

#[tokio::test]
async fn text_ranges_for_markdown() {
    let w = World::new();
    let md = "Rollout plan for deploy.\n\nSend the invoice.";
    w.upload(
        "m",
        "alice",
        None,
        "plan.md",
        "text/markdown",
        md.as_bytes().to_vec(),
    );
    w.index("alice", "m").await.unwrap();
    let s = w
        .search("alice", false, Scope::Personal, None, "invoice", None)
        .await
        .unwrap();
    let l = &s.results[0].locator;
    assert_eq!(l.kind, LocatorKind::Text);
    assert_eq!(
        &md[l.start.unwrap() as usize..l.end.unwrap() as usize],
        "Send the invoice."
    );
}

#[tokio::test]
async fn content_not_file_names_decides() {
    let w = World::new();
    // Names lie; the bytes tell the truth.
    w.upload("x", "alice", None, "piano.wav", "audio/wav", wav("DDD"));
    w.upload("y", "alice", None, "drums.wav", "audio/wav", wav("__PP_"));
    w.upload("v", "alice", None, "cat.mp4", "video/mp4", mp4("__GG_R"));
    for id in ["x", "y", "v"] {
        w.index("alice", id).await.unwrap();
    }
    let s = w
        .search("alice", false, Scope::Personal, None, "piano", None)
        .await
        .unwrap();
    let top = &s.results[0];
    assert_eq!(top.name, "drums.wav");
    assert_eq!(top.locator.kind, LocatorKind::Time);
    assert_eq!(
        (top.locator.start_ms, top.locator.end_ms),
        (Some(2000), Some(4000))
    );

    let s = w
        .search(
            "alice",
            false,
            Scope::Personal,
            None,
            "dog",
            Some(Kind::Video),
        )
        .await
        .unwrap();
    assert!(s.results.iter().all(|r| r.kind == Kind::Video));
    let top = &s.results[0];
    assert_eq!(top.name, "cat.mp4");
    assert_eq!(
        (top.locator.start_ms, top.locator.end_ms),
        (Some(1000), Some(2000))
    );
    assert!(top.excerpt.is_none());
}

#[tokio::test]
async fn images_rank_by_pixels_and_keep_no_caption() {
    let w = World::new();
    w.upload(
        "r",
        "alice",
        None,
        "ocean.png",
        "image/png",
        png(&[(250, 40, 10); 6]),
    );
    w.upload(
        "b",
        "alice",
        None,
        "sunset.png",
        "image/png",
        png(&[(10, 40, 250); 6]),
    );
    w.index("alice", "r").await.unwrap();
    w.index("alice", "b").await.unwrap();
    let s = w
        .search("alice", true, Scope::Personal, None, "sunset", None)
        .await
        .unwrap();
    assert_eq!(
        s.results[0].name, "ocean.png",
        "red pixels win for 'sunset'"
    );
    assert_eq!(s.results[0].locator.kind, LocatorKind::Image);
    assert!(
        s.results.iter().all(|r| r.excerpt.is_none()),
        "captions are not kept"
    );
    // A classifier is configured, but nothing here is text: no System One
    // call, honest embedding order and label.
    assert!(w.fx.decided().is_empty());
    assert_eq!(s.ranking, Ranking::Embeddings);
    assert!(s.results.iter().all(|r| r.score_kind == ScoreKind::Cosine));
}

#[tokio::test]
async fn unindexed_files_never_leave() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload(
        "a",
        "alice",
        None,
        "notes.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.upload("b", "bob", Some(&sid), "song.wav", "audio/wav", wav("PP"));
    let s = w
        .search("alice", true, Scope::All, None, "invoice", None)
        .await
        .unwrap();
    assert!(s.results.is_empty());
    assert!(
        s.notice
            .unwrap()
            .contains("Only files you index are searched")
    );
    assert!(
        w.fx.sent().is_empty(),
        "no query and no bytes went anywhere"
    );
}

// ---- Access ----

#[tokio::test]
async fn outsiders_and_other_peoples_personal_files() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload(
        "s",
        "bob",
        Some(&sid),
        "plan.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.upload(
        "p",
        "bob",
        None,
        "diary.txt",
        "text/plain",
        "invoice".into(),
    );
    // Carol is not a member; Alice cannot read Bob's personal file.
    assert_eq!(
        w.index("carol", "s").await.unwrap_err(),
        LibrarySearchError::NotFound("file")
    );
    assert_eq!(
        w.index("alice", "p").await.unwrap_err(),
        LibrarySearchError::NotFound("file")
    );
    assert_eq!(
        w.index("alice", "nope").await.unwrap_err(),
        LibrarySearchError::NotFound("file")
    );
    assert_eq!(
        w.search("carol", false, Scope::Space, Some(&sid), "invoice", None)
            .await
            .unwrap_err(),
        LibrarySearchError::NotFound("space")
    );
    assert!(w.fx.sent().is_empty());
    // Bob indexes both; Alice still sees neither in her own searches.
    w.index("bob", "s").await.unwrap();
    w.index("bob", "p").await.unwrap();
    let s = w
        .search("alice", false, Scope::All, None, "invoice", None)
        .await
        .unwrap();
    assert!(s.results.is_empty(), "indexes are per person");
    let s = w
        .search("bob", false, Scope::All, None, "invoice", None)
        .await
        .unwrap();
    let ids: Vec<&str> = s.results.iter().map(|r| r.asset_id.as_str()).collect();
    assert!(ids.contains(&"s") && ids.contains(&"p"));
}

#[tokio::test]
async fn a_leaky_library_is_still_refused() {
    let w = World::new();
    let sid = w.space("bob", &[]);
    w.upload(
        "p",
        "bob",
        None,
        "diary.txt",
        "text/plain",
        "invoice".into(),
    );
    w.upload(
        "s",
        "bob",
        Some(&sid),
        "s.txt",
        "text/plain",
        "invoice".into(),
    );
    *w.lib.leaky.lock().unwrap() = true;
    assert!(w.index("alice", "p").await.is_err());
    assert!(w.index("alice", "s").await.is_err());
    assert!(w.fx.sent().is_empty(), "Bob's bytes never left");
}

#[tokio::test]
async fn removed_member_loses_results_and_rows() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload(
        "s",
        "alice",
        Some(&sid),
        "plan.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.index("bob", "s").await.unwrap();
    assert_eq!(
        w.search("bob", false, Scope::All, None, "invoice", None)
            .await
            .unwrap()
            .results
            .len(),
        3.min(MAX_PER_ASSET)
    );
    assert_eq!(w.segments("bob"), 3);
    w.db.remove_member(&sid, "bob").unwrap();
    let before = w.fx.sent().len();
    let s = w
        .search("bob", false, Scope::All, None, "invoice", None)
        .await
        .unwrap();
    assert!(s.results.is_empty());
    assert_eq!(w.segments("bob"), 0, "rows purged");
    assert!(
        w.fx.sent()[before..]
            .iter()
            .all(|s| s.dest.as_deref() != Some(sid.as_str())),
        "nothing more was sent for the Space he left"
    );
    assert_eq!(
        w.index("bob", "s").await.unwrap_err(),
        LibrarySearchError::NotFound("file")
    );
}

#[tokio::test]
async fn revoked_while_the_model_runs_stores_nothing() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload(
        "s",
        "alice",
        Some(&sid),
        "plan.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    let db = w.db.clone();
    let s2 = sid.clone();
    *w.fx.on_embed.lock().unwrap() = Some(Box::new(move || {
        db.remove_member(&s2, "bob").unwrap();
        None
    }));
    assert_eq!(
        w.index("bob", "s").await.unwrap_err(),
        LibrarySearchError::NotFound("file")
    );
    assert_eq!(w.segments("bob"), 0);
}

#[tokio::test]
async fn removed_between_retrieval_and_rerank() {
    // A file deleted after its vectors were read is never sent to System
    // One nor returned.
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "notes.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.index("alice", "a").await.unwrap();
    w.delete("a");
    let s = w
        .search("alice", true, Scope::Personal, None, "invoice", None)
        .await
        .unwrap();
    assert!(s.results.is_empty());
    assert!(w.fx.decided().is_empty());
    assert_eq!(w.segments("alice"), 0);
}

#[tokio::test]
async fn space_content_goes_only_to_calls_for_that_space() {
    let w = World::new();
    let s1 = w.space("alice", &[]);
    let s2 = w.space("alice", &[]);
    w.upload(
        "p",
        "alice",
        None,
        "p.txt",
        "text/plain",
        "invoice one".into(),
    );
    w.upload(
        "a",
        "alice",
        Some(&s1),
        "a.txt",
        "text/plain",
        "invoice two".into(),
    );
    w.upload(
        "b",
        "alice",
        Some(&s2),
        "b.png",
        "image/png",
        png(&[(200, 0, 0)]),
    );
    for id in ["p", "a", "b"] {
        w.index("alice", id).await.unwrap();
    }
    let s = w
        .search("alice", true, Scope::All, None, "invoice", None)
        .await
        .unwrap();
    assert_eq!(s.results.len(), 3);
    let assets: Vec<(String, Option<String>)> =
        w.fx.sent()
            .into_iter()
            .filter(|s| s.bytes_sha.is_some())
            .map(|s| (s.what, s.dest))
            .collect();
    let dest_of = |name: &str| assets.iter().find(|(n, _)| n == name).unwrap().1.clone();
    assert_eq!(dest_of("p.txt"), None);
    assert_eq!(dest_of("a.txt"), Some(s1.clone()));
    assert_eq!(dest_of("b.png"), Some(s2.clone()));
    // One query embedding per destination.
    let queries: Vec<Option<String>> =
        w.fx.sent()
            .into_iter()
            .filter(|s| s.bytes_sha.is_none())
            .map(|s| s.dest)
            .collect();
    assert_eq!(queries.len(), 3);
    // Excerpts went to System One only for their own Space.
    for (dest, text) in w.fx.decided() {
        match text.as_str() {
            "invoice one" => assert_eq!(dest, None),
            "invoice two" => assert_eq!(dest.as_deref(), Some(s1.as_str())),
            other => panic!("unexpected excerpt sent: {other}"),
        }
    }
}

#[tokio::test]
async fn hooks_delete_rows() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload("a", "alice", None, "a.txt", "text/plain", "invoice".into());
    w.upload(
        "s",
        "alice",
        Some(&sid),
        "s.txt",
        "text/plain",
        "invoice".into(),
    );
    w.index("alice", "a").await.unwrap();
    w.index("alice", "s").await.unwrap();
    w.index("bob", "s").await.unwrap();
    hooks::member_removed(&w.db.lock(), &sid, "bob").unwrap();
    assert_eq!(w.segments("bob"), 0);
    hooks::asset_deleted(&w.db.lock(), "a").unwrap();
    assert_eq!(w.segments("alice"), 1);
    hooks::space_deleted(&w.db.lock(), &sid).unwrap();
    assert_eq!(w.segments("alice"), 0);
}

#[tokio::test]
async fn deleting_a_file_cascades_to_every_index() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload(
        "s",
        "alice",
        Some(&sid),
        "s.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.index("alice", "s").await.unwrap();
    w.index("bob", "s").await.unwrap();
    assert_eq!((w.segments("alice"), w.segments("bob")), (3, 3));
    w.delete("s");
    assert_eq!((w.segments("alice"), w.segments("bob")), (0, 0));
    let n: i64 =
        w.db.lock()
            .query_row("SELECT COUNT(*) FROM library_search_assets", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(n, 0);
}

// ---- Staleness ----

#[tokio::test]
async fn new_model_fingerprint_drops_the_old_index() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.index("alice", "a").await.unwrap();
    w.fx.allow(None); // the connection was reconfigured
    let before = w.fx.sent().len();
    let s = w
        .search("alice", false, Scope::Personal, None, "invoice", None)
        .await
        .unwrap();
    assert!(s.results.is_empty());
    assert!(s.partial);
    assert!(s.notice.unwrap().contains("index them again"));
    assert_eq!(w.segments("alice"), 0);
    assert_eq!(
        w.fx.sent().len(),
        before,
        "no query sent for an empty index"
    );
    // Reindexing restores it.
    w.index("alice", "a").await.unwrap();
    assert!(
        !w.search("alice", false, Scope::Personal, None, "invoice", None)
            .await
            .unwrap()
            .results
            .is_empty()
    );
}

#[tokio::test]
async fn new_dimension_under_the_same_fingerprint_is_stale() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.index("alice", "a").await.unwrap();
    *w.fx.pad.lock().unwrap() = 3;
    let s = w
        .search("alice", false, Scope::Personal, None, "invoice", None)
        .await
        .unwrap();
    assert!(s.results.is_empty());
    assert!(s.partial);
    assert_eq!(w.segments("alice"), 0);
}

#[tokio::test]
async fn changed_bytes_are_not_returned() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.index("alice", "a").await.unwrap();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        "Lunch menu only.".into(),
    );
    let s = w
        .search("alice", false, Scope::Personal, None, "invoice", None)
        .await
        .unwrap();
    assert!(s.results.is_empty(), "old vectors never describe new bytes");
    assert_eq!(w.segments("alice"), 0);
}

#[tokio::test]
async fn connection_changed_during_indexing() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    // Reconfigure while the model runs.
    let n = RUN.fetch_add(1, Ordering::SeqCst);
    let fps = w.fx.fingerprints.clone();
    *w.fx.on_embed.lock().unwrap() = Some(Box::new(move || {
        fps.lock()
            .unwrap()
            .insert("personal".into(), format!("fp-{n}"));
        None
    }));
    assert!(matches!(
        w.index("alice", "a").await,
        Err(LibrarySearchError::Unusable(_))
    ));
    assert_eq!(w.segments("alice"), 0);
}

// ---- Unsupported and typed models ----

#[tokio::test]
async fn unsupported_types_and_models() {
    let w = World::new();
    // A file the model service cannot prepare: a clear typed error,
    // nothing stored.
    w.upload(
        "z",
        "alice",
        None,
        "broken.pdf",
        "application/pdf",
        b"%PDF-1.7 truncated".to_vec(),
    );
    match w.index("alice", "z").await {
        Err(LibrarySearchError::Upstream(m)) => assert!(m.contains("could not read the PDF")),
        other => panic!("{other:?}"),
    }
    // A type Metro sends but this model refuses: a typed model error,
    // nothing stored.
    w.upload(
        "f",
        "alice",
        None,
        "a.webm",
        "video/webm",
        b"\x1aE\xdf\xa3webm".to_vec(),
    );
    match w.index("alice", "f").await {
        Err(LibrarySearchError::Upstream(m)) => assert!(m.contains("does not support video/webm")),
        other => panic!("{other:?}"),
    }
    assert_eq!(w.segments("alice"), 0);
    let mimes: Vec<Option<String>> = w.fx.sent().into_iter().map(|s| s.mime).collect();
    assert_eq!(
        mimes,
        [Some("application/pdf".into()), Some("video/webm".into())]
    );
    // Too large: refused before the bytes are read.
    w.upload(
        "big",
        "alice",
        None,
        "big.txt",
        "text/plain",
        vec![b'a'; MAX_ASSET_BYTES as usize + 1],
    );
    assert_eq!(
        w.index("alice", "big").await.unwrap_err(),
        LibrarySearchError::TooLarge
    );
    assert_eq!(w.fx.sent().len(), 2);
}

#[tokio::test]
async fn text_only_connection_never_stands_in() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    let e = Engine::new(
        &w.db,
        "alice",
        &w.lib,
        &w.fx,
        Routing {
            embedding: Some("text-only".into()),
            classifier: None,
        },
    );
    assert_eq!(
        e.index(&["a".into()], false).await.unwrap_err(),
        LibrarySearchError::NotMediaCapable
    );
    assert!(w.fx.sent().is_empty());
    // No media connection at all.
    let e = Engine::new(&w.db, "alice", &w.lib, &w.fx, Routing::default());
    assert_eq!(
        e.index(&["a".into()], false).await.unwrap_err(),
        LibrarySearchError::NoMediaModel
    );
    assert_eq!(
        e.search("invoice", &[Source::Personal], 0, 10, None)
            .await
            .unwrap_err(),
        LibrarySearchError::NoMediaModel
    );
}

#[tokio::test]
async fn settings_never_switch_silently() {
    let w = World::new();
    // Nothing chosen: a text-only prior is not suggested, a media one is.
    let v = engine::settings_view(&w.db, &w.fx, "alice", Some("text-only".into()))
        .await
        .unwrap();
    assert_eq!(
        v,
        SettingsView {
            embedding_id: None,
            selection_precondition: true,
            suggested_id: None
        }
    );
    let v = engine::settings_view(&w.db, &w.fx, "alice", Some("media".into()))
        .await
        .unwrap();
    assert_eq!(
        v,
        SettingsView {
            embedding_id: None,
            selection_precondition: true,
            suggested_id: Some("media".into())
        }
    );
    // Saving: text-only and unknown connections are refused.
    let put = |id: Option<&str>| id.map(str::to_owned);
    assert_eq!(
        engine::save_settings(&w.db, &w.fx, "alice", put(Some("text-only")), None)
            .await
            .unwrap_err(),
        LibrarySearchError::NotMediaCapable
    );
    assert!(matches!(
        engine::save_settings(&w.db, &w.fx, "alice", put(Some("ghost")), None).await,
        Err(LibrarySearchError::Unusable(_))
    ));
    engine::save_settings(&w.db, &w.fx, "alice", put(Some("media")), None)
        .await
        .unwrap();
    let v = engine::settings_view(&w.db, &w.fx, "alice", Some("media".into()))
        .await
        .unwrap();
    assert_eq!(
        v,
        SettingsView {
            embedding_id: Some("media".into()),
            selection_precondition: true,
            suggested_id: None
        }
    );
    // Changing (or clearing) the connection discards the index.
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.index("alice", "a").await.unwrap();
    assert_eq!(w.segments("alice"), 3);
    engine::save_settings(&w.db, &w.fx, "alice", put(Some("media")), None)
        .await
        .unwrap();
    assert_eq!(w.segments("alice"), 3, "same connection keeps it");
    engine::save_settings(&w.db, &w.fx, "alice", put(None), None)
        .await
        .unwrap();
    assert_eq!(w.segments("alice"), 0);
    assert!(
        serde_json::from_value::<Settings>(json!({"embedding_id": "x", "model": "y"})).is_err()
    );
}

#[tokio::test]
async fn bulk_index_reports_each_file() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.upload("p", "bob", None, "b.txt", "text/plain", "invoice".into());
    w.upload(
        "z",
        "alice",
        None,
        "a.webm",
        "video/webm",
        b"\x1aE\xdf\xa3".to_vec(),
    );
    let r = w
        .engine("alice", false)
        .index(&["a".into(), "p".into(), "z".into()], true)
        .await
        .unwrap();
    let st: Vec<ItemStatus> = r.items.iter().map(|i| i.status).collect();
    assert_eq!(
        st,
        [
            ItemStatus::Indexed,
            ItemStatus::NotFound,
            ItemStatus::Failed
        ]
    );
    assert!(
        r.items[2]
            .error
            .as_deref()
            .unwrap()
            .contains("does not support video/webm")
    );
    assert_eq!(r.indexed_segments, 3);
    assert!(r.partial);
    assert_eq!(w.fx.sent().len(), 2, "Bob's file was never sent");
}

// ---- Score label integrity ----

#[tokio::test]
async fn reranking_only_reorders_document_excerpts() {
    let w = World::new();
    // Two documents and a red image all match "red invoice" by embedding.
    w.upload(
        "d1",
        "alice",
        None,
        "d1.txt",
        "text/plain",
        "invoice invoice red".into(),
    );
    w.upload(
        "d2",
        "alice",
        None,
        "d2.txt",
        "text/plain",
        "invoice".into(),
    );
    w.upload(
        "img",
        "alice",
        None,
        "red.png",
        "image/png",
        png(&[(250, 0, 0); 3]),
    );
    for id in ["d1", "d2", "img"] {
        w.index("alice", id).await.unwrap();
    }
    let plain = w
        .search("alice", false, Scope::Personal, None, "red invoice", None)
        .await
        .unwrap();
    let plain_order: Vec<String> = plain.results.iter().map(|r| r.asset_id.clone()).collect();
    let img_at = plain_order.iter().position(|a| a == "img").unwrap();

    // System One prefers d2's excerpt.
    w.fx.nouls.lock().unwrap().insert("invoice".into(), 0.95);
    w.fx.nouls
        .lock()
        .unwrap()
        .insert("invoice invoice red".into(), 0.20);
    let s = w
        .search("alice", true, Scope::Personal, None, "red invoice", None)
        .await
        .unwrap();
    assert_eq!(s.ranking, Ranking::Mixed);
    // Only document excerpts reached System One.
    let mut sent: Vec<String> = w.fx.decided().into_iter().map(|(_, t)| t).collect();
    sent.sort();
    assert_eq!(sent, ["invoice", "invoice invoice red"]);
    // The image keeps its embedding position, score and label.
    let order: Vec<&str> = s.results.iter().map(|r| r.asset_id.as_str()).collect();
    assert_eq!(order[img_at], "img");
    let img = &s.results[img_at];
    assert_eq!(img.score_kind, ScoreKind::Cosine);
    assert_eq!(img.score, plain.results[img_at].score);
    // The documents were reordered among their own positions, by noul.
    let docs: Vec<(&str, f64, ScoreKind)> = s
        .results
        .iter()
        .filter(|r| r.kind == Kind::Document)
        .map(|r| (r.asset_id.as_str(), r.score, r.score_kind))
        .collect();
    assert_eq!(
        docs,
        [
            ("d2", 0.95, ScoreKind::Classifier),
            ("d1", 0.20, ScoreKind::Classifier)
        ]
    );
}

#[tokio::test]
async fn classifier_label_only_when_every_result_was_judged() {
    let w = World::new();
    w.upload("d", "alice", None, "d.txt", "text/plain", "invoice".into());
    w.index("alice", "d").await.unwrap();
    w.fx.nouls.lock().unwrap().insert("invoice".into(), 0.8);
    let s = w
        .search("alice", true, Scope::Personal, None, "invoice", None)
        .await
        .unwrap();
    assert_eq!(s.ranking, Ranking::Classifier);
    assert_eq!(s.results[0].score, 0.8);
    assert_eq!(s.results[0].score_kind, ScoreKind::Classifier);

    // System One fails: embeddings, said so.
    w.fx.nouls.lock().unwrap().clear();
    let s = w
        .search("alice", true, Scope::Personal, None, "invoice", None)
        .await
        .unwrap();
    assert_eq!(s.ranking, Ranking::Embeddings);
    assert_eq!(s.results[0].score_kind, ScoreKind::Cosine);
    assert!(s.notice.unwrap().contains("could not rerank"));

    // No classifier chosen.
    let s = w
        .search("alice", false, Scope::Personal, None, "invoice", None)
        .await
        .unwrap();
    assert_eq!(s.ranking, Ranking::Embeddings);
    assert!(s.notice.unwrap().contains("No System One connection"));
}

#[tokio::test]
async fn media_excerpts_are_dropped_and_never_reranked() {
    // The model returned transcript text for the audio; it is the model's
    // words, not the file, so it is neither shown nor sent to System One.
    let w = World::new();
    w.upload("x", "alice", None, "x.wav", "audio/wav", wav("PP"));
    w.index("alice", "x").await.unwrap();
    w.fx.nouls.lock().unwrap().insert("piano solo".into(), 0.99);
    let s = w
        .search("alice", true, Scope::Personal, None, "piano", None)
        .await
        .unwrap();
    assert_eq!(s.results[0].excerpt, None);
    assert!(w.fx.decided().is_empty());
    assert_eq!(s.ranking, Ranking::Embeddings);
    assert_eq!(s.results[0].score_kind, ScoreKind::Cosine);
}

#[tokio::test]
async fn unshared_space_is_skipped_not_guessed() {
    let w = World::new();
    let sid = w.space("alice", &[]);
    w.upload(
        "s",
        "alice",
        Some(&sid),
        "s.txt",
        "text/plain",
        "invoice".into(),
    );
    w.upload("p", "alice", None, "p.txt", "text/plain", "invoice".into());
    w.index("alice", "s").await.unwrap();
    w.index("alice", "p").await.unwrap();
    // The connection's share with that Space is revoked.
    w.fx.fingerprints.lock().unwrap().remove(&sid);
    let before = w.fx.sent().len();
    let s = w
        .search("alice", false, Scope::All, None, "invoice", None)
        .await
        .unwrap();
    assert!(s.partial);
    assert_eq!(
        s.results
            .iter()
            .map(|r| r.asset_id.as_str())
            .collect::<Vec<_>>(),
        ["p"]
    );
    assert!(w.fx.sent()[before..].iter().all(|x| x.dest.is_none()));
    assert!(matches!(
        w.index("alice", "s").await,
        Err(LibrarySearchError::Unusable(_))
    ));
}

#[test]
fn wire_shapes() {
    let r = SearchResult {
        asset_id: "a".into(),
        name: "n".into(),
        mime_type: "audio/wav".into(),
        kind: Kind::Audio,
        space_id: None,
        locator: Locator {
            kind: LocatorKind::Time,
            start: None,
            end: None,
            page: None,
            start_ms: Some(0),
            end_ms: Some(1000),
        },
        excerpt: None,
        score: 0.5,
        score_kind: ScoreKind::Cosine,
    };
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        json!({"asset_id": "a", "name": "n", "mime_type": "audio/wav", "kind": "audio",
               "space_id": null, "locator": {"kind": "time", "start_ms": 0, "end_ms": 1000},
               "score": 0.5, "score_kind": "cosine"})
    );
    let b: SearchBody =
        serde_json::from_value(json!({"query": "q", "scope": "all", "kind": "video"})).unwrap();
    assert_eq!(b.kind, Some(Kind::Video));
    assert!(serde_json::from_value::<SearchBody>(json!({"query": "q", "scope": "chat"})).is_err());
}

// ---- PDF and DOCX ----

const PDF_PAGES: [Option<&str>; 3] = [
    Some("Deploy checklist for the rollout. Lunch is at noon."),
    None,
    Some("Payment schedule. The invoice total and billing terms."),
];

#[tokio::test]
async fn pdf_pages_text_offsets_and_rendered_scans() {
    let w = World::new();
    let bytes = pdf(&PDF_PAGES);
    w.upload(
        "pdf",
        "alice",
        None,
        "report.pdf",
        "application/pdf",
        bytes.clone(),
    );
    // An outsider can't read it, so nothing is sent for them.
    assert_eq!(
        w.index("carol", "pdf").await.unwrap_err(),
        LibrarySearchError::NotFound("file")
    );
    assert!(w.fx.sent().is_empty());

    let r = w.index("alice", "pdf").await.unwrap();
    assert_eq!(r.items[0].kind, Some(Kind::Document));
    assert_eq!(r.indexed_segments, 5, "2 + 1 rendered page + 2");
    // The model service got the library's exact bytes, as a PDF.
    let sent = w.fx.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].bytes_sha.as_deref(), Some(hex(&bytes).as_str()));
    assert_eq!(sent[0].mime.as_deref(), Some("application/pdf"));
    assert_eq!(sent[0].dest, None);

    // The right page, with offsets into that page's extracted text.
    w.fx.nouls
        .lock()
        .unwrap()
        .insert("The invoice total and billing terms.".into(), 0.9);
    let s = w
        .search(
            "alice",
            true,
            Scope::Personal,
            None,
            "billing invoice",
            None,
        )
        .await
        .unwrap();
    let top = &s.results[0];
    assert_eq!(
        (top.locator.kind, top.locator.page),
        (LocatorKind::Page, Some(3))
    );
    let excerpt = top.excerpt.as_deref().unwrap();
    assert_eq!(excerpt, "The invoice total and billing terms.");
    let page3 = PDF_PAGES[2].unwrap();
    assert_eq!(
        char_slice(page3, top.locator.start.unwrap(), top.locator.end.unwrap()),
        excerpt
    );
    assert_eq!(top.score_kind, ScoreKind::Classifier);

    // The scanned page is found by its rendered image; it has no excerpt,
    // so System One never saw it and it is never labelled reranked.
    let s = w
        .search("alice", true, Scope::Personal, None, "blue", None)
        .await
        .unwrap();
    let scan = &s.results[0];
    assert_eq!((scan.locator.page, scan.locator.start), (Some(2), None));
    assert!(scan.excerpt.is_none());
    assert_eq!(scan.score_kind, ScoreKind::Cosine);
    assert!(w.fx.decided().iter().all(|(_, t)| !t.is_empty()));
    assert!(
        w.fx.decided()
            .iter()
            .all(|(_, t)| PDF_PAGES.iter().flatten().any(|p| p.contains(t.as_str())))
    );
}

#[tokio::test]
async fn docx_in_a_space_with_character_offsets() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    let paragraphs = [
        "Quarterly rollout notes.",
        "Send the invoice to billing. Then deploy.",
    ];
    let bytes = docx(&paragraphs);
    w.upload(
        "doc",
        "alice",
        Some(&sid),
        "notes.docx",
        DOCX,
        bytes.clone(),
    );
    assert_eq!(
        w.index("carol", "doc").await.unwrap_err(),
        LibrarySearchError::NotFound("file")
    );
    assert!(w.fx.sent().is_empty(), "nothing sent for an outsider");

    let r = w.index("bob", "doc").await.unwrap();
    assert_eq!(r.indexed_segments, 3);
    let sent = w.fx.sent();
    assert_eq!(
        sent[0].dest.as_deref(),
        Some(sid.as_str()),
        "sent for the file's Space"
    );
    assert_eq!(sent[0].bytes_sha.as_deref(), Some(hex(&bytes).as_str()));
    assert_eq!(sent[0].mime.as_deref(), Some(DOCX));

    let s = w
        .search("bob", false, Scope::Space, Some(&sid), "invoice", None)
        .await
        .unwrap();
    let top = &s.results[0];
    assert_eq!(
        (top.name.as_str(), top.space_id.as_deref()),
        ("notes.docx", Some(sid.as_str()))
    );
    assert_eq!(top.locator.kind, LocatorKind::Text);
    let text = paragraphs.join("\n");
    let excerpt = top.excerpt.as_deref().unwrap();
    assert_eq!(excerpt, "Send the invoice to billing.");
    assert_eq!(
        char_slice(&text, top.locator.start.unwrap(), top.locator.end.unwrap()),
        excerpt
    );
    // Alice never indexed it: her index is her own.
    assert!(
        w.search("alice", false, Scope::Space, Some(&sid), "invoice", None)
            .await
            .unwrap()
            .results
            .is_empty()
    );
    // Removed from the Space: gone, and no more egress for it.
    w.db.remove_member(&sid, "bob").unwrap();
    assert_eq!(w.segments("bob"), 0, "the storage owner's hook purged it");
    assert_eq!(
        w.index("bob", "doc").await.unwrap_err(),
        LibrarySearchError::NotFound("file")
    );
}

#[test]
fn fixture_payloads_are_genuine() {
    let p = pdf(&PDF_PAGES);
    assert!(p.starts_with(b"%PDF-1.7"));
    // The xref table's offsets point at the objects, in bytes.
    let find = |needle: &[u8]| p.windows(needle.len()).rposition(|w| w == needle).unwrap();
    let tail = std::str::from_utf8(&p[find(b"startxref\n") + 10..]).unwrap();
    let xref_at: usize = tail.lines().next().unwrap().parse().unwrap();
    assert!(p[xref_at..].starts_with(b"xref\n"));
    let table = std::str::from_utf8(&p[xref_at..]).unwrap();
    for (i, line) in table.lines().skip(3).take(5).enumerate() {
        let off: usize = line[..10].parse().unwrap();
        assert!(p[off..].starts_with(format!("{} 0 obj", i + 1).as_bytes()));
    }
    assert_eq!(pdf_pages(&p).unwrap()[1], None);
    let d = docx(&["a b."]);
    assert!(d.starts_with(b"PK\x03\x04"));
    assert_eq!(docx_paragraphs(&d).unwrap(), ["a b."]);
    let mut corrupt = d.clone();
    let at = corrupt.windows(4).position(|w| w == b"a b.").unwrap();
    corrupt[at] = b'z';
    assert!(docx_paragraphs(&corrupt).is_err(), "the CRC is checked");
}

#[test]
fn reference_service_extras_are_accepted() {
    // Optional per-row `modality` and top-level `notes`; a rendered scan
    // page without excerpt next to a text chunk of the same PDF.
    let answer = json!({
        "model": "google/embeddinggemma-2",
        "data": [
            {"index": 0, "embedding": [1.0, 0.0], "modality": "text",
             "locator": {"kind": "page", "page": 1, "start": 0, "end": 12},
             "excerpt": "Invoice due."},
            {"index": 1, "embedding": [0.0, 1.0], "modality": "image",
             "locator": {"kind": "page", "page": 2}}
        ],
        "partial": false,
        "notes": ["page 2 had no text layer and was rendered"]
    });
    let a = protocol::read_asset(&answer, Kind::Document, None).unwrap();
    assert_eq!(a.segments.len(), 2);
    assert_eq!(a.segments[0].excerpt.as_deref(), Some("Invoice due."));
    assert_eq!(a.segments[1].excerpt, None);
    assert_eq!(a.segments[1].locator.page, Some(2));
    // A 16 s video clip and an 8 s audio window.
    let clip = json!({"model": "m", "partial": false, "data": [
        {"index": 0, "embedding": [1.0], "modality": "video",
         "locator": {"kind": "time", "start_ms": 0, "end_ms": 16000}}]});
    assert!(protocol::read_asset(&clip, Kind::Video, None).is_ok());
    let wav = json!({"model": "m", "partial": false, "data": [
        {"index": 0, "embedding": [1.0], "locator": {"kind": "time", "start_ms": 8000, "end_ms": 16000}}]});
    assert!(protocol::read_asset(&wav, Kind::Audio, None).is_ok());
}

// ---- Races: held model calls ----

fn hold(w: &World, f: impl Fn(&Db) + Send + Sync + 'static) {
    let db = w.db.clone();
    *w.fx.on_embed.lock().unwrap() = Some(Box::new(move || {
        f(&db);
        None
    }));
}

#[tokio::test]
async fn unsuccessful_file_embed_rechecks_access_and_preserves_error_kinds() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload(
        "s",
        "alice",
        Some(&sid),
        "plan.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.index("bob", "s").await.unwrap();
    let indexed = w.segments("bob");

    *w.fx.on_embed.lock().unwrap() =
        Some(Box::new(|| Some(MediaError::transient("backend offline"))));
    let e = w.index("bob", "s").await.unwrap_err();
    assert_eq!(e.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(e.message(), "backend offline");

    *w.fx.on_embed.lock().unwrap() = Some(Box::new(|| {
        Some(MediaError::internal("relay database fault"))
    }));
    let e = w.index("bob", "s").await.unwrap_err();
    assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(e.message(), "internal error");

    let db = w.db.clone();
    *w.fx.on_embed.lock().unwrap() = Some(Box::new(move || {
        // Fail the actual access query after the embed reached the backend.
        db.lock()
            .execute("ALTER TABLE space_members RENAME TO held_members", [])
            .unwrap();
        Some(MediaError::transient("backend offline"))
    }));
    let e = w.index("bob", "s").await.unwrap_err();
    assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(e.message(), "internal error");
    w.db.lock()
        .execute("ALTER TABLE held_members RENAME TO space_members", [])
        .unwrap();
    assert_eq!(
        w.segments("bob"),
        indexed,
        "faults must not purge valid rows"
    );

    let db = w.db.clone();
    *w.fx.on_embed.lock().unwrap() = Some(Box::new(move || {
        db.remove_member(&sid, "bob").unwrap();
        Some(MediaError::gone("provider withheld the held answer"))
    }));
    assert_eq!(
        w.index("bob", "s").await.unwrap_err(),
        LibrarySearchError::NotFound("file")
    );
    assert_eq!(w.segments("bob"), 0);
}

#[tokio::test]
async fn settings_switched_while_the_old_model_runs_stores_nothing() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.engine("alice", false); // the setting names `media`
    hold(&w, |db| {
        store::set_embedding_id(&mut db.lock(), "alice", Some("media2"), None).unwrap();
    });
    match w.index("alice", "a").await {
        Err(LibrarySearchError::Unusable(m)) => assert!(m.contains("nothing was stored")),
        other => panic!("{other:?}"),
    }
    assert_eq!(w.segments("alice"), 0, "the old model's vectors never land");
    assert_eq!(
        store::embedding_id(&w.db.lock(), "alice")
            .unwrap()
            .as_deref(),
        Some("media2"),
        "the new setting stands"
    );
}

#[tokio::test]
async fn deleted_while_the_model_runs_is_not_found_not_an_error() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.upload("b", "alice", None, "b.txt", "text/plain", "invoice".into());
    hold(&w, |db| {
        db.lock()
            .execute("DELETE FROM library_assets WHERE id = 'a'", [])
            .unwrap();
    });
    assert_eq!(
        w.index("alice", "a").await.unwrap_err(),
        LibrarySearchError::NotFound("file")
    );
    // In bulk, the deleted file is one item's status; the rest index.
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    let r = w
        .engine("alice", false)
        .index(&["a".into(), "b".into()], true)
        .await
        .unwrap();
    assert_eq!(r.items[0].status, ItemStatus::NotFound);
    assert_eq!(r.items[0].error.as_deref(), Some("file not found"));
    assert_eq!(r.items[1].status, ItemStatus::Indexed);
    assert_eq!(w.segments("alice"), 1, "only b's segment");
}

#[tokio::test]
async fn changed_while_the_model_runs_stores_nothing() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    hold(&w, |db| {
        let c = db.lock();
        c.execute(
            "UPDATE library_blobs SET bytes = x'00' WHERE asset_id = 'a'",
            [],
        )
        .unwrap();
        c.execute(
            "UPDATE library_assets SET sha256 = 'other' WHERE id = 'a'",
            [],
        )
        .unwrap();
    });
    assert!(w.index("alice", "a").await.is_err());
    assert_eq!(w.segments("alice"), 0);
}

/// `store_asset` alone, so each revalidation is seen without the engine's
/// earlier checks in front of it.
#[test]
fn the_store_transaction_revalidates_everything() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload("s", "alice", Some(&sid), "s.txt", "text/plain", "x".into());
    let sha = hex(b"x");
    store::set_embedding_id(&mut w.db.lock(), "bob", Some("media"), None).unwrap();
    let seg = protocol::Segment {
        seq: 0,
        vector: vec![1.0],
        locator: Locator {
            kind: LocatorKind::Text,
            start: None,
            end: None,
            page: None,
            start_ms: None,
            end_ms: None,
        },
        excerpt: None,
    };
    let segs = [seg];
    fn new<'a>(conn: &'a str, hash: &'a str) -> store::NewAsset<'a> {
        store::NewAsset {
            asset_id: "s",
            source: "",
            space_id: None,
            connection_id: conn,
            kind: Kind::Document,
            fingerprint: "fp",
            content_hash: hash,
            model: "m",
            dims: 1,
            partial: false,
            segments: &[],
        }
    }
    let key =
        |k: &'static str| -> GrantCheck { std::sync::Arc::new(move |_, _, _| Ok(k.to_owned())) };
    let store = |a: store::NewAsset<'_>, g: GrantCheck| {
        let key = "k1".to_owned();
        let a = store::NewAsset {
            source: &format!("space:{sid}"),
            space_id: Some(&sid),
            segments: &segs,
            ..a
        };
        store::store_asset(&mut w.db.lock(), "bob", &a, &g, &key, 100).unwrap()
    };
    use store::StoreOutcome as O;
    assert_eq!(
        store(new("old", &sha), key("k1")),
        O::Switched,
        "setting moved on"
    );
    assert_eq!(
        store(new("media", "stale"), key("k1")),
        O::Changed,
        "other bytes now"
    );
    assert_eq!(
        store(new("media", &sha), key("k2")),
        O::Switched,
        "grant or config changed"
    );
    let gone: GrantCheck = std::sync::Arc::new(|_, _, _| Err(MediaError::gone("revoked")));
    assert_eq!(
        store(new("media", &sha), gone),
        O::Switched,
        "share revoked"
    );
    assert_eq!(store(new("media", &sha), key("k1")), O::Stored);
    w.db.remove_member(&sid, "bob").unwrap();
    assert_eq!(
        store(new("media", &sha), key("k1")),
        O::Revoked,
        "no longer a member"
    );
    w.delete("s");
    assert_eq!(
        store(new("media", &sha), key("k1")),
        O::Revoked,
        "deleted: no FK error"
    );
}

#[tokio::test]
async fn an_old_request_cannot_purge_a_fresh_index() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    // An old search took its snapshot and holds the old fingerprint...
    let old_snapshot = store::snapshot(&w.db.lock()).unwrap();
    let old_fp = w.fx.fingerprints.lock().unwrap()["personal"].clone();
    // ...meanwhile the connection is reconfigured and the file reindexed.
    w.fx.allow(None);
    w.index("alice", "a").await.unwrap();
    assert_eq!(w.segments("alice"), 3);
    // The old request's purge doesn't touch rows written after it began.
    let n = store::purge_stale(
        &mut w.db.lock(),
        "alice",
        "personal",
        "media",
        &old_fp,
        None,
        old_snapshot,
    )
    .unwrap();
    assert_eq!((n, w.segments("alice")), (0, 3));
    let n =
        store::purge_source(&mut w.db.lock(), "alice", "personal", "media", old_snapshot).unwrap();
    assert_eq!((n, w.segments("alice")), (0, 3));
    // A request on a connection that is no longer the setting purges nothing.
    let now = store::snapshot(&w.db.lock()).unwrap();
    let n = store::purge_source(&mut w.db.lock(), "alice", "personal", "media2", now).unwrap();
    assert_eq!((n, w.segments("alice")), (0, 3));
    // A current request with the current fingerprint keeps them too.
    let fp = w.fx.fingerprints.lock().unwrap()["personal"].clone();
    let n = store::purge_stale(
        &mut w.db.lock(),
        "alice",
        "personal",
        "media",
        &fp,
        None,
        now,
    )
    .unwrap();
    assert_eq!((n, w.segments("alice")), (0, 3));
}

#[tokio::test]
async fn definitive_loss_purges_but_an_outage_only_skips() {
    let w = World::new();
    let sid = w.space("alice", &[]);
    w.upload("p", "alice", None, "p.txt", "text/plain", "invoice".into());
    w.upload(
        "s",
        "alice",
        Some(&sid),
        "s.txt",
        "text/plain",
        "invoice".into(),
    );
    w.index("alice", "p").await.unwrap();
    w.index("alice", "s").await.unwrap();
    // The device is offline for the Space: skipped, rows kept.
    w.fx.outage.lock().unwrap().insert(sid.clone());
    let s = w
        .search("alice", false, Scope::All, None, "invoice", None)
        .await
        .unwrap();
    assert!(s.partial);
    assert_eq!(
        s.results
            .iter()
            .map(|r| r.asset_id.as_str())
            .collect::<Vec<_>>(),
        ["p"]
    );
    assert_eq!(w.segments("alice"), 2, "an outage deletes nothing");
    // The share is revoked: the Space's rows go, even though no
    // fingerprint can be read any more.
    w.fx.outage.lock().unwrap().clear();
    w.fx.fingerprints.lock().unwrap().remove(&sid);
    let s = w
        .search("alice", false, Scope::All, None, "invoice", None)
        .await
        .unwrap();
    assert!(s.notice.unwrap().contains("removed from your index"));
    assert_eq!(w.segments("alice"), 1);
}

#[tokio::test]
async fn provider_hooks_purge_by_connection() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload(
        "s",
        "alice",
        Some(&sid),
        "s.txt",
        "text/plain",
        "invoice".into(),
    );
    w.index("alice", "s").await.unwrap();
    w.index("bob", "s").await.unwrap();
    // A share of `media` (owned by alice) was revoked: only bob's rows go.
    hooks::connection_access_changed(&w.db.lock(), "media", "alice").unwrap();
    assert_eq!((w.segments("alice"), w.segments("bob")), (1, 0));
    // `media` was reconfigured or deleted: everyone's rows go.
    hooks::connection_changed(&w.db.lock(), "media").unwrap();
    assert_eq!(w.segments("alice"), 0);
}

// ---- Excerpts come from the file ----

#[tokio::test]
async fn text_excerpts_are_derived_from_the_uploaded_bytes() {
    let w = World::new();
    // BOM, CRLF and non-ASCII: offsets are code points of the cleaned text.
    let raw = "\u{feff}Café menu.\r\nThe invoice total is due.";
    w.upload("a", "alice", None, "a.txt", "text/plain", raw.into());
    w.index("alice", "a").await.unwrap();
    w.fx.nouls
        .lock()
        .unwrap()
        .insert("The invoice total is due.".into(), 0.9);
    let s = w
        .search("alice", true, Scope::Personal, None, "invoice", None)
        .await
        .unwrap();
    let top = &s.results[0];
    assert_eq!(top.excerpt.as_deref(), Some("The invoice total is due."));
    let clean = protocol::clean_text(raw.as_bytes()).unwrap();
    assert_eq!(
        char_slice(&clean, top.locator.start.unwrap(), top.locator.end.unwrap()),
        "The invoice total is due."
    );
    assert_eq!(top.score_kind, ScoreKind::Classifier);
}

#[tokio::test]
async fn fabricated_text_is_never_shown_or_judged() {
    let w = World::new();
    w.upload(
        "a",
        "alice",
        None,
        "a.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    *w.fx.fabricate.lock().unwrap() = true;
    w.index("alice", "a").await.unwrap();
    w.fx.nouls
        .lock()
        .unwrap()
        .insert("FAKE invoice summary".into(), 1.0);
    let s = w
        .search("alice", true, Scope::Personal, None, "invoice", None)
        .await
        .unwrap();
    assert!(!s.results.is_empty());
    assert!(s.results.iter().all(|r| r.excerpt.is_none()));
    assert!(w.fx.decided().is_empty(), "nothing went to System One");
    assert_eq!(s.ranking, Ranking::Embeddings);
}

#[test]
fn text_excerpt_rules() {
    let loc = |a, b| Locator {
        kind: LocatorKind::Text,
        start: Some(a),
        end: Some(b),
        page: None,
        start_ms: None,
        end_ms: None,
    };
    let t = "ab  cd\nef";
    assert_eq!(
        protocol::text_excerpt(t, &loc(0, 6), None).as_deref(),
        Some("ab cd")
    );
    assert_eq!(
        protocol::text_excerpt(t, &loc(0, 6), Some("ab cd")).as_deref(),
        Some("ab cd")
    );
    assert_eq!(
        protocol::text_excerpt(t, &loc(0, 6), Some("ab c…")).as_deref(),
        Some("ab cd"),
        "a cut prefix agrees"
    );
    assert_eq!(protocol::text_excerpt(t, &loc(0, 6), Some("xy")), None);
    assert_eq!(
        protocol::text_excerpt(t, &loc(0, 99), None),
        None,
        "outside the text"
    );
    assert_eq!(protocol::text_excerpt(t, &loc(2, 4), None), None, "blank");
    let no_range = Locator {
        start: None,
        end: None,
        ..loc(0, 0)
    };
    assert_eq!(protocol::text_excerpt(t, &no_range, Some("ab")), None);
    assert_eq!(
        protocol::clean_text(b"\xef\xbb\xbfa\r\nb\x07\rc\xe2\x80\x8bd").as_deref(),
        Some("a\nb\ncd")
    );
    assert_eq!(protocol::clean_text(b"\xff"), None);
    // A text file must have text locators.
    let page = json!({"model": "m", "partial": false, "data": [
        {"index": 0, "embedding": [1.0], "locator": {"kind": "page", "page": 1}}]});
    assert!(protocol::read_asset(&page, Kind::Document, Some("x")).is_err());
}

// ---- Search ACL while a call is held ----

#[tokio::test]
async fn removed_while_the_query_embeds_gets_nothing() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload(
        "s",
        "alice",
        Some(&sid),
        "s.txt",
        "text/plain",
        "The invoice total.".into(),
    );
    w.index("bob", "s").await.unwrap();
    let (db, s2) = (w.db.clone(), sid.clone());
    *w.fx.on_query.lock().unwrap() = Some(Box::new(move || {
        db.remove_member(&s2, "bob").unwrap();
        None
    }));
    w.fx.nouls
        .lock()
        .unwrap()
        .insert("The invoice total.".into(), 0.9);
    let s = w
        .search("bob", true, Scope::Space, Some(&sid), "invoice", None)
        .await
        .unwrap();
    assert!(s.results.is_empty());
    assert!(w.fx.decided().is_empty(), "no excerpt left for System One");
    assert_eq!(w.segments("bob"), 0);
}

#[tokio::test]
async fn failed_query_after_membership_loss_reads_as_not_found() {
    let w = World::new();
    let sid = w.space("alice", &["bob"]);
    w.upload(
        "s",
        "alice",
        Some(&sid),
        "s.txt",
        "text/plain",
        INVOICE_DOC.into(),
    );
    w.index("bob", "s").await.unwrap();
    // A provider failure while access still holds stays an upstream error.
    *w.fx.on_query.lock().unwrap() =
        Some(Box::new(|| Some(MediaError::transient("endpoint offline"))));
    assert!(matches!(
        w.search("bob", false, Scope::Space, Some(&sid), "invoice", None)
            .await,
        Err(LibrarySearchError::Upstream(_))
    ));
    *w.fx.on_query.lock().unwrap() = Some(Box::new(|| {
        Some(MediaError::internal("relay database fault"))
    }));
    let e = w
        .search("bob", false, Scope::Space, Some(&sid), "invoice", None)
        .await
        .unwrap_err();
    assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(e.message(), "internal error");
    // The typed provider withholds this result after Bob is removed.
    let (db, space) = (w.db.clone(), sid.clone());
    *w.fx.on_query.lock().unwrap() = Some(Box::new(move || {
        db.remove_member(&space, "bob").unwrap();
        Some(MediaError::gone("access revoked"))
    }));
    assert!(matches!(
        w.search("bob", false, Scope::Space, Some(&sid), "invoice", None)
            .await,
        Err(LibrarySearchError::NotFound("space"))
    ));
    assert!(w.fx.decided().is_empty());
    assert_eq!(w.segments("bob"), 0);
}

#[test]
fn docx_is_on_the_protocol_allowlist() {
    assert!(metro_model_protocol::valid_mime_type(DOCX));
    assert!(metro_model_protocol::valid_mime_type(protocol::PDF_MIME));
}
