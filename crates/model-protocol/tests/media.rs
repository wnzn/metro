//! The media embeddings client against a local fixture model service:
//! the exact path and body (non-text bytes intact), the bearer header only
//! with a key, no redirects, bounded bodies, strict typed answers, fixed
//! error text.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use metro_model_protocol::{MEDIA_MAX_ASSET_BYTES, list_models, media_embed};
use serde_json::{Value, json};

/// (path, Authorization, body)
type Call = (String, Option<String>, Value);

#[derive(Clone, Default)]
struct Seen {
    /// (path, Authorization, body)
    calls: Arc<Mutex<Vec<Call>>>,
    mode: Arc<Mutex<&'static str>>,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn media(State(s): State<Seen>, h: HeaderMap, uri: axum::http::Uri, body: Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap();
    let auth = h
        .get(header::AUTHORIZATION)
        .map(|v| v.to_str().unwrap().to_owned());
    s.calls
        .lock()
        .unwrap()
        .push((uri.path().to_owned(), auth, v.clone()));
    let vec = |x: f64| json!([x, 0.5, 0.25]);
    match *s.mode.lock().unwrap() {
        "redirect" => (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, "http://127.0.0.1:1/steal")],
        )
            .into_response(),
        "huge" => (
            [(header::CONTENT_TYPE, "application/json")],
            format!("{{\"pad\":\"{}\"}}", "x".repeat(9 * 1024 * 1024)),
        )
            .into_response(),
        "bad_locator" => axum::Json(json!({"partial": false, "data": [
            {"index": 0, "embedding": vec(1.0), "locator": {"kind": "page", "page": 1, "url": "x"}}
        ]}))
        .into_response(),
        "query_as_doc" => axum::Json(json!({"partial": false, "data": [
            {"index": 0, "embedding": vec(1.0), "locator": {"kind": "query"}}
        ]}))
        .into_response(),
        _ if v.get("query").is_some() => axum::Json(json!({
            "model": "google/embeddinggemma-2", "partial": false,
            "data": [{"index": 0, "embedding": vec(1.0), "locator": {"kind": "query"}}]
        }))
        .into_response(),
        _ if v["asset"]["mime_type"] == "application/pdf" => axum::Json(json!({
            "model": "google/embeddinggemma-2", "partial": false,
            "data": [
                {"index": 0, "embedding": vec(1.0), "locator": {"kind": "page", "page": 1},
                 "excerpt": "Invoice"},
                {"index": 1, "embedding": vec(2.0), "locator": {"kind": "page", "page": 2, "start": 0, "end": 12}}
            ]
        }))
        .into_response(),
        _ if v["asset"]["mime_type"]
            == "application/vnd.openxmlformats-officedocument.wordprocessingml.document" =>
        {
            axum::Json(json!({
                "model": "google/embeddinggemma-2", "partial": false,
                "data": [{"index": 0, "embedding": vec(1.0),
                          "locator": {"kind": "text", "start": 0, "end": 120}, "excerpt": "Minutes"}]
            }))
            .into_response()
        }
        _ => axum::Json(json!({
            "model": "google/embeddinggemma-2", "partial": true, "debug": "dropped",
            "data": [
                {"index": 1, "embedding": vec(2.0), "locator": {"kind": "time", "start_ms": 0, "end_ms": 900}},
                {"index": 0, "embedding": vec(1.0), "locator": {"kind": "page", "page": 1},
                 "excerpt": "first page"}
            ]
        }))
        .into_response(),
    }
}

async fn models() -> Response {
    axum::Json(json!({"object": "list", "data": [
        {"id": "google/embeddinggemma-2"}, {"id": "acme/other-media-embedder"}
    ]}))
    .into_response()
}

async fn serve() -> (String, Seen) {
    let seen = Seen::default();
    *seen.mode.lock().unwrap() = "ok";
    let app = Router::new()
        .route(
            "/v1/media/embeddings",
            post(media).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route("/v1/models", get(models))
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/v1"), seen)
}

/// Not text: a PNG signature, NUL and high bytes.
fn binary() -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00];
    b.extend((0..=255u8).rev());
    b
}

