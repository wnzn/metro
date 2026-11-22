//! Media embeddings: Metro's own typed wire for embedding models that take
//! documents and media natively (EmbeddingGemma 2 and compatible services).
//!
//! This is **not** OpenAI's `/embeddings`: no standard embeddings API is
//! assumed to accept media. It is served by a self-hosted model service
//! that implements exactly this contract:
//!
//! - `POST {base}/media/embeddings` (`base` includes `/v1`) with
//!   `{model, query: TEXT}` or
//!   `{model, asset: {name: BASENAME, mime_type: TYPE/SUBTYPE, data_base64}}`;
//! - answers `{model?, data: [{index, embedding: [f], locator, modality?,
//!   excerpt?}], partial: bool, notes?: [string]}`.
//!
//! `modality` (`text`, `image`, `audio`, `video`) says what was embedded
//! and must fit the locator (a scanned PDF page is `page` + `image`; a
//! video clip `time` + `video`). `notes` are the service's remarks about
//! what it could not embed (such as "video: visual frames only; the audio
//! track is not embedded"): untrusted display text, sanitized and bounded,
//! never instructions. Non-text rows (images, scanned pages, clips) carry no
//! `excerpt`. Any other row or top-level field is dropped.
//!
//! A query answers exactly one row with locator `{kind: "query"}`. A
//! document (asset) answers 1..=[`MEDIA_MAX_ROWS`] rows, each with a typed
//! locator saying where in the asset it comes from:
//!
//! | `kind`  | fields                                   |
//! |---------|------------------------------------------|
//! | `text`  | optional `start`, `end` (both or neither) |
//! | `page`  | `page` (1-based), optional `start`, `end` |
//! | `image` | none (the whole image)                    |
//! | `time`  | `start_ms`, `end_ms`                      |
//!
//! `partial: true` says the service embedded only part of the asset (it was
//! truncated). The model (`google/embeddinggemma-2` by default) takes
//! text, images, audio and video. Assets are exactly the types in
//! [`MEDIA_MIME_TYPES`]. PDF and DOCX are sent as their bytes for the model
//! service to process, never to the model raw: it extracts (PDF and DOCX)
//! or renders (PDF) the pages first and answers `page` or `text` locators.
//! Role prompts (query vs document) are the model service's business:
//! Metro sends the role only as the shape of the request.
//!
//! Assets travel as bytes only: never a URL or a file path, so the model
//! service never fetches anything. Names are bare basenames. Every check is
//! strict, and a response that does not fit is an error, never a guess.

use std::collections::HashSet;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::ClientError;
use crate::client::{R, bad_request, call_bytes, model_field, only_keys, reported_model};

/// Largest decoded asset.
pub const MEDIA_MAX_ASSET_BYTES: usize = 8 * 1024 * 1024;
/// Longest query text, in bytes.
pub const MEDIA_MAX_QUERY_BYTES: usize = 32 * 1024;
/// Longest asset name (a basename), in bytes.
pub const MEDIA_MAX_NAME_BYTES: usize = 255;
/// Longest MIME type.
pub const MEDIA_MAX_MIME_BYTES: usize = 127;
/// Largest serialized request: the base64 of the largest asset plus a small
/// envelope (~10.7 MiB of base64 for 8 MiB of bytes).
pub const MEDIA_MAX_REQUEST_BYTES: usize = 11 * 1024 * 1024;
/// Most rows one response may have.
pub const MEDIA_MAX_ROWS: usize = 48;
/// Largest vector accepted.
pub const MEDIA_MAX_DIMENSIONS: usize = 4096;
/// Dimensions of a standard service: 48 rows of 768 floats fit the device
/// relay's 2 MiB result bound with room for excerpts.
pub const MEDIA_STANDARD_DIMENSIONS: usize = 768;
/// Longest excerpt, in bytes.
pub const MEDIA_MAX_EXCERPT_BYTES: usize = 2048;
/// Largest response read from a server-side endpoint.
pub const MEDIA_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Largest position in a locator (characters, pages or milliseconds).
pub const MEDIA_MAX_POSITION: u64 = 1 << 53;
/// Highest page number.
pub const MEDIA_MAX_PAGE: u64 = 100_000;
/// Most service notes kept from one response, and the longest kept note.
pub const MEDIA_MAX_NOTES: usize = 16;
pub const MEDIA_MAX_NOTE_CHARS: usize = 300;
/// What a row's optional `modality` may be.
pub const MEDIA_MODALITIES: [&str; 4] = ["text", "image", "audio", "video"];

