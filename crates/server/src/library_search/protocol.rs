//! The self-hosted media embedding protocol, and the checks applied to
//! everything a media model answers.
//!
//! Request (the `input` passed to `providers::media_embed`, which adds the
//! connection's saved `model` and posts it to `POST /v1/media/embeddings`):
//!
//! ```json
//! { "query": "red sunset over water" }
//! { "asset": { "name": "trip.png", "mime_type": "image/png", "data_base64": "…" } }
//! ```
//!
//! Response:
//!
//! ```json
//! { "model": "…",
//!   "data": [ { "index": 0, "embedding": [0.1, …],
//!               "locator": { "kind": "page", "page": 2 }, "excerpt": "…" } ],
//!   "partial": false }
//! ```
//!
//! A query answer has exactly one item with locator kind `query`. An asset
//! answer has one item per segment, with locators that fit the file's kind:
//! `text` or `page` for documents, `image` for images, `time` for audio and
//! video. Anything else is refused as a whole: nothing from a malformed
//! answer is stored.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::{DEVICE_TOKEN_PREFIX, RUNNER_TOKEN_PREFIX};
use crate::search::vector;

/// Segments accepted for one file (the protocol's row bound).
pub const MAX_SEGMENTS_PER_ASSET: usize = metro_model_protocol::MEDIA_MAX_ROWS;
/// Longest stored excerpt, in characters.
pub const MAX_EXCERPT_CHARS: usize = 1_000;
/// Highest page number accepted.
pub const MAX_PAGE: u64 = metro_model_protocol::MEDIA_MAX_PAGE;
/// Largest text offset or timestamp accepted.
pub const MAX_POSITION: u64 = metro_model_protocol::MEDIA_MAX_POSITION;

/// What a file is, from its MIME type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Document,
    Image,
    Audio,
    Video,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Document => "document",
            Kind::Image => "image",
            Kind::Audio => "audio",
            Kind::Video => "video",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "document" => Some(Kind::Document),
            "image" => Some(Kind::Image),
            "audio" => Some(Kind::Audio),
            "video" => Some(Kind::Video),
            _ => None,
        }
    }

    /// The file kind of `mime` (parameters such as `charset` ignored), or
    /// `None` when Metro does not send that type to a media model.
    ///
    /// PDF and DOCX are documents. The model has no native PDF or DOCX
    /// input: Metro sends the file's genuine bytes, and the model service
    /// prepares them (extracts text, or renders pages without text) and
    /// answers `page` or `text` locators. Which preparation happens is the
    /// service's decision, never Metro's.
    pub fn of_mime(mime: &str) -> Option<Kind> {
        match base_mime(mime).as_str() {
            "text/plain" | "text/markdown" | "text/x-markdown" | PDF_MIME | DOCX_MIME => {
                Some(Kind::Document)
            }
            "image/png" | "image/jpeg" => Some(Kind::Image),
            "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" | "audio/mpeg"
            | "audio/mp3" => Some(Kind::Audio),
            "video/mp4" | "video/webm" => Some(Kind::Video),
            _ => None,
        }
    }

    fn allows(self, locator: LocatorKind) -> bool {
        matches!(
            (self, locator),
            (Kind::Document, LocatorKind::Text | LocatorKind::Page)
                | (Kind::Image, LocatorKind::Image)
                | (Kind::Audio | Kind::Video, LocatorKind::Time)
        )
    }
}

