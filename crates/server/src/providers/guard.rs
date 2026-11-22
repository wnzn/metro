//! Per-request grant checks for borrowed connections.
//!
//! An agent run builds its model once and then sends many requests. For a
//! connection used under someone else's share (or a subscription preset),
//! every request first re-checks the grant: a revoked share, a member who
//! left the Space or a deleted connection stops the next request, not just
//! the next run. Borrowed requests also hold a slot of the connection's
//! in-flight cap for as long as their reply is being read, so one borrower
//! can't monopolize the owner's endpoint or device.
//!
//! [`GuardedHttp`] does this for server connections: it wraps the
//! no-redirect HTTP client Rig's adapters send through. Borrowed *device*
//! connections are rechecked by the relay itself
//! ([`crate::device_relay::DeviceRelay::rig_model_as`] asks
//! [`super::DeviceDirectory`] at dispatch, before the device claims the job
//! and before the result is returned), and capped by its per-connection
//! queue. A subscription preset that resolved to the caller's *own*
//! Sign in with ChatGPT device runs on [`PresetDeviceTransport`], which
//! rechecks the preset's share before each request and again before its
//! answer is returned.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rig_core::ProviderError as RigProviderError;
use rig_core::driver::{Exchange, Opened, Opening, Transport};
use rig_core::http_client::{
    self, Bytes, DynHttpClient, HttpClientExt, LazyBody, MultipartForm, Request, Response,
};
use rig_core::providers::openai::wire::{Chat, Dialect};
use rig_core::wire::{Body, Encoded, Framing, Mode, WireFrame};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::UseError;
use super::sharing;
use crate::db::Db;
use crate::device_relay::{Caller, DeviceRelay};

/// How long a borrowed request waits for a slot of the connection's cap.
const SLOT_WAIT: Duration = Duration::from_secs(30);

/// One caller's use of one shared connection in one scope.
#[derive(Clone)]
pub struct Grant {
    pub(crate) db: Db,
    pub(crate) caller: String,
    pub(crate) space: Option<String>,
    /// The connection the caller chose.
    pub(crate) chosen: String,
    /// The connection that runs: the owner's, or the caller's own for a
    /// preset. A grant that now resolves elsewhere is refused.
    pub(crate) target: String,
    /// The in-flight cap of a borrowed connection; `None` for a preset.
    pub(crate) slots: Option<Arc<Semaphore>>,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("chosen", &self.chosen)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl Grant {
    /// Recheck the grant now, then take a slot. A grant that no longer
    /// holds is [`UseError::Revoked`].
    pub async fn admit(&self) -> Result<Option<OwnedSemaphorePermit>, UseError> {
        let (caller, space, chosen, target) = (
            self.caller.clone(),
            self.space.clone(),
            self.chosen.clone(),
            self.target.clone(),
        );
        let still = self
            .db
            .call(move |db| {
                Ok(
                    sharing::authorize(&db.lock(), &caller, space.as_deref(), &chosen)
                        .map(|a| a.row.id == target),
                )
            })
            .await
            .map_err(|e| {
                tracing::error!(error = %format!("{e:#}"), "rechecking a model connection grant");
                UseError::Internal
            })?;
        match still {
            Ok(true) => {}
            Ok(false) | Err(UseError::NotFound) => return Err(UseError::Revoked),
            Err(e) => return Err(e),
        }
        match &self.slots {
            None => Ok(None),
            Some(slots) => {
                match tokio::time::timeout(SLOT_WAIT, slots.clone().acquire_owned()).await {
                    Ok(Ok(permit)) => Ok(Some(permit)),
                    _ => Err(UseError::Busy),
                }
            }
        }
    }
}

/// A refused request, as the HTTP layer reports it. Carries only the
/// [`UseError`] text.
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn refused(e: UseError) -> http_client::Error {
    http_client::Error::Permanent(Box::new(Refused(e.to_string())))
}

/// The no-redirect provider client with a grant check before each request.
#[derive(Clone)]
pub struct GuardedHttp {
    pub(crate) inner: DynHttpClient,
    pub(crate) grant: Grant,
}

impl HttpClientExt for GuardedHttp {
    fn send(
        &self,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody>>> + Send + 'static {
        let (inner, grant) = (self.inner.clone(), self.grant.clone());
        async move {
            let permit = grant.admit().await.map_err(refused)?;
            let response = HttpClientExt::send(&inner, req).await?;
            let (parts, body) = response.into_parts();
            let body: LazyBody = Box::pin(async move {
                let bytes = body.await;
                drop(permit);
                bytes
            });
            Ok(Response::from_parts(parts, body))
        }
    }

    fn send_multipart(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody>>> + Send + 'static {
        let (inner, grant) = (self.inner.clone(), self.grant.clone());
        async move {
            let permit = grant.admit().await.map_err(refused)?;
            let response = HttpClientExt::send_multipart(&inner, req).await?;
            let (parts, body) = response.into_parts();
            let body: LazyBody = Box::pin(async move {
                let bytes = body.await;
                drop(permit);
                bytes
            });
            Ok(Response::from_parts(parts, body))
        }
    }

