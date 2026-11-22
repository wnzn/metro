//! Contract test: the responses of Metro's reference media embedding
//! service (`services/embeddinggemma2`, `metro_eg2/app.py` and
//! `metro_eg2/decode.py`), shape for shape, pass the strict checks, and the
//! service's optional fields are kept only in their checked, bounded form.
//!
//! Each fixture mirrors what the service emits:
//!
//! - every row has `modality`; image, scanned-page and time rows have no
//!   `excerpt`, and excerpts are whitespace-collapsed and at most 240
//!   characters;
//! - text rows (TXT, Markdown, DOCX) are `{kind:"text", start, end}`, as
//!   code-point offsets into the extracted text;
//! - PDF text pages are `{kind:"page", start, end, page}`, and pages with
//!   no extractable text are `{kind:"page", page}` with modality `image`,
//!   plus a note;
//! - audio is 8 s `time` clips and video 16 s `time` clips, and video always
//!   carries the "visual frames only" note;
//! - more than 48 segments gives `partial: true` with a note;
//! - vectors have 768 dimensions (the native size), L2-normalized.

use metro_model_protocol::{
    MEDIA_MAX_NOTE_CHARS, MEDIA_MAX_NOTES, MEDIA_MAX_ROWS, MEDIA_STANDARD_DIMENSIONS,
    check_media_request, check_media_response,
};
use serde_json::{Value, json};

const MODEL: &str = "google/embeddinggemma-2";
const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// A 768-dimension unit vector, like the service's `finish`.
fn vector(seed: usize) -> Vec<f64> {
    let raw: Vec<f64> = (0..768)
        .map(|i| (((i * 7919 + seed * 104_729) % 1000) as f64 / 1000.0) - 0.5)
        .collect();
    let norm = raw.iter().map(|x| x * x).sum::<f64>().sqrt();
    raw.into_iter().map(|x| x / norm).collect()
}

fn row(i: usize, locator: Value, modality: &str, excerpt: Option<&str>) -> Value {
    let mut r =
        json!({"index": i, "embedding": vector(i), "locator": locator, "modality": modality});
    if let Some(e) = excerpt {
        r["excerpt"] = json!(e);
    }
    r
}

fn asset(name: &str, mime: &str) -> metro_model_protocol::MediaRequest {
    check_media_request(&json!({"model": MODEL, "asset": {
        "name": name, "mime_type": mime, "data_base64": "AAECAw=="}}))
    .unwrap()
}

fn check(req: &metro_model_protocol::MediaRequest, resp: &Value) -> Value {
    let out = check_media_response(req, resp, MEDIA_STANDARD_DIMENSIONS)
        .unwrap_or_else(|e| panic!("a real service response was refused: {e}"));
    // What the device relay can carry back (2 MiB result bound).
    assert!(serde_json::to_vec(&out).unwrap().len() < 2 * 1024 * 1024 - 4096);
    out
}

#[test]
fn query() {
    let req =
        check_media_request(&json!({"model": MODEL, "query": "bridges over rivers"})).unwrap();
    let resp = json!({"model": MODEL, "data": [
        {"index": 0, "embedding": vector(0), "locator": {"kind": "query"}, "modality": "text"}],
        "partial": false});
    let out = check(&req, &resp);
    assert_eq!(out["data"][0]["modality"], "text");
    assert!(out.get("notes").is_none());
}

#[test]
fn text_markdown_and_docx_carry_offsets_and_excerpts() {
    for (name, mime) in [
        ("notes.txt", "text/plain"),
        ("readme.md", "text/markdown"),
        ("minutes.docx", DOCX),
    ] {
        let resp = json!({"model": MODEL, "partial": false, "data": [
            row(0, json!({"kind": "text", "start": 0, "end": 1187}), "text",
                Some("Solar panels on the roof. More details. More details.")),
            row(1, json!({"kind": "text", "start": 1187, "end": 2427}), "text",
                Some(&format!("{}…", "More details. ".repeat(17).trim_end()))),
        ]});
        let out = check(&asset(name, mime), &resp);
        assert_eq!(
            out["data"][1]["locator"],
            json!({"kind": "text", "start": 1187, "end": 2427}),
            "{mime}"
        );
        assert!(out["data"][1]["excerpt"].as_str().unwrap().ends_with('…'));
    }
}

#[test]
fn pdf_text_pages_and_scanned_pages() {
    let resp = json!({"model": MODEL, "partial": false, "data": [
        row(0, json!({"kind": "page", "start": 0, "end": 66, "page": 1}), "text",
            Some("A page with plenty of extractable words about rivers and bridges.")),
        row(1, json!({"kind": "page", "page": 2}), "image", None),
    ], "notes": ["1 page(s) had no extractable text and were embedded as rendered images (no OCR)"]});
    let out = check(&asset("doc.pdf", "application/pdf"), &resp);
    assert_eq!(
        out["data"][0]["locator"],
        json!({"kind": "page", "page": 1, "start": 0, "end": 66})
    );
    assert_eq!(
        out["data"][1]["locator"],
        json!({"kind": "page", "page": 2})
    );
    assert_eq!(out["data"][1]["modality"], "image");
    assert!(out["data"][1].get("excerpt").is_none());
    assert_eq!(out["notes"][0], resp["notes"][0]);
}

