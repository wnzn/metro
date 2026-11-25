//! A response byte budget for upstream calls Metro reads whole: model
//! listings and the endpoint tests a connection change runs.
//!
//! Rig's bundled transport buffers a successful body with
//! `response.bytes()`, which has no limit. [`BudgetedHttp`] sends the very
//! same request (same method, URL, headers and body; only the response side
//! differs) through the inner client's streaming path and counts each chunk
//! against one budget before keeping it. The budget is shared by every
//! request made through one client, so a paginated listing is bounded in
//! total, not per page. Once it is spent the body is dropped mid-stream,
//! which closes the connection: memory stays near the budget whatever the
//! endpoint sends. Error replies keep the inner client's own 64 KiB bound.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::StreamExt;
use rig_core::http_client::{
    self, Bytes, DynHttpClient, HttpClientExt, LazyBody, MultipartForm, Request, Response,
};

/// The most bytes one model listing reads, all of its pages together.
pub(crate) const LISTING_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// The most bytes one chat completion test reads.
pub(crate) const PROBE_MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// The fixed error of a spent budget: no upstream text.
#[derive(Debug)]
pub(crate) struct TooLarge;

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the model endpoint's response is too large")
    }
}

impl std::error::Error for TooLarge {}

fn too_large() -> http_client::Error {
    http_client::Error::permanent(TooLarge)
}

/// Whether `error` is (or wraps) a spent budget.
pub(crate) fn is_too_large(error: &(dyn std::error::Error + 'static)) -> bool {
    std::iter::successors(Some(error), |e| e.source()).any(|e| e.is::<TooLarge>())
}

/// `inner` with one response byte budget for everything sent through it.
#[derive(Clone)]
pub(crate) struct BudgetedHttp {
    inner: DynHttpClient,
    left: Arc<AtomicUsize>,
}

impl BudgetedHttp {
    pub(crate) fn new(inner: DynHttpClient, bytes: usize) -> Self {
        Self {
            inner,
            left: Arc::new(AtomicUsize::new(bytes)),
        }
    }

    /// Erased, as Rig's providers take it.
    pub(crate) fn client(inner: DynHttpClient, bytes: usize) -> DynHttpClient {
        DynHttpClient::new(Self::new(inner, bytes))
    }
}

/// Take `n` bytes from the budget, or none when fewer are left.
fn take(left: &AtomicUsize, n: usize) -> bool {
    left.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |l| l.checked_sub(n))
        .is_ok()
}

/// A declared length the budget can't cover fails before any body is read.
fn declared_too_large(headers: &http_client::HeaderMap, left: &AtomicUsize) -> bool {
    headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .is_some_and(|len| len > left.load(Ordering::SeqCst) as u64)
}

impl HttpClientExt for BudgetedHttp {
    fn send(
        &self,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody>>> + Send + 'static {
        let (inner, left) = (self.inner.clone(), self.left.clone());
        async move {
            let response = HttpClientExt::send_streaming(&inner, req).await?;
            let (parts, mut stream) = response.into_parts();
            if declared_too_large(&parts.headers, &left) {
                return Err(too_large());
            }
            let body: LazyBody = Box::pin(async move {
                let mut kept = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    if !take(&left, chunk.len()) {
                        return Err(too_large());
                    }
                    kept.extend_from_slice(&chunk);
                }
                Ok(Bytes::from(kept))
            });
            Ok(Response::from_parts(parts, body))
        }
    }

    // An `async fn` would borrow `self`; the trait wants a `'static` future.
    #[allow(clippy::manual_async_fn)]
    fn send_multipart(
        &self,
        _req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody>>> + Send + 'static {
        // Listings and endpoint tests send no forms; refuse rather than
        // read an unbounded body.
        async { Err(http_client::Error::permanent(MultipartRefused)) }
    }

    fn send_streaming(
        &self,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<http_client::StreamingResponse>> + Send {
        let (inner, left) = (self.inner.clone(), self.left.clone());
        async move {
            let response = HttpClientExt::send_streaming(&inner, req).await?;
            let (parts, stream) = response.into_parts();
            if declared_too_large(&parts.headers, &left) {
                return Err(too_large());
            }
            // Ends after the first error, so nothing is read past the budget.
            let stream = futures::stream::try_unfold((stream, left), |(mut stream, left)| async {
                match stream.next().await {
                    None => Ok(None),
                    Some(Err(e)) => Err(e),
                    Some(Ok(chunk)) if take(&left, chunk.len()) => {
                        Ok(Some((chunk, (stream, left))))
                    }
                    Some(Ok(_)) => Err(too_large()),
                }
            });
            let stream: http_client::BoxedStream = Box::pin(stream);
            Ok(Response::from_parts(parts, stream))
        }
    }
}