    fn send_streaming(
        &self,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<http_client::StreamingResponse>> + Send {
        let (inner, grant) = (self.inner.clone(), self.grant.clone());
        async move {
            let permit = grant.admit().await.map_err(refused)?;
            let response = HttpClientExt::send_streaming(&inner, req).await?;
            let (parts, body) = response.into_parts();
            // The slot is held until the stream is dropped.
            #[allow(clippy::result_large_err)]
            let body: http_client::BoxedStream = Box::pin(body.map(move |chunk| {
                let _held = &permit;
                chunk
            }));
            Ok(Response::from_parts(parts, body))
        }
    }
}

/// The (unused) URL and dialect Rig encodes device chat requests with: the
/// transport relays only the JSON body.
pub(crate) const DEVICE_WIRE_URL: &str = "http://metro-device.invalid/v1";
pub(crate) const DEVICE_WIRE_DIALECT: Dialect =
    Dialect::gateway("metro-device", DEVICE_WIRE_URL, "METRO_DEVICE_UNUSED_KEY");

/// Chat Completions for a shared subscription preset on the caller's *own*
/// device connection. The device is the caller's, so it is relayed as them
/// ([`DeviceRelay::complete_as`]); what this adds is the preset's grant,
/// checked before every request and again before the answer is returned, so
/// revoking the preset stops further use. Never someone else's device.
#[derive(Clone)]
pub struct PresetDeviceTransport {
    pub(crate) relay: DeviceRelay,
    pub(crate) caller: Caller,
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) grant: Grant,
}

impl std::fmt::Debug for PresetDeviceTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresetDeviceTransport")
            .field("connection", &self.connection)
            .finish_non_exhaustive()
    }
}

impl Transport<Chat> for PresetDeviceTransport {
    fn send(&self, payload: Encoded, exchange: Exchange) -> Opening<WireFrame> {
        let mode = exchange.mode;
        let mut body: Value = match payload.request.body() {
            Body::Bytes(bytes) => match serde_json::from_slice(bytes) {
                Ok(v) => v,
                Err(_) => {
                    return Opening::failed(RigProviderError::request(
                        "the device relay could not read the request body",
                    ));
                }
            },
            Body::Multipart(_) => {
                return Opening::failed(RigProviderError::request(
                    "the device relay sends no multipart requests",
                ));
            }
        };
        if let Some(object) = body.as_object_mut() {
            object.insert("model".into(), Value::String(self.model.clone()));
        }
        let this = self.clone();
        Opening::new(async move {
            let refused = |e: UseError| RigProviderError::Provider(e.to_string());
            this.grant.admit().await.map_err(refused)?;
            // Only ever the caller's own connection.
            if this.grant.target != this.connection {
                return Err(refused(UseError::Revoked));
            }
            let response = this
                .relay
                .complete_as(&this.grant.db, &this.caller, &this.connection, body)
                .await
                .map_err(|e| RigProviderError::Provider(e.to_string()))?;
            // Revoked while the device worked: the answer is withheld.
            this.grant.admit().await.map_err(refused)?;
            Ok(match mode {
                Mode::Unary => {
                    let bytes = serde_json::to_vec(&response).unwrap_or_default();
                    let frames = Framing::Whole.split(&bytes);
                    Opened::new(futures::stream::iter(frames.into_iter().map(Ok)))
                        .with_document(response)
                }
                Mode::Streaming => {
                    let frames = Framing::Sse.split(sse_from_completion(&response).as_bytes());
                    Opened::new(futures::stream::iter(frames.into_iter().map(Ok)))
                }
            })
        })
    }
}

/// A whole `chat.completion` as the event stream a streaming endpoint would
/// have sent (the relay's own conversion is private to it).
fn sse_from_completion(response: &Value) -> String {
    let id = response.get("id").cloned().unwrap_or(json!("device"));
    let model = response.get("model").cloned().unwrap_or(Value::Null);
    let created = response.get("created").cloned().unwrap_or(json!(0));
    let chunk = |choices: Value, usage: Option<&Value>| {
        let mut c = json!({
            "id": id, "object": "chat.completion.chunk", "created": created,
            "model": model, "choices": choices,
        });
        if let Some(u) = usage {
            c["usage"] = u.clone();
        }
        format!("data: {c}\n\n")
    };
    let mut out = String::new();
    let choices = response
        .get("choices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (n, choice) in choices.iter().enumerate() {
        let index = choice.get("index").cloned().unwrap_or(json!(n));
        let message = choice.get("message").cloned().unwrap_or(json!({}));
        let mut delta = serde_json::Map::new();
        delta.insert("role".into(), json!("assistant"));
        for key in ["content", "refusal", "reasoning_content", "reasoning"] {
            if let Some(v) = message.get(key).filter(|v| !v.is_null()) {
                delta.insert(key.into(), v.clone());
            }
        }
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            let calls: Vec<Value> = calls
                .iter()
                .enumerate()
                .map(|(i, call)| {
                    let mut call = call.clone();
                    if let Some(o) = call.as_object_mut() {
                        o.entry("index").or_insert(json!(i));
                    }
                    call
                })
                .collect();
            delta.insert("tool_calls".into(), Value::Array(calls));
        }
        out.push_str(&chunk(
            json!([{"index": index, "delta": delta, "finish_reason": null}]),
            None,
        ));
        let finish = choice
            .get("finish_reason")
            .cloned()
            .filter(|v| !v.is_null())
            .unwrap_or(json!("stop"));
        out.push_str(&chunk(
            json!([{"index": index, "delta": {}, "finish_reason": finish}]),
            None,
        ));
    }
    if let Some(usage) = response.get("usage").filter(|u| !u.is_null()) {
        out.push_str(&chunk(json!([]), Some(usage)));
    }
    out.push_str("data: [DONE]\n\n");
    out
}