/// `mime` without parameters, lowercased: what is sent as `mime_type`.
pub fn base_mime(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// PDF, prepared by the model service (`metro_model_protocol::MEDIA_PDF`).
pub const PDF_MIME: &str = metro_model_protocol::MEDIA_PDF;
/// DOCX, whose text the model service extracts.
pub const DOCX_MIME: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// Why a file of `mime` is not indexed, for people.
pub fn unsupported_reason(mime: &str) -> String {
    format!(
        "{} files can't be indexed",
        crate::search::clip(&base_mime(mime), 100, "…")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocatorKind {
    Query,
    Text,
    Page,
    Image,
    Time,
}

/// Where in a file a segment is. Only the fields of its kind are set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Locator {
    pub kind: LocatorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u64>,
}

impl Locator {
    /// Is the locator well formed for its own kind? The same rules as
    /// `metro_model_protocol::check_media_response`: `text` has `start <=
    /// end` or neither, `page` a 1-based page and optionally such a range
    /// (character offsets in that page's extracted text), `time` `start_ms
    /// <= end_ms`.
    fn valid(&self) -> bool {
        let none = |v: Option<u64>| v.is_none();
        let range = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(a), Some(b)) => a <= b && b <= MAX_POSITION,
            (None, None) => true,
            _ => false,
        };
        match self.kind {
            LocatorKind::Query | LocatorKind::Image => {
                none(self.start)
                    && none(self.end)
                    && none(self.page)
                    && none(self.start_ms)
                    && none(self.end_ms)
            }
            LocatorKind::Text => {
                range(self.start, self.end)
                    && none(self.page)
                    && none(self.start_ms)
                    && none(self.end_ms)
            }
            LocatorKind::Page => {
                matches!(self.page, Some(p) if (1..=MAX_PAGE).contains(&p))
                    && range(self.start, self.end)
                    && none(self.start_ms)
                    && none(self.end_ms)
            }
            LocatorKind::Time => {
                self.start_ms.is_some()
                    && self.end_ms.is_some()
                    && range(self.start_ms, self.end_ms)
                    && none(self.start)
                    && none(self.end)
                    && none(self.page)
            }
        }
    }
}

/// The request embedding a search query.
pub fn query_request(query: &str) -> Value {
    json!({ "query": query })
}