#[test]
fn images_audio_and_video() {
    let img = json!({"model": MODEL, "partial": false,
        "data": [row(0, json!({"kind": "image"}), "image", None)]});
    for mime in ["image/png", "image/jpeg"] {
        let out = check(&asset("a-red-circle.png", mime), &img);
        assert_eq!(out["data"][0], img["data"][0]);
    }
    let audio = json!({"model": MODEL, "partial": false, "data": [
        row(0, json!({"kind": "time", "start_ms": 0, "end_ms": 8000}), "audio", None),
        row(1, json!({"kind": "time", "start_ms": 8000, "end_ms": 10000}), "audio", None),
    ]});
    for mime in ["audio/wav", "audio/mpeg"] {
        check(&asset("a.wav", mime), &audio);
    }
    // A trailing remainder below 500 ms is dropped and reported.
    let cut = json!({"model": MODEL, "partial": true, "data": [
        row(0, json!({"kind": "time", "start_ms": 0, "end_ms": 8000}), "audio", None)],
        "notes": ["dropped a trailing 300 ms remainder"]});
    assert_eq!(check(&asset("a.mp3", "audio/mpeg"), &cut)["partial"], true);
    let video = json!({"model": MODEL, "partial": false, "data": [
        row(0, json!({"kind": "time", "start_ms": 0, "end_ms": 16000}), "video", None),
        row(1, json!({"kind": "time", "start_ms": 16000, "end_ms": 20000}), "video", None),
    ], "notes": ["video: visual frames only; the audio track is not embedded"]});
    for (name, mime) in [("v.mp4", "video/mp4"), ("v.webm", "video/webm")] {
        let out = check(&asset(name, mime), &video);
        assert_eq!(out["data"][1]["modality"], "video");
        assert_eq!(
            out["notes"],
            json!(["video: visual frames only; the audio track is not embedded"])
        );
    }
}

#[test]
fn the_segment_cap_is_partial_and_fits_the_device_relay() {
    let data: Vec<Value> = (0..MEDIA_MAX_ROWS)
        .map(|i| {
            row(
                i,
                json!({"kind": "text", "start": i * 1200, "end": (i + 1) * 1200}),
                "text",
                Some(&"word ".repeat(48)[..239]),
            )
        })
        .collect();
    let resp = json!({"model": MODEL, "partial": true, "data": data,
                      "notes": ["stopped at 48 chunks"]});
    let out = check(&asset("long.txt", "text/plain"), &resp);
    assert_eq!(out["partial"], true);
    assert_eq!(out["data"].as_array().unwrap().len(), MEDIA_MAX_ROWS);
}

#[test]
fn optional_fields_are_checked_and_bounded() {
    let req = asset("v.mp4", "video/mp4");
    let base = || {
        json!({"model": MODEL, "partial": false,
               "data": [row(0, json!({"kind": "time", "start_ms": 0, "end_ms": 4000}), "video", None)]})
    };
    // Notes are display text: control characters gone, length and count
    // bounded; other fields are dropped.
    let mut r = base();
    r["notes"] = json!(
        std::iter::once("evil\u{1b}[31m red\nline\u{0}".to_owned())
            .chain(std::iter::once("x".repeat(5000)))
            .chain((0..40).map(|i| format!("note {i}")))
            .collect::<Vec<_>>()
    );
    r["usage"] = json!({"x": 1});
    r["data"][0]["debug"] = json!("dropped");
    let out = check(&req, &r);
    let notes = out["notes"].as_array().unwrap();
    assert_eq!(notes.len(), MEDIA_MAX_NOTES);
    assert_eq!(notes[0], "evil [31m red line");
    assert_eq!(
        notes[1].as_str().unwrap().chars().count(),
        MEDIA_MAX_NOTE_CHARS
    );
    assert!(out.get("usage").is_none() && out["data"][0].get("debug").is_none());
    // Without modality and notes (another compatible service) it still fits.
    let mut r = base();
    r["data"][0].as_object_mut().unwrap().remove("modality");
    assert!(check(&req, &r)["data"][0].get("modality").is_none());

    // Refused: notes that aren't strings, modalities that don't fit.
    let mut bad = Vec::new();
    for notes in [json!("one string"), json!([1]), json!({"a": "b"})] {
        let mut r = base();
        r["notes"] = notes;
        bad.push((req.clone(), r));
    }
    for (loc, modality) in [
        (
            json!({"kind": "time", "start_ms": 0, "end_ms": 4000}),
            "image",
        ),
        (
            json!({"kind": "time", "start_ms": 0, "end_ms": 4000}),
            "document",
        ),
        (
            json!({"kind": "time", "start_ms": 0, "end_ms": 4000}),
            "VIDEO",
        ),
    ] {
        let mut r = base();
        r["data"][0]["locator"] = loc;
        r["data"][0]["modality"] = json!(modality);
        bad.push((req.clone(), r));
    }
    let pdf = asset("doc.pdf", "application/pdf");
    let mut r = base();
    r["data"][0] = row(0, json!({"kind": "page", "page": 1}), "audio", None);
    bad.push((pdf.clone(), r));
    let mut r = base();
    r["data"][0] = row(0, json!({"kind": "page", "page": 1}), "image", None);
    r["data"][0]["modality"] = json!(7);
    bad.push((pdf, r));
    let q = check_media_request(&json!({"model": MODEL, "query": "q"})).unwrap();
    let mut r = base();
    r["data"][0] = row(0, json!({"kind": "query"}), "image", None);
    bad.push((q, r));
    for (req, r) in bad {
        assert!(
            check_media_response(&req, &r, MEDIA_STANDARD_DIMENSIONS).is_err(),
            "accepted {}",
            r.to_string().chars().take(200).collect::<String>()
        );
    }
}

#[test]
fn svg_never_goes_out() {
    assert!(
        check_media_request(&json!({"model": MODEL, "asset": {
            "name": "logo.svg", "mime_type": "image/svg+xml", "data_base64": "PHN2Zy8+"}}))
        .is_err()
    );
}