const MEDIA_TIMEOUT: Duration = Duration::from_secs(240);

/// PDF. The model has no native PDF input: the model service extracts or
/// renders the pages first and answers `page` (or `text`) locators.
pub const MEDIA_PDF: &str = "application/pdf";
/// Word (DOCX). The model service extracts its text first.
pub const MEDIA_DOCX: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// Exactly the asset types Metro sends, by their canonical names: the same
/// list the library accepts for upload and the reference model service
/// processes (TXT, Markdown, PDF, DOCX, PNG, JPEG, WAV, MP3, MP4, WebM).
/// Nothing else: no SVG, HTML, XML or other formats that can reference
/// remote or active content, and no blanket `image/*`.
pub const MEDIA_MIME_TYPES: [&str; 10] = [
    "text/plain",
    "text/markdown",
    MEDIA_PDF,
    MEDIA_DOCX,
    "image/png",
    "image/jpeg",
    "audio/wav",
    "audio/mpeg",
    "video/mp4",
    "video/webm",
];

/// Other names in use for the same formats, and their canonical name.
const MEDIA_MIME_ALIASES: [(&str, &str); 6] = [
    ("text/x-markdown", "text/markdown"),
    ("audio/x-wav", "audio/wav"),
    ("audio/wave", "audio/wav"),
    ("audio/vnd.wave", "audio/wav"),
    ("audio/mp3", "audio/mpeg"),
    ("image/jpg", "image/jpeg"),
];

fn bad_media(what: &str) -> ClientError {
    ClientError::new(format!(
        "the model endpoint returned an invalid media embeddings response: {what}"
    ))
}

/// What is embedded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaInput {
    /// A search query (text).
    Query(String),
    /// A document: bytes with a basename and a MIME type.
    Asset {
        name: String,
        mime_type: String,
        /// Canonical base64 of 1..=[`MEDIA_MAX_ASSET_BYTES`] bytes.
        data_base64: String,
        /// Decoded length.
        size: usize,
    },
}

/// A checked media request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaRequest {
    pub model: String,
    pub input: MediaInput,
}

impl MediaRequest {
    pub fn is_query(&self) -> bool {
        matches!(self.input, MediaInput::Query(_))
    }

    /// The exact length of the request body, counted while serializing
    /// into nothing: no copy of the asset is made.
    pub fn encoded_len(&self) -> usize {
        let mut counter = Counter(0);
        match serde_json::to_writer(&mut counter, self) {
            Ok(()) => counter.0,
            Err(_) => usize::MAX,
        }
    }

    /// The request body as JSON, moving the asset instead of copying it.
    pub fn into_json(self) -> Value {
        match self.input {
            MediaInput::Query(q) => json!({"model": self.model, "query": q}),
            MediaInput::Asset {
                name,
                mime_type,
                data_base64,
                ..
            } => json!({"model": self.model, "asset": {
                "name": name, "mime_type": mime_type, "data_base64": data_base64,
            }}),
        }
    }

    /// The same request without the asset's bytes, which is all
    /// [`check_media_response`] needs; the bytes can then be sent on with
    /// [`Self::into_json`] without being copied.
    pub fn shape(&self) -> MediaRequest {
        MediaRequest {
            model: self.model.clone(),
            input: match &self.input {
                MediaInput::Query(q) => MediaInput::Query(q.clone()),
                MediaInput::Asset {
                    name,
                    mime_type,
                    size,
                    ..
                } => MediaInput::Asset {
                    name: name.clone(),
                    mime_type: mime_type.clone(),
                    data_base64: String::new(),
                    size: *size,
                },
            },
        }
    }

    /// The request body, exactly as it is sent (a copy; see
    /// [`Self::into_json`]).
    pub fn to_json(&self) -> Value {
        match &self.input {
            MediaInput::Query(q) => json!({"model": self.model, "query": q}),
            MediaInput::Asset {
                name,
                mime_type,
                data_base64,
                ..
            } => json!({"model": self.model, "asset": {
                "name": name, "mime_type": mime_type, "data_base64": data_base64,
            }}),
        }
    }
}

/// Serialized borrowing every field, so the body can be written or counted
/// without building a copy.
impl serde::Serialize for MediaRequest {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        #[derive(serde::Serialize)]
        struct Asset<'a> {
            name: &'a str,
            mime_type: &'a str,
            data_base64: &'a str,
        }
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("model", &self.model)?;
        match &self.input {
            MediaInput::Query(q) => m.serialize_entry("query", q)?,
            MediaInput::Asset {
                name,
                mime_type,
                data_base64,
                ..
            } => m.serialize_entry(
                "asset",
                &Asset {
                    name,
                    mime_type,
                    data_base64,
                },
            )?,
        }
        m.end()
    }
}