#[derive(Debug)]
struct MultipartRefused;

impl std::fmt::Display for MultipartRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("this request sends no multipart body")
    }
}

impl std::error::Error for MultipartRefused {}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;

    use super::*;

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        origin
    }

    fn get_req(url: &str) -> Request<Bytes> {
        Request::get(url).body(Bytes::new()).unwrap()
    }

    #[tokio::test]
    async fn one_budget_covers_every_request_and_chunked_bodies_stop_early() {
        let produced = Arc::new(AtomicUsize::new(0));
        let counter = produced.clone();
        let origin = serve(
            Router::new()
                .route("/page", get(|| async { "0123456789" }))
                .route(
                    "/endless",
                    get(move || {
                        let counter = counter.clone();
                        // Chunked (no length), and longer than any budget.
                        let chunks = futures::stream::iter(0..4096).map(move |_| {
                            counter.fetch_add(64 * 1024, Ordering::SeqCst);
                            Ok::<_, std::io::Error>(Bytes::from(vec![b'a'; 64 * 1024]))
                        });
                        async move { Body::from_stream(chunks) }
                    }),
                ),
        )
        .await;
        let inner = super::super::no_redirect_client().unwrap();

        // Two pages of 10 bytes against a 15-byte budget: the second fails.
        let http = BudgetedHttp::new(inner.clone(), 15);
        let first = http.send(get_req(&format!("{origin}/page"))).await.unwrap();
        assert_eq!(first.into_body().await.unwrap(), Bytes::from("0123456789"));
        let second = http.send(get_req(&format!("{origin}/page"))).await;
        let err = match second {
            Ok(r) => r.into_body().await.unwrap_err(),
            Err(e) => e,
        };
        assert!(is_too_large(&err), "{err}");

        // A 256 MiB chunked body is cut off near a 1 MiB budget, for whole
        // and streamed reads alike.
        let http = BudgetedHttp::new(inner.clone(), 1024 * 1024);
        let whole = http.send(get_req(&format!("{origin}/endless"))).await;
        let err = whole.unwrap().into_body().await.unwrap_err();
        assert!(is_too_large(&err));
        let http = BudgetedHttp::new(inner, 1024 * 1024);
        let mut stream = http
            .send_streaming(get_req(&format!("{origin}/endless")))
            .await
            .unwrap()
            .into_body();
        let mut read = 0;
        let mut failed = false;
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(c) => read += c.len(),
                Err(e) => {
                    assert!(is_too_large(&e));
                    failed = true;
                }
            }
        }
        assert!(failed && read <= 1024 * 1024);
        drop(stream);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // Socket buffers aside, the server stopped sending.
        assert!(produced.load(Ordering::SeqCst) < 96 * 1024 * 1024);
    }

    #[tokio::test]
    async fn a_declared_length_over_budget_fails_before_reading() {
        let origin = serve(Router::new().route("/big", get(|| async { vec![b'a'; 4096] }))).await;
        let http = BudgetedHttp::new(super::super::no_redirect_client().unwrap(), 1024);
        let err = http
            .send(get_req(&format!("{origin}/big")))
            .await
            .err()
            .unwrap();
        assert!(is_too_large(&err));
    }
}