/// The request embedding one file's own bytes.
pub fn asset_request(name: &str, mime_type: &str, bytes: &[u8]) -> Value {
    json!({
        "asset": {
            "name": name,
            "mime_type": base_mime(mime_type),
            "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    })
}

#[derive(Debug, Deserialize)]
struct Wire {
    model: String,
    data: Vec<WireItem>,
    partial: bool,
}

#[derive(Debug, Deserialize)]
struct WireItem {
    index: usize,
    embedding: Vec<f32>,
    locator: Locator,
    #[serde(default)]
    excerpt: Option<String>,
}

/// One checked, normalized segment of a file.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub seq: usize,
    pub vector: Vec<f32>,
    pub locator: Locator,
    pub excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssetAnswer {
    pub model: String,
    pub dims: usize,
    pub segments: Vec<Segment>,
    /// The model said it did not cover the whole file.
    pub partial: bool,
}

/// Why a media model's answer was refused. Messages are shown to people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError(pub String);

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn bad(what: &str) -> ProtocolError {
    ProtocolError(format!("the media embedding model returned {what}"))
}

fn parse(answer: &Value) -> Result<Wire, ProtocolError> {
    let wire: Wire =
        serde_json::from_value(answer.clone()).map_err(|_| bad("an invalid response"))?;
    if wire.model.trim().is_empty() {
        return Err(bad("no model name"));
    }
    if wire.data.is_empty() {
        return Err(bad("no embeddings"));
    }
    // Indices must be exactly 0..n, each once.
    let mut seen = vec![false; wire.data.len()];
    for item in &wire.data {
        match seen.get_mut(item.index) {
            Some(s) if !*s => *s = true,
            _ => return Err(bad("an invalid response (bad segment indices)")),
        }
    }
    Ok(wire)
}

/// The query vector of a query answer, normalized.
pub fn read_query(answer: &Value) -> Result<Vec<f32>, ProtocolError> {
    let wire = parse(answer)?;
    let [item] = <[WireItem; 1]>::try_from(wire.data)
        .map_err(|_| bad("more than one embedding for a query"))?;
    if item.locator.kind != LocatorKind::Query || !item.locator.valid() {
        return Err(bad("a query embedding without a query locator"));
    }
    vector::normalize(item.embedding).map_err(|e| ProtocolError(e.to_string()))
}

/// A stored excerpt: trimmed, cut to [`MAX_EXCERPT_CHARS`], and dropped
/// when blank or carrying a Metro credential.
fn excerpt(text: Option<String>) -> Option<String> {
    let t = text?;
    let t = t.trim();
    if t.is_empty() || t.contains(RUNNER_TOKEN_PREFIX) || t.contains(DEVICE_TOKEN_PREFIX) {
        return None;
    }
    Some(crate::search::clip(t, MAX_EXCERPT_CHARS, "…"))
}

/// An uploaded UTF-8 text file as the model service reads it: BOM
/// stripped, `\r\n` and `\r` turned into `\n`, control and format
/// characters dropped except `\n` and `\t`. Locator offsets for `text/*`
/// files are Unicode code points into this text. `None` when the bytes are
/// not UTF-8 (the service refuses those). The service also applies NFC;
/// text that NFC would change can make offsets disagree, and then
/// [`text_excerpt`] drops the excerpt rather than show the wrong text.
pub fn clean_text(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    Some(
        text.chars()
            .filter(|c| matches!(c, '\n' | '\t') || !(c.is_control() || is_format(*c)))
            .collect(),
    )
}

/// Unicode format characters (category Cf) that occur in real text.
fn is_format(c: char) -> bool {
    matches!(c as u32,
        0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x180E | 0x200B..=0x200F
        | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
        | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

/// `text` with runs of whitespace collapsed to one space, as excerpts are.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The excerpt shown for a segment of an uploaded text file: the file's own
/// text at the locator's code-point range, never the model's words. When
/// the model sent an excerpt it must agree with that text (exactly, or as
/// its `…`-cut prefix); otherwise there is no excerpt, so fabricated text
/// is never shown or sent to System One. No range, or a range outside the
/// text: no excerpt.
pub fn text_excerpt(text: &str, locator: &Locator, model: Option<&str>) -> Option<String> {
    let (start, end) = (locator.start? as usize, locator.end? as usize);
    if start > end {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let slice: String = chars.get(start..end)?.iter().collect();
    let own = collapse(&slice);
    if own.is_empty() {
        return None;
    }
    if let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) {
        let agrees = m == own
            || m.strip_suffix('…')
                .map(str::trim_end)
                .is_some_and(|prefix| !prefix.is_empty() && own.starts_with(prefix));
        if !agrees {
            return None;
        }
    }
    excerpt(Some(own))
}

/// The segments of an asset answer for a file of `kind`.
///
/// Excerpts: images, audio and video keep none, whatever the model sent
/// (captions and transcripts are the model's words, not the file). For an
/// uploaded text file, `source_text` is its [`clean_text`]: its segments
/// must have `text` locators, and their excerpts come from that text
/// ([`text_excerpt`]). PDF and DOCX (`source_text` is `None`) keep the
/// model service's extracted text, the only text Metro has for them.
pub fn read_asset(
    answer: &Value,
    kind: Kind,
    source_text: Option<&str>,
) -> Result<AssetAnswer, ProtocolError> {
    let wire = parse(answer)?;
    let mut items = wire.data;
    items.sort_by_key(|i| i.index);
    if items.len() > MAX_SEGMENTS_PER_ASSET {
        return Err(bad("more segments than the protocol allows"));
    }
    let partial = wire.partial;
    let mut dims = None;
    let mut segments = Vec::with_capacity(items.len());
    for item in items {
        let loc = item.locator;
        if !loc.valid() || !kind.allows(loc.kind) {
            return Err(bad(&format!(
                "a locator that does not fit a {} file",
                kind.as_str()
            )));
        }
        if source_text.is_some() && loc.kind != LocatorKind::Text {
            return Err(bad("a locator that does not fit a text file"));
        }
        match dims {
            Some(d) if d != item.embedding.len() => {
                return Err(ProtocolError(
                    vector::VectorError::Mismatch {
                        expected: d,
                        got: item.embedding.len(),
                    }
                    .to_string(),
                ));
            }
            _ => dims = Some(item.embedding.len()),
        }
        let v = vector::normalize(item.embedding).map_err(|e| ProtocolError(e.to_string()))?;
        let excerpt = match (kind, source_text) {
            (Kind::Document, Some(text)) => text_excerpt(text, &loc, item.excerpt.as_deref()),
            (Kind::Document, None) => excerpt(item.excerpt),
            _ => None,
        };
        segments.push(Segment {
            seq: item.index,
            vector: v,
            locator: loc,
            excerpt,
        });
    }
    Ok(AssetAnswer {
        model: crate::search::clip(wire.model.trim(), 200, ""),
        dims: dims.unwrap_or(0),
        segments,
        partial,
    })
}