/// Counts bytes written to it.
struct Counter(usize);

impl std::io::Write for Counter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The length of `v` as compact JSON, counted without building it.
pub fn json_len(v: &Value) -> usize {
    let mut counter = Counter(0);
    match serde_json::to_writer(&mut counter, v) {
        Ok(()) => counter.0,
        Err(_) => usize::MAX,
    }
}

/// A bare file name: no directory part, no drive or scheme, no control
/// characters, not `.` or `..`.
pub fn valid_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MEDIA_MAX_NAME_BYTES
        && name != "."
        && name != ".."
        && name.trim() == name
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '\0'))
}

/// Whether `m` is one of [`MEDIA_MIME_TYPES`], exactly (canonical name,
/// lowercase, no parameters). This is what the wire accepts.
pub fn valid_mime_type(m: &str) -> bool {
    MEDIA_MIME_TYPES.contains(&m)
}

/// The canonical wire name of a MIME type, or `None` when Metro does not
/// send that format: trimmed, lowercased, parameters dropped
/// (`Text/Plain; charset=utf-8` → `text/plain`) and known aliases mapped
/// (`audio/x-wav` → `audio/wav`). For senders only; the wire check
/// ([`valid_mime_type`]) takes canonical names alone.
pub fn canonical_mime_type(m: &str) -> Option<&'static str> {
    let base = m
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    MEDIA_MIME_TYPES
        .iter()
        .find(|t| **t == base)
        .copied()
        .or_else(|| {
            MEDIA_MIME_ALIASES
                .iter()
                .find(|(alias, _)| *alias == base)
                .map(|(_, canonical)| *canonical)
        })
}

/// Check a media request: `{model, query}` or `{model, asset}` and nothing
/// else. The asset's base64 is decoded to check it and its size; it must
/// be canonical (standard alphabet, padded, no whitespace).
///
/// Copies the asset; [`parse_media_request`] takes the request by value
/// and moves it instead.
pub fn check_media_request(request: &Value) -> R<MediaRequest> {
    parse_media_request(request.clone())
}

/// [`check_media_request`], taking the request by value: the asset's
/// base64 is moved into the result, not copied, and it is checked in
/// fixed-size pieces, so no decoded copy of the asset is made either.
pub fn parse_media_request(request: Value) -> R<MediaRequest> {
    let Value::Object(mut obj) = request else {
        return Err(bad_request("the request must be an object"));
    };
    only_keys(&obj, &["model", "query", "asset"], "the request")?;
    let model = model_field(&obj)?;
    let input = match (obj.remove("query"), obj.remove("asset")) {
        (Some(q), None) => {
            let Value::String(q) = q else {
                return Err(bad_request("'query' must be a string"));
            };
            if q.trim().is_empty() || q.len() > MEDIA_MAX_QUERY_BYTES {
                return Err(bad_request(&format!(
                    "'query' must be 1..={MEDIA_MAX_QUERY_BYTES} bytes of text"
                )));
            }
            MediaInput::Query(q)
        }
        (None, Some(a)) => check_asset(a)?,
        _ => return Err(bad_request("send exactly one of 'query' or 'asset'")),
    };
    let req = MediaRequest { model, input };
    if req.encoded_len() > MEDIA_MAX_REQUEST_BYTES {
        return Err(bad_request("the request is too large"));
    }
    Ok(req)
}

/// The decoded length of canonical standard base64 (padded, no
/// whitespace, zero trailing bits), checked in pieces of a fixed buffer.
fn canonical_base64_len(data: &str) -> Option<usize> {
    // A multiple of 4 characters decodes on its own; padding may only be
    // in the last piece.
    const PIECE: usize = 64 * 1024;
    let mut buf = vec![0u8; PIECE / 4 * 3];
    let mut total = 0usize;
    if !data.len().is_multiple_of(4) {
        return None;
    }
    for piece in data.as_bytes().chunks(PIECE) {
        let n = base64::engine::general_purpose::STANDARD
            .decode_slice(piece, &mut buf)
            .ok()?;
        total += n;
    }
    Some(total)
}

