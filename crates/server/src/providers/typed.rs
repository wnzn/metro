//! The typed (non-chat) wires, through the shared `metro-model-protocol`
//! client: System One decisions and OpenAI-compatible embeddings.
//!
//! System One is TypeSafe's typed API, as documented at
//! <https://docs.typesafe.ai/api/system-one> and in
//! <https://api.typesafe.ai/openapi.json>: `POST {base}/systemone` with
//! `{model, state, questions}` answers `{model, answers, usage}`, and
//! `GET {base}/models` lists `{models: [{name, ...}]}`. Only `noul`,
//! `choice` and `score` questions are sent (no assets).
//!
//! Embeddings: `POST {base}/embeddings` with `{model, input}` answers
//! `{data: [{index, embedding}]}`; models come from `GET {base}/models`.
//!
//! Media embeddings (EmbeddingGemma 2 adapters only): Metro's own
//! `POST {base}/media/embeddings` with `{model, query}` or
//! `{model, asset}` answers typed rows with locators (see
//! `metro_model_protocol::check_media_response`).
//!
//! This module only binds the saved connection's model to the caller's
//! payload and maps errors. Validation, limits, the no-redirect client and
//! bounded reads are the shared crate's; its error texts are fixed and never
//! carry a key, URL or payload.

use serde_json::{Value, json};

use super::ProviderError;
use super::catalog::Capability;

/// The body of a decision with the connection's `model`, checked before
/// anything is sent.
pub fn decision_request(
    model: &str,
    state: Value,
    questions: Value,
) -> Result<Value, ProviderError> {
    let request = json!({ "model": model, "state": state, "questions": questions });
    metro_model_protocol::check_decide_request(&request).map_err(input)?;
    Ok(request)
}

/// The body of an embeddings call with the connection's `model`, checked
/// before anything is sent.
pub fn embedding_request(model: &str, inputs: &[String]) -> Result<Value, ProviderError> {
    let request = json!({ "model": model, "input": inputs });
    metro_model_protocol::check_embed_request(&request).map_err(input)?;
    Ok(request)
}

/// The body of a media embeddings call: the caller's `{query}` or
/// `{asset}` with the connection's `model`, checked in full (bytes only,
/// bounded) before anything is sent. A caller-supplied `model` is refused.
///
/// The payload is moved, never copied: the checked request owns the
/// caller's base64.
pub fn media_request(
    model: &str,
    payload: Value,
) -> Result<metro_model_protocol::MediaRequest, ProviderError> {
    let Value::Object(mut fields) = payload else {
        return Err(ProviderError::InvalidInput(
            "invalid request: the input must be {\"query\"} or {\"asset\"}".into(),
        ));
    };
    if fields.contains_key("model") {
        return Err(ProviderError::InvalidInput(
            "invalid request: the model comes from the connection, not the input".into(),
        ));
    }
    // Library MIME types may carry parameters, capitals or a known alias;
    // the wire takes the canonical name. Anything unsupported is left as
    // it is and refused by the check below.
    if let Some(Value::Object(asset)) = fields.get_mut("asset")
        && let Some(Value::String(m)) = asset.get_mut("mime_type")
        && let Some(canonical) = metro_model_protocol::canonical_mime_type(m)
    {
        *m = canonical.to_owned();
    }
    fields.insert("model".into(), Value::String(model.to_owned()));
    metro_model_protocol::parse_media_request(Value::Object(fields)).map_err(input)
}

/// One media embeddings call on a server endpoint; the answer is validated
/// and normalized by the shared client.
pub async fn media_embed(
    base: &str,
    key: Option<&str>,
    request: metro_model_protocol::MediaRequest,
) -> Result<Value, ProviderError> {
    metro_model_protocol::media_embed_request(base, key, request)
        .await
        .map_err(endpoint)
}

fn input(e: metro_model_protocol::ClientError) -> ProviderError {
    ProviderError::InvalidInput(e.to_string())
}

/// An error of the shared client, as the fixed text it carries.
fn endpoint(e: anyhow::Error) -> ProviderError {
    match e.downcast_ref::<metro_model_protocol::ClientError>() {
        Some(e) => ProviderError::Endpoint(e.to_string()),
        None => ProviderError::Internal,
    }
}

pub async fn decide(base: &str, key: Option<&str>, request: Value) -> Result<Value, ProviderError> {
    metro_model_protocol::decide(base, key, request)
        .await
        .map_err(endpoint)
}

/// The vectors of a validated embeddings answer, in input order.
pub async fn embed(
    base: &str,
    key: Option<&str>,
    request: Value,
) -> Result<Vec<Vec<f32>>, ProviderError> {
    let answer = metro_model_protocol::embed(base, key, request)
        .await
        .map_err(endpoint)?;
    vectors(&answer)
}

/// The vectors of an embeddings answer the shared crate already validated
/// (sorted, one per input, finite, one dimension).
pub fn vectors(answer: &Value) -> Result<Vec<Vec<f32>>, ProviderError> {
    let bad = || {
        ProviderError::Endpoint("the model endpoint returned an invalid embeddings response".into())
    };
    answer
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(bad)?
        .iter()
        .map(|item| {
            item.get("embedding")
                .and_then(Value::as_array)
                .ok_or_else(bad)?
                .iter()
                .map(|v| v.as_f64().map(|f| f as f32).ok_or_else(bad))
                .collect()
        })
        .collect()
}

/// Model names from `GET {base}/models` for a typed capability.
pub async fn list_models(
    base: &str,
    key: Option<&str>,
    capability: Capability,
) -> Result<Vec<String>, ProviderError> {
    metro_model_protocol::list_models(base, key, capability.as_str())
        .await
        .map_err(endpoint)
}