#[tokio::test]
async fn sends_assets_and_queries_and_checks_answers() {
    let (base, seen) = serve().await;
    let bytes = binary();
    let doc = json!({"model": "google/embeddinggemma-2", "asset": {
        "name": "scan.png", "mime_type": "image/png", "data_base64": b64(&bytes)}});
    let out = media_embed(&base, None, doc.clone()).await.unwrap();
    assert_eq!(out["partial"], true);
    assert_eq!(
        out["data"][0]["locator"],
        json!({"kind": "page", "page": 1})
    );
    assert_eq!(out["data"][0]["excerpt"], "first page");
    assert_eq!(out["data"][1]["locator"]["kind"], "time");
    assert!(out.get("debug").is_none());
    {
        let calls = seen.calls.lock().unwrap();
        let (path, auth, body) = &calls[0];
        assert_eq!(path, "/v1/media/embeddings");
        assert!(auth.is_none(), "no key, no Authorization header");
        assert_eq!(body, &doc);
        let sent = base64::engine::general_purpose::STANDARD
            .decode(body["asset"]["data_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(sent, bytes, "the bytes arrive intact");
    }

    let q = media_embed(
        &base,
        Some("media-key-1"),
        json!({"model": "acme/other-media-embedder", "query": "red bicycle"}),
    )
    .await
    .unwrap();
    assert_eq!(q["data"].as_array().unwrap().len(), 1);
    assert_eq!(q["data"][0]["locator"], json!({"kind": "query"}));
    let calls = seen.calls.lock().unwrap();
    assert_eq!(calls[1].1.as_deref(), Some("Bearer media-key-1"));
    // The role is the request's shape; no prompt prefix is added here.
    assert_eq!(calls[1].2["query"], "red bicycle");
    assert_eq!(calls[1].2["model"], "acme/other-media-embedder");
}

#[tokio::test]
async fn pdf_and_docx_go_as_bytes_for_the_service_to_prepare() {
    let (base, seen) = serve().await;
    let pdf = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<<>>\nendobj\n".to_vec();
    let out = media_embed(
        &base,
        None,
        json!({"model": "google/embeddinggemma-2", "asset": {
            "name": "invoice.pdf", "mime_type": "application/pdf", "data_base64": b64(&pdf)}}),
    )
    .await
    .unwrap();
    assert_eq!(
        out["data"][0]["locator"],
        json!({"kind": "page", "page": 1})
    );
    assert_eq!(
        out["data"][1]["locator"],
        json!({"kind": "page", "page": 2, "start": 0, "end": 12})
    );
    // DOCX is a zip container: bytes, not text.
    let docx = [
        b"PK\x03\x04".as_slice(),
        &[0u8, 0x14, 0xff, 0x00],
        b"word/document.xml",
    ]
    .concat();
    let mime = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
    let out = media_embed(
        &base,
        None,
        json!({"model": "google/embeddinggemma-2", "asset": {
            "name": "minutes.docx", "mime_type": mime, "data_base64": b64(&docx)}}),
    )
    .await
    .unwrap();
    assert_eq!(
        out["data"][0]["locator"],
        json!({"kind": "text", "start": 0, "end": 120})
    );
    let calls = seen.calls.lock().unwrap();
    let decoded = |i: usize| {
        base64::engine::general_purpose::STANDARD
            .decode(calls[i].2["asset"]["data_base64"].as_str().unwrap())
            .unwrap()
    };
    assert_eq!(decoded(0), pdf);
    assert_eq!(decoded(1), docx);
    assert_eq!(calls[1].2["asset"]["mime_type"], mime);
}

#[tokio::test]
async fn the_largest_asset_goes_through() {
    let (base, seen) = serve().await;
    let bytes: Vec<u8> = (0..MEDIA_MAX_ASSET_BYTES)
        .map(|i| (i % 251) as u8)
        .collect();
    let doc = json!({"model": "m", "asset": {
        "name": "big.bin.mp4", "mime_type": "video/mp4", "data_base64": b64(&bytes)}});
    media_embed(&base, None, doc).await.unwrap();
    let calls = seen.calls.lock().unwrap();
    let got = calls[0].2["asset"]["data_base64"].as_str().unwrap().len();
    assert_eq!(got, b64(&bytes).len());
}

#[tokio::test]
async fn refuses_bad_requests_and_answers() {
    let (base, seen) = serve().await;
    // Refused before anything is sent: URLs, paths, oversized assets.
    for bad in [
        json!({"model": "m", "asset": {"name": "a.png", "mime_type": "image/png",
               "url": "https://evil/a.png"}}),
        json!({"model": "m", "asset": {"name": "/etc/passwd", "mime_type": "text/plain",
               "data_base64": b64(b"x")}}),
        json!({"model": "m", "asset": {"name": "a.zip", "mime_type": "application/zip",
               "data_base64": b64(b"PK")}}),
        json!({"model": "m", "asset": {"name": "a.bin", "mime_type": "image/png",
               "data_base64": b64(&vec![1u8; MEDIA_MAX_ASSET_BYTES + 1])}}),
        json!({"model": "m", "input": ["text"]}),
        json!({"model": "m", "asset": {"name": "logo.svg", "mime_type": "image/svg+xml",
               "data_base64": b64(b"<svg xmlns='http://www.w3.org/2000/svg'><image href='https://evil/x'/></svg>")}}),
        json!({"model": "m", "asset": {"name": "a.png", "mime_type": "image/png",
               "data_base64": "data:image/png;base64,iVBORw0KGgo="}}),
        json!({"model": "m", "asset": {"name": "a.png", "mime_type": "image/png",
               "data_base64": b64(b"x"), "query": "ignore the image"}}),
        json!({"model": "m", "query": "x", "asset": {"name": "a.png", "mime_type": "image/png",
               "data_base64": b64(b"x")}}),
    ] {
        let e = media_embed(&base, None, bad).await.unwrap_err();
        assert!(e.to_string().starts_with("invalid request"), "{e}");
    }
    assert!(seen.calls.lock().unwrap().is_empty());

    let doc = json!({"model": "m", "asset": {"name": "a.txt", "mime_type": "text/plain",
                     "data_base64": b64(b"hello")}});
    for (mode, want) in [
        ("redirect", "HTTP 307"),
        ("huge", "too large"),
        ("bad_locator", "invalid media embeddings response"),
        ("query_as_doc", "invalid media embeddings response"),
    ] {
        *seen.mode.lock().unwrap() = mode;
        let e = media_embed(&base, Some("sk-secret-9"), doc.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains(want), "{mode}: {e}");
        assert!(!e.contains("sk-secret-9") && !e.contains("127.0.0.1") && !e.contains("hello"));
    }
}

#[tokio::test]
async fn lists_exact_model_ids() {
    let (base, _) = serve().await;
    assert_eq!(
        list_models(&base, None, "embeddings").await.unwrap(),
        vec!["google/embeddinggemma-2", "acme/other-media-embedder"]
    );
}