fn check_asset(a: Value) -> R<MediaInput> {
    let Value::Object(mut a) = a else {
        return Err(bad_request("'asset' must be an object"));
    };
    only_keys(&a, &["name", "mime_type", "data_base64"], "the asset")?;
    let name = a
        .get("name")
        .and_then(Value::as_str)
        .filter(|n| valid_asset_name(n))
        .ok_or_else(|| bad_request("the asset 'name' must be a bare file name"))?;
    let mime_type = a
        .get("mime_type")
        .and_then(Value::as_str)
        .filter(|m| valid_mime_type(m))
        .ok_or_else(|| {
            bad_request(
                "the asset 'mime_type' must be one of the supported types (TXT, Markdown, PDF, \
                 DOCX, PNG, JPEG, WAV, MP3, MP4, WebM) by its canonical name",
            )
        })?;
    let (name, mime_type) = (name.to_owned(), mime_type.to_owned());
    let Some(Value::String(data)) = a.remove("data_base64") else {
        return Err(bad_request("the asset needs 'data_base64'"));
    };
    // Bound the work before decoding anything.
    if data.is_empty() || data.len() > MEDIA_MAX_ASSET_BYTES.div_ceil(3) * 4 {
        return Err(bad_request(&format!(
            "the asset must be 1..={MEDIA_MAX_ASSET_BYTES} bytes"
        )));
    }
    // The STANDARD engine requires canonical padding and zero trailing
    // bits, so one byte string has exactly one accepted encoding.
    let size = canonical_base64_len(&data)
        .ok_or_else(|| bad_request("'data_base64' is not canonical base64"))?;
    if size == 0 || size > MEDIA_MAX_ASSET_BYTES {
        return Err(bad_request(&format!(
            "the asset must be 1..={MEDIA_MAX_ASSET_BYTES} bytes"
        )));
    }
    Ok(MediaInput::Asset {
        name,
        mime_type,
        data_base64: data,
        size,
    })
}

fn position(loc: &Map<String, Value>, key: &str) -> R<Option<u64>> {
    match loc.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|n| *n <= MEDIA_MAX_POSITION)
            .map(Some)
            .ok_or_else(|| bad_media("a locator position is not a non-negative integer")),
    }
}

/// Optional `start`/`end` text offsets: both (`start <= end`) or neither.
fn range(loc: &Map<String, Value>, out: &mut Value) -> R<()> {
    match (position(loc, "start")?, position(loc, "end")?) {
        (None, None) => Ok(()),
        (Some(s), Some(e)) if s <= e => {
            out["start"] = json!(s);
            out["end"] = json!(e);
            Ok(())
        }
        _ => Err(bad_media("a text range needs start <= end, or neither")),
    }
}

/// A locator rebuilt with only its kind's fields.
fn check_locator(v: Option<&Value>, query: bool) -> R<Value> {
    let loc = v
        .and_then(Value::as_object)
        .ok_or_else(|| bad_media("a row has no locator object"))?;
    let kind = loc
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_media("a locator has no kind"))?;
    let allowed: &[&str] = match kind {
        "query" => &["kind"],
        "text" => &["kind", "start", "end"],
        "page" => &["kind", "page", "start", "end"],
        "image" => &["kind"],
        "time" => &["kind", "start_ms", "end_ms"],
        _ => return Err(bad_media("a locator has an unknown kind")),
    };
    if loc.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(bad_media("a locator has a field its kind doesn't have"));
    }
    if query != (kind == "query") {
        return Err(bad_media(if query {
            "a query must answer with a query locator"
        } else {
            "a document row has a query locator"
        }));
    }
    Ok(match kind {
        "text" => {
            let mut out = json!({"kind": "text"});
            range(loc, &mut out)?;
            out
        }
        "page" => {
            let p = position(loc, "page")?
                .filter(|p| (1..=MEDIA_MAX_PAGE).contains(p))
                .ok_or_else(|| bad_media("a page locator needs a page number"))?;
            let mut out = json!({"kind": "page", "page": p});
            range(loc, &mut out)?;
            out
        }
        "time" => match (position(loc, "start_ms")?, position(loc, "end_ms")?) {
            (Some(s), Some(e)) if s <= e => {
                json!({"kind": "time", "start_ms": s, "end_ms": e})
            }
            _ => return Err(bad_media("a time locator needs start_ms <= end_ms")),
        },
        other => json!({ "kind": other }),
    })
}

/// The modality a row may name for its locator kind.
fn modality_fits(modality: &str, locator_kind: &str) -> bool {
    matches!(
        (locator_kind, modality),
        ("query" | "text", "text")
            | ("page", "text" | "image")
            | ("image", "image")
            | ("time", "audio" | "video")
    )
}

