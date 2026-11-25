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
//! Sign in with ChatGPT device runs on [`GuardedDeviceTransport`], which
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
use super::{sharing, turn};
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
    pub(super) turn_check: Option<TurnCheck>,
    /// The in-flight cap of a borrowed connection; `None` for a preset.
    pub(crate) slots: Option<Arc<Semaphore>>,
    /// The target's `config_rev` when the model was built: its endpoint,
    /// credential and model. A request after any of them changed is
    /// refused, so a run never keeps using a retired endpoint or key.
    pub(crate) config_rev: Option<i64>,
}

/// Additional checks for a client-selected model, including owned connections.
#[derive(Clone)]
pub(super) struct TurnCheck {
    pub state: crate::AppState,
    pub selection: turn::TurnSelection,
}

/// Minimal selection grant a queued relay job can retain. Does not hold the
/// AppState or relay that owns the job.
pub(crate) struct DeviceClaimGrant {
    caller: String,
    space: Option<String>,
    chosen: String,
    target: String,
    model: String,
    config_rev: i64,
    /// Typed calls always use the saved model, even for their owner.
    require_saved_model: bool,
}

impl DeviceClaimGrant {
    pub(super) fn typed(
        caller: &str,
        space: Option<&str>,
        chosen: &str,
        row: &super::store::Row,
    ) -> Self {
        Self {
            caller: caller.to_owned(),
            space: space.map(str::to_owned),
            chosen: chosen.to_owned(),
            target: row.id.clone(),
            model: row.model.clone(),
            config_rev: row.config_rev,
            require_saved_model: true,
        }
    }

    pub(crate) fn allowed(&self, conn: &rusqlite::Connection) -> anyhow::Result<bool> {
        if let Some(space) = &self.space
            && !super::store::is_member(conn, space, &self.caller)?
        {
            return Ok(false);
        }
        match sharing::authorize(conn, &self.caller, self.space.as_deref(), &self.chosen) {
            Ok(a) => Ok(a.row.id == self.target
                && a.row.config_rev == self.config_rev
                && ((!self.require_saved_model && a.access == sharing::Access::Own)
                    || a.row.model == self.model)),
            Err(UseError::Internal) => Err(super::ProviderError::Use(UseError::Internal).into()),
            Err(_) => Ok(false),
        }
    }
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
    /// Take a slot and recheck the grant immediately before using it, since
    /// access or the selected model can change while a request is waiting.
    pub async fn admit(&self) -> Result<Option<OwnedSemaphorePermit>, UseError> {
        let permit = match &self.slots {
            None => None,
            Some(slots) => {
                match tokio::time::timeout(SLOT_WAIT, slots.clone().acquire_owned()).await {
                    Ok(Ok(permit)) => Some(permit),
                    _ => return Err(UseError::Busy),
                }
            }
        };
        let (caller, space, chosen, target, turn_check, config_rev) = (
            self.caller.clone(),
            self.space.clone(),
            self.chosen.clone(),
            self.target.clone(),
            self.turn_check.clone(),
            self.config_rev,
        );
        let still = self
            .db
            .call(move |db| {
                let conn = db.lock();
                // A use in a Space needs the caller still in it, whatever
                // connection runs (their own, or one shared organization-wide).
                if let Some(sid) = &space
                    && !super::store::is_member(&conn, sid, &caller)?
                {
                    return Ok(Err(UseError::NotFound));
                }
                let checked = sharing::authorize(&conn, &caller, space.as_deref(), &chosen)
                    .and_then(|a| {
                        if a.row.id != target {
                            return Ok(false);
                        }
                        if config_rev.is_some_and(|rev| rev != a.row.config_rev) {
                            return Err(UseError::TurnChanged);
                        }
                        if let Some(check) = turn_check {
                            check.selection.recheck(&check.state, &conn, &a)?;
                        }
                        Ok(true)
                    });
                Ok(checked)
            })
            .await
            .map_err(|e| {
                tracing::error!(error = %format!("{e:#}"), "rechecking a model connection grant");
                UseError::Internal
            })?;
        match still {
            Ok(true) => Ok(permit),
            Ok(false) | Err(UseError::NotFound) => Err(UseError::Revoked),
            Err(e) => Err(e),
        }
    }
}