/// An optional row `modality` (`text`, `image`, `audio`, `video`): kept
/// when it fits the locator (a scanned PDF page is `page` + `image`, a
/// clip `time` + `audio` or `video`), refused otherwise.
fn check_modality(v: Option<&Value>, locator: &Value) -> R<Option<&'static str>> {
    let kind = locator["kind"].as_str().unwrap_or("");
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(m)) => MEDIA_MODALITIES
            .iter()
            .find(|x| **x == m.as_str() && modality_fits(m, kind))
            .map(|x| Some(*x))
            .ok_or_else(|| bad_media("a row's modality doesn't fit its locator")),
        Some(_) => Err(bad_media("a row's modality is not a string")),
    }
}

/// Optional top-level `notes`: service remarks such as "video: visual
/// frames only; the audio track is not embedded". They are untrusted
/// display text: an array of strings, each stripped of control characters,
/// whitespace collapsed and cut to [`MEDIA_MAX_NOTE_CHARS`]; at most
/// [`MEDIA_MAX_NOTES`] are kept. Anything that is not an array of strings
/// is refused.
fn check_notes(v: Option<&Value>) -> R<Vec<String>> {
    let items = match v {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(bad_media("'notes' must be an array of strings")),
    };
    let mut out = Vec::new();
    for item in items {
        let s = item
            .as_str()
            .ok_or_else(|| bad_media("'notes' must be an array of strings"))?;
        if out.len() >= MEDIA_MAX_NOTES {
            continue;
        }
        let clean: String = s
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let clean = if clean.chars().count() > MEDIA_MAX_NOTE_CHARS {
            let mut cut: String = clean.chars().take(MEDIA_MAX_NOTE_CHARS - 1).collect();
            cut.push('…');
            cut
        } else {
            clean
        };
        if !clean.is_empty() {
            out.push(clean);
        }
    }
    Ok(out)
}

fn check_excerpt(v: Option<&Value>) -> R<Option<String>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s))
            if s.len() <= MEDIA_MAX_EXCERPT_BYTES
                && !s
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t')) =>
        {
            Ok(Some(s.clone()))
        }
        Some(_) => Err(bad_media("an excerpt is not short plain text")),
    }
}

/// Check a media response against its request and rebuild it, sorted by
/// index, as `{model, data: [{index, embedding, locator, modality?,
/// excerpt?}], partial, notes?}` (see the module docs for `modality` and
/// `notes`; any other field is dropped).
/// Every row: a unique index (together 0..n), a finite non-zero
/// vector of the same length as the others (1..=`max_dimensions`), a
/// locator of its kind. A query has exactly one row.
pub fn check_media_response(
    request: &MediaRequest,
    response: &Value,
    max_dimensions: usize,
) -> R<Value> {
    let max_dimensions = max_dimensions.min(MEDIA_MAX_DIMENSIONS);
    let obj = response
        .as_object()
        .ok_or_else(|| bad_media("not an object"))?;
    let data = obj
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_media("no data"))?;
    let query = request.is_query();
    if query && data.len() != 1 {
        return Err(bad_media("a query must answer exactly one vector"));
    }
    if data.is_empty() || data.len() > MEDIA_MAX_ROWS {
        return Err(bad_media("the number of rows is out of range"));
    }
    let partial = obj
        .get("partial")
        .and_then(Value::as_bool)
        .ok_or_else(|| bad_media("'partial' must be a boolean"))?;
    let notes = check_notes(obj.get("notes"))?;
    let n = data.len();
    let mut slots: Vec<Option<Value>> = vec![None; n];
    let mut seen = HashSet::new();
    let mut dims: Option<usize> = None;
    for item in data {
        let item = item
            .as_object()
            .ok_or_else(|| bad_media("a row is not an object"))?;
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|i| i as usize)
            .filter(|i| *i < n)
            .ok_or_else(|| bad_media("an index is missing or out of range"))?;
        if !seen.insert(index) {
            return Err(bad_media("an index appears twice"));
        }
        let values = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| bad_media("an embedding is not a float array"))?;
        if values.is_empty() || values.len() > max_dimensions {
            return Err(bad_media("an embedding has an unsupported length"));
        }
        if dims.is_some_and(|d| d != values.len()) {
            return Err(bad_media("the embeddings have different lengths"));
        }
        dims = Some(values.len());
        let mut norm = 0f64;
        let mut vector = Vec::with_capacity(values.len());
        for v in values {
            let x = v
                .as_f64()
                .filter(|x| x.is_finite() && x.abs() <= f32::MAX as f64)
                .ok_or_else(|| bad_media("an embedding has a non-finite value"))?;
            norm += x * x;
            vector.push(x);
        }
        if norm.sqrt() < crate::EMBED_MIN_NORM {
            return Err(bad_media("an embedding is all zeros"));
        }
        let locator = check_locator(item.get("locator"), query)?;
        let modality = check_modality(item.get("modality"), &locator)?;
        let mut row = json!({"index": index, "embedding": vector, "locator": locator});
        if let Some(m) = modality {
            row["modality"] = json!(m);
        }
        if let Some(e) = check_excerpt(item.get("excerpt"))? {
            row["excerpt"] = json!(e);
        }
        slots[index] = Some(row);
    }
    let rows: Vec<Value> = slots.into_iter().flatten().collect();
    if rows.len() != n {
        return Err(bad_media("the indexes are not contiguous"));
    }
    let mut out = json!({
        "model": reported_model(obj, &request.model),
        "data": rows,
        "partial": partial,
    });
    if !notes.is_empty() {
        out["notes"] = json!(notes);
    }
    Ok(out)
}