/// A refused request, as the HTTP layer reports it: the [`UseError`],
/// shown as its text.
#[derive(Debug)]
struct Refused(UseError);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Refused {}

fn refused(e: UseError) -> http_client::Error {
    http_client::Error::Permanent(Box::new(Refused(e)))
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

/// An HTTP client for a Space's work on an endpoint no connection guards
/// (the operator's `METRO_MODEL`): before each request, the person must
/// still be a member of the Space, or nothing of the Space leaves Metro.
#[derive(Clone)]
pub struct SpaceMemberHttp {
    pub(crate) inner: DynHttpClient,
    pub(crate) db: Db,
    pub(crate) space: String,
    pub(crate) user: String,
}

impl SpaceMemberHttp {
    async fn admit(&self) -> Result<(), UseError> {
        let (space, user) = (self.space.clone(), self.user.clone());
        let member = self
            .db
            .call(move |db| Ok(super::store::is_member(&db.lock(), &space, &user)?))
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %format!("{e:#}"), "rechecking Space membership");
                false
            });
        if member {
            Ok(())
        } else {
            Err(UseError::Revoked)
        }
    }
}

impl HttpClientExt for SpaceMemberHttp {
    fn send(
        &self,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody>>> + Send + 'static {
        let this = self.clone();
        async move {
            this.admit().await.map_err(refused)?;
            HttpClientExt::send(&this.inner, req).await
        }
    }

    fn send_multipart(
        &self,
        req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody>>> + Send + 'static {
        let this = self.clone();
        async move {
            this.admit().await.map_err(refused)?;
            HttpClientExt::send_multipart(&this.inner, req).await
        }
    }

    fn send_streaming(
        &self,
        req: Request<Bytes>,
    ) -> impl Future<Output = http_client::Result<http_client::StreamingResponse>> + Send {
        let this = self.clone();
        async move {
            this.admit().await.map_err(refused)?;
            HttpClientExt::send_streaming(&this.inner, req).await
        }
    }
}

/// The (unused) URL and dialect Rig encodes device chat requests with: the
/// transport relays only the JSON body.
pub(crate) const DEVICE_WIRE_URL: &str = "http://metro-device.invalid/v1";
pub(crate) const DEVICE_WIRE_DIALECT: Dialect =
    Dialect::gateway("metro-device", DEVICE_WIRE_URL, "METRO_DEVICE_UNUSED_KEY");

/// Chat Completions with an extra grant check for a subscription preset or
/// an explicit model selection. The relay still applies its own checks.
#[derive(Clone)]
pub struct GuardedDeviceTransport {
    pub(crate) relay: DeviceRelay,
    pub(crate) caller: Caller,
    pub(crate) connection: String,
    pub(crate) model: String,
    pub(crate) grant: Grant,
}

impl std::fmt::Debug for GuardedDeviceTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardedDeviceTransport")
            .field("connection", &self.connection)
            .finish_non_exhaustive()
    }
}

impl Transport<Chat> for GuardedDeviceTransport {
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
            if this.grant.target != this.connection {
                return Err(refused(UseError::Revoked));
            }
            let selection = this
                .grant
                .turn_check
                .as_ref()
                .and_then(|check| check.selection.device.map(|expected| (check, expected)));
            let response = match selection {
                Some((check, expected)) => {
                    this.relay
                        .complete_selected_as(
                            &this.grant.db,
                            &this.caller,
                            &this.connection,
                            body,
                            expected,
                            DeviceClaimGrant {
                                caller: this.grant.caller.clone(),
                                space: this.grant.space.clone(),
                                chosen: this.grant.chosen.clone(),
                                target: this.grant.target.clone(),
                                model: check.selection.model_id.clone(),
                                config_rev: check.selection.config_rev,
                                require_saved_model: false,
                            },
                        )
                        .await
                }
                None => {
                    this.relay
                        .complete_as(&this.grant.db, &this.caller, &this.connection, body)
                        .await
                }
            }
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