/// One media embeddings call on a server-side endpoint: `request` is
/// `{model, query}` or `{model, asset}`; the result is the validated,
/// rebuilt response (see [`check_media_response`]).
pub async fn media_embed(base: &str, key: Option<&str>, request: Value) -> anyhow::Result<Value> {
    media_embed_request(base, key, parse_media_request(request)?).await
}

/// [`media_embed`] with a request already checked; the body is written
/// once, straight from the request.
pub async fn media_embed_request(
    base: &str,
    key: Option<&str>,
    request: MediaRequest,
) -> anyhow::Result<Value> {
    let body = serde_json::to_vec(&request).map_err(|_| bad_request("cannot encode"))?;
    let shape = request.shape();
    drop(request);
    let resp = call_bytes(
        reqwest::Method::POST,
        base,
        "/media/embeddings",
        key,
        Some(body),
        MEDIA_TIMEOUT,
        MEDIA_MAX_RESPONSE_BYTES,
    )
    .await?;
    Ok(check_media_response(&shape, &resp, MEDIA_MAX_DIMENSIONS)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    /// Bytes that are not text: a PNG signature and a NUL.
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff];

    fn asset_req() -> Value {
        json!({"model": "google/embeddinggemma-2", "asset": {
            "name": "scan.png", "mime_type": "image/png", "data_base64": b64(PNG)}})
    }

    fn row(i: usize, loc: Value) -> Value {
        json!({"index": i, "embedding": [0.6, 0.8, 0.0], "locator": loc})
    }

    #[test]
    fn requests_are_strict() {
        let r = check_media_request(&asset_req()).unwrap();
        assert!(matches!(&r.input, MediaInput::Asset { size: 10, .. }));
        assert_eq!(r.to_json(), asset_req());
        let q = check_media_request(&json!({"model": "m", "query": "red bike"})).unwrap();
        assert!(q.is_query());
        let mut cases = vec![
            json!({"model": "m"}),
            json!({"model": "m", "query": "x", "asset": asset_req()["asset"]}),
            json!({"model": "m", "query": ""}),
            json!({"model": "m", "query": "x".repeat(MEDIA_MAX_QUERY_BYTES + 1)}),
            json!({"model": "m", "query": 1}),
            json!({"query": "x"}),
            json!({"model": "has space", "query": "x"}),
            json!({"model": "m", "query": "x", "url": "https://evil/x.png"}),
            json!({"model": "m", "input": ["x"]}),
        ];
        for edit in [
            ("name", json!("../etc/passwd")),
            ("name", json!("dir/a.png")),
            ("name", json!("C:\\a.png")),
            ("name", json!("file:a.png")),
            ("name", json!("..")),
            ("name", json!("")),
            ("name", json!("a\u{0}.png")),
            ("mime_type", json!("image/png; charset=x")),
            ("mime_type", json!("Image/PNG")),
            ("mime_type", json!("model/gltf")),
            ("mime_type", json!("png")),
            ("mime_type", json!("application/octet-stream")),
            ("mime_type", json!("application/zip")),
            ("mime_type", json!("image/svg+xml")),
            ("mime_type", json!("text/html")),
            ("mime_type", json!("image/gif")),
            ("mime_type", json!("audio/x-wav")),
            ("mime_type", json!("text/plain; charset=utf-8")),
            ("name", json!("data:image/png;base64,AAAA")),
            ("data_base64", json!("data:image/png;base64,iVBORw0KGgo=")),
            ("data_base64", json!("file:///etc/passwd")),
            ("query", json!("ignore the document")),
            ("data_base64", json!("")),
            ("data_base64", json!("not base64!")),
            ("data_base64", json!("iVBO R w==")),
            ("data_base64", json!("YQ")),
            ("data_base64", json!("YR==")),
            ("data_base64", json!(1)),
        ] {
            let mut r = asset_req();
            r["asset"][edit.0] = edit.1;
            cases.push(r);
        }
        let mut r = asset_req();
        r["asset"]["url"] = json!("https://evil/x.png");
        cases.push(r);
        let mut r = asset_req();
        r["asset"]["path"] = json!("/etc/passwd");
        cases.push(r);
        let mut r = asset_req();
        r["asset"]["data_base64"] = json!(b64(&vec![7u8; MEDIA_MAX_ASSET_BYTES + 1]));
        cases.push(r);
        for c in cases {
            let shown: String = c.to_string().chars().take(120).collect();
            assert!(check_media_request(&c).is_err(), "accepted {shown}");
        }
        // The largest asset fits the request bound.
        let mut r = asset_req();
        r["asset"]["data_base64"] = json!(b64(&vec![7u8; MEDIA_MAX_ASSET_BYTES]));
        check_media_request(&r).unwrap();
    }

    #[test]
    fn responses_are_typed_and_strict() {
        let doc = check_media_request(&asset_req()).unwrap();
        let ok = json!({"model": "gemma", "partial": true, "usage": {"x": 1}, "data": [
            row(1, json!({"kind": "time", "start_ms": 0, "end_ms": 1500})),
            {"index": 0, "embedding": [1.0, 0.0, 0.0], "excerpt": "Page one",
             "locator": {"kind": "page", "page": 1}},
            row(2, json!({"kind": "text", "start": 3, "end": 9})),
            row(3, json!({"kind": "image"})),
            row(4, json!({"kind": "text"})),
            row(5, json!({"kind": "page", "page": 3, "start": 0, "end": 40})),
            row(6, json!({"kind": "time", "start_ms": 700, "end_ms": 700})),
        ]});
        let out = check_media_response(&doc, &ok, MEDIA_MAX_DIMENSIONS).unwrap();
        assert_eq!(out["partial"], true);
        assert_eq!(out["model"], "gemma");
        assert!(out.get("usage").is_none());
        assert_eq!(
            out["data"][0]["locator"],
            json!({"kind": "page", "page": 1})
        );
        assert_eq!(out["data"][0]["excerpt"], "Page one");
        assert_eq!(out["data"][1]["locator"]["end_ms"], 1500);
        assert_eq!(out["data"].as_array().unwrap().len(), 7);
        assert_eq!(
            out["data"][5]["locator"],
            json!({"kind": "page", "page": 3, "start": 0, "end": 40})
        );

        let q = check_media_request(&json!({"model": "m", "query": "q"})).unwrap();
        let qok = json!({"partial": false, "data": [row(0, json!({"kind": "query"}))]});
        assert_eq!(
            check_media_response(&q, &qok, MEDIA_MAX_DIMENSIONS).unwrap()["model"],
            "m"
        );
        // A query answers exactly one query row.
        for bad in [
            json!({"partial": false, "data": [row(0, json!({"kind": "query"})), row(1, json!({"kind": "query"}))]}),
            json!({"partial": false, "data": [row(0, json!({"kind": "text"}))]}),
            json!({"partial": false, "data": []}),
        ] {
            assert!(
                check_media_response(&q, &bad, MEDIA_MAX_DIMENSIONS).is_err(),
                "{bad}"
            );
        }

        let rows = |r: Vec<Value>| json!({"partial": false, "data": r});
        let page = json!({"kind": "page", "page": 2});
        let mut bad = vec![
            rows(vec![row(0, json!({"kind": "query"}))]),
            rows(vec![row(0, json!({"kind": "page"}))]),
            rows(vec![row(0, json!({"kind": "page", "page": 0}))]),
            rows(vec![row(0, json!({"kind": "page", "page": 2, "url": "x"}))]),
            rows(vec![row(
                0,
                json!({"kind": "page", "page": 2, "start": 9, "end": 3}),
            )]),
            rows(vec![row(
                0,
                json!({"kind": "page", "page": 2, "start_ms": 1, "end_ms": 3}),
            )]),
            rows(vec![row(0, json!({"kind": "image", "page": 2}))]),
            rows(vec![row(
                0,
                json!({"kind": "time", "start_ms": 6, "end_ms": 5}),
            )]),
            rows(vec![row(0, json!({"kind": "time", "start_ms": 5}))]),
            rows(vec![row(
                0,
                json!({"kind": "time", "start_ms": -1, "end_ms": 5}),
            )]),
            rows(vec![row(0, json!({"kind": "text", "start": 3}))]),
            rows(vec![row(0, json!({"kind": "file"}))]),
            rows(vec![json!({"index": 0, "embedding": [1.0]})]),
            rows(vec![row(0, page.clone()), row(0, page.clone())]),
            rows(vec![row(0, page.clone()), row(2, page.clone())]),
            rows(vec![
                row(0, page.clone()),
                json!({"index": 1, "embedding": [1.0, 0.0], "locator": page}),
            ]),
            rows(vec![
                json!({"index": 0, "embedding": [0.0, 0.0], "locator": page}),
            ]),
            rows(vec![
                json!({"index": 0, "embedding": [1e300], "locator": page}),
            ]),
            rows(vec![
                json!({"index": 0, "embedding": "AAAA", "locator": page}),
            ]),
            rows(vec![
                json!({"index": 0, "embedding": [1.0], "locator": page,
                             "excerpt": "x".repeat(MEDIA_MAX_EXCERPT_BYTES + 1)}),
            ]),
            rows(vec![
                json!({"index": 0, "embedding": [1.0], "locator": page,
                             "excerpt": "bell\u{7}"}),
            ]),
            rows((0..=MEDIA_MAX_ROWS).map(|i| row(i, page.clone())).collect()),
            json!({"data": [row(0, page.clone())]}),
            json!({"partial": "no", "data": [row(0, page.clone())]}),
            json!({"partial": false}),
        ];
        bad.push(rows(vec![
            json!({"index": 0, "embedding": vec![0.5; 5000], "locator": page}),
        ]));
        for b in bad {
            let shown: String = b.to_string().chars().take(160).collect();
            assert!(
                check_media_response(&doc, &b, MEDIA_MAX_DIMENSIONS).is_err(),
                "accepted {shown}"
            );
        }
        // A caller may bound dimensions lower (the device relay does).
        let wide = rows(vec![
            json!({"index": 0, "embedding": vec![0.5; 1024], "locator": page}),
        ]);
        assert!(check_media_response(&doc, &wide, MEDIA_STANDARD_DIMENSIONS).is_err());
        assert!(check_media_response(&doc, &wide, MEDIA_MAX_DIMENSIONS).is_ok());
    }

    #[test]
    fn sizes_are_counted_without_copies() {
        for v in [
            asset_req(),
            json!({"model": "m", "query": "naïve \"quoted\" \u{1}"}),
        ] {
            let r = check_media_request(&v).unwrap();
            assert_eq!(
                r.encoded_len(),
                serde_json::to_vec(&r.to_json()).unwrap().len()
            );
            assert_eq!(r.encoded_len(), json_len(&v));
            assert_eq!(serde_json::to_value(&r).unwrap(), v);
            assert_eq!(r.shape().is_query(), r.is_query());
            assert_eq!(r.clone().into_json(), v);
        }
        // Long assets are checked piece by piece, padding only at the end.
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
        assert_eq!(canonical_base64_len(&b64(&bytes)), Some(bytes.len()));
        let mut split = b64(&bytes[..3]);
        split.push_str("YQ==");
        split.push_str(&b64(&bytes[..3]));
        assert_eq!(canonical_base64_len(&split), None);
    }

    #[test]
    fn names_and_types() {
        for ok in ["a.pdf", "Scan 01.png", "voice-memo.m4a", "é.txt", ".hidden"] {
            assert!(valid_asset_name(ok), "{ok}");
        }
        for ok in MEDIA_MIME_TYPES {
            assert!(valid_mime_type(ok), "{ok}");
        }
        for bad in [
            "image/svg+xml",
            "text/html",
            "image/gif",
            "image/webp",
            "audio/ogg",
            "application/octet-stream",
            "text/plain/x",
            " text/plain",
            "TEXT/PLAIN",
        ] {
            assert!(!valid_mime_type(bad), "{bad}");
        }
        assert_eq!(
            canonical_mime_type(" Text/Plain; charset=UTF-8"),
            Some("text/plain")
        );
        assert_eq!(canonical_mime_type("audio/x-wav"), Some("audio/wav"));
        assert_eq!(
            canonical_mime_type("text/x-markdown"),
            Some("text/markdown")
        );
        assert_eq!(canonical_mime_type("image/svg+xml"), None);
        assert_eq!(canonical_mime_type("text/html"), None);
    }
}
