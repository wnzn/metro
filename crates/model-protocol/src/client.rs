//! HTTP clients for the two typed model protocols, shared by the server's
//! adapters and by `metro-connector`:
//!
//! - **System One** ([`decide`]): `POST {base}/systemone` with
//!   `{model, state, questions}`, as documented at
//!   <https://docs.typesafe.ai/api/system-one> and served by self-hosted
//!   OpenJev helpers (`http://127.0.0.1:3000/v1`). Only `noul`, `choice` and
//!   `score` questions; no `assets`, no `box`, no Danger Zone headers.
//! - **Embeddings** ([`embed`]): OpenAI's `POST {base}/embeddings` with
//!   `{model, input: [strings]}`.
//! - **Model lists** ([`list_models`]): `GET {base}/models`, either
//!   TypeSafe's `{models:[{name}]}` or OpenAI's `{data:[{id}]}`.
//!
//! `base` is always chosen by the operator (server) or by the person on their
//! own machine (connector), never by whoever sends a request, and includes
//! the version prefix (`https://api.typesafe.ai/v1`). Requests and responses
//! are validated on both sides with fixed limits; a response that doesn't
//! fit the request exactly is an error, never a guess. Redirects are never
//! followed, bodies are read with a cap, and with no key no
//! `Authorization` header is sent at all. Errors are [`ClientError`]s with
//! fixed text: they never carry the key, the URL, the request or the
//! response.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::{Question, Questions, SystemOneRequest, valid_key, validate_questions};

/// Most questions in one `decide` request: one. The hosted API accepts
/// several, but one question per request is what every System One server
/// (hosted and self-hosted helpers) answers the same way, so callers ask
/// several questions as several requests.
pub const DECIDE_MAX_QUESTIONS: usize = 1;
/// Largest serialized `decide` request.
pub const DECIDE_MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// Largest `decide` response read from an endpoint.
pub const DECIDE_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Deepest nesting of `state`, `instructions` and criteria (the API's rule).
pub const MAX_NESTING: usize = 64;
/// Tolerance for probabilities that should sum to 1 (helpers round them).
pub const PROBABILITY_SUM_TOLERANCE: f64 = 0.05;
/// How far below the most likely option the chosen one may be (near ties).
pub const CHOICE_TIE_TOLERANCE: f64 = 0.01;

/// Most inputs in one `embed` request.
pub const EMBED_MAX_INPUTS: usize = 32;
/// Longest single input, in bytes.
pub const EMBED_MAX_INPUT_BYTES: usize = 32 * 1024;
/// Most input bytes in one request.
pub const EMBED_MAX_TOTAL_BYTES: usize = 512 * 1024;
/// Largest vector accepted (and largest `dimensions` that may be asked).
pub const EMBED_MAX_DIMENSIONS: usize = 4096;
/// Largest `embed` response read from an endpoint.
pub const EMBED_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
/// A vector shorter than this (L2 norm) carries no information.
pub const EMBED_MIN_NORM: f64 = 1e-6;

/// Most model ids kept from a list, and the longest one.
pub const MAX_LISTED_MODELS: usize = 1000;
pub const MAX_MODEL_NAME: usize = 256;
const MAX_MODELS_BODY: usize = 2 * 1024 * 1024;

const DECIDE_TIMEOUT: Duration = Duration::from_secs(120);
const EMBED_TIMEOUT: Duration = Duration::from_secs(120);
const MODELS_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!("metro-model-protocol/", env!("CARGO_PKG_VERSION"));

/// A failed typed call. The text is fixed by this crate and safe to show:
/// it never contains a key, URL, request or response content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientError(String);

impl ClientError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClientError {}

pub(crate) type R<T> = Result<T, ClientError>;

pub(crate) fn bad_request(what: &str) -> ClientError {
    ClientError::new(format!("invalid request: {what}"))
}

fn bad_decide(what: &str) -> ClientError {
    ClientError::new(format!(
        "the model endpoint returned an invalid System One response: {what}"
    ))
}

fn bad_embed(what: &str) -> ClientError {
    ClientError::new(format!(
        "the model endpoint returned an invalid embeddings response: {what}"
    ))
}

// ---------------------------------------------------------------------------
// Shared checks.

pub(crate) fn only_keys(obj: &Map<String, Value>, allowed: &[&str], what: &str) -> R<()> {
    if obj.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(bad_request(&format!(
            "{what} has a field that is not allowed"
        )));
    }
    Ok(())
}

fn depth_ok(v: &Value, depth: usize) -> bool {
    if depth > MAX_NESTING {
        return false;
    }
    match v {
        Value::Array(a) => a.iter().all(|x| depth_ok(x, depth + 1)),
        Value::Object(o) => o.values().all(|x| depth_ok(x, depth + 1)),
        _ => true,
    }
}

/// A model name: 1..=256 characters, no whitespace or control characters.
pub fn valid_model_name(m: &str) -> bool {
    !m.is_empty()
        && m.chars().count() <= MAX_MODEL_NAME
        && !m.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub(crate) fn model_field(obj: &Map<String, Value>) -> R<String> {
    match obj.get("model") {
        Some(Value::String(m)) if valid_model_name(m) => Ok(m.clone()),
        _ => Err(bad_request("'model' must be a model name")),
    }
}

/// A model name as reported back: TypeSafe may resolve an alias, and the
/// OpenJev helper reports a descriptive label. Anything odd falls back to
/// the requested name.
pub(crate) fn reported_model(response: &Map<String, Value>, requested: &str) -> String {
    response
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| {
            !m.is_empty() && m.chars().count() <= MAX_MODEL_NAME && !m.chars().any(char::is_control)
        })
        .unwrap_or(requested)
        .to_owned()
}

fn unit(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
}

// ---------------------------------------------------------------------------
// System One.

/// Check a `decide` request and return it typed. Accepts exactly
/// `{model, state, questions}`; each question has only `type`,
/// `instructions` and `criteria`, and `type` is `noul`, `choice` or `score`.
pub fn check_decide_request(request: &Value) -> R<SystemOneRequest> {
    let obj = request
        .as_object()
        .ok_or_else(|| bad_request("the request must be an object"))?;
    only_keys(obj, &["model", "state", "questions"], "the request")?;
    let model = model_field(obj)?;
    let state = obj
        .get("state")
        .filter(|s| s.is_string() || s.is_object() || s.is_array())
        .ok_or_else(|| bad_request("'state' must be a string, object or array"))?;
    if !depth_ok(state, 1) {
        return Err(bad_request("'state' is nested too deeply"));
    }
    let raw = obj
        .get("questions")
        .and_then(Value::as_object)
        .ok_or_else(|| bad_request("'questions' must be an object"))?;
    if raw.is_empty() || raw.len() > DECIDE_MAX_QUESTIONS {
        return Err(bad_request(&format!(
            "ask 1..={DECIDE_MAX_QUESTIONS} questions"
        )));
    }
    let mut questions = Questions::new();
    for (id, q) in raw {
        if !valid_key(id, crate::MAX_QUESTION_ID) {
            return Err(bad_request("a question id is malformed"));
        }
        let qo = q
            .as_object()
            .ok_or_else(|| bad_request("a question must be an object"))?;
        only_keys(qo, &["type", "instructions", "criteria"], "a question")?;
        match qo.get("type").and_then(Value::as_str) {
            Some("noul" | "choice" | "score") => {}
            _ => {
                return Err(bad_request("a question type must be noul, choice or score"));
            }
        }
        match qo.get("instructions") {
            Some(i) if (i.is_string() || i.is_object() || i.is_array()) && depth_ok(i, 2) => {}
            _ => return Err(bad_request("a question needs 'instructions'")),
        }
        if let Some(c) = qo.get("criteria")
            && !depth_ok(c, 2)
        {
            return Err(bad_request("criteria are nested too deeply"));
        }
        let parsed: Question = serde_json::from_value(q.clone())
            .map_err(|_| bad_request("a question is malformed"))?;
        if let Question::Noul {
            criteria: Some(c), ..
        } = &parsed
            && (c.yes.is_null() && c.no.is_null())
        {
            return Err(bad_request("noul criteria are empty"));
        }
        if let Question::Score { criteria, .. } = &parsed
            && criteria.iter().any(Value::is_null)
        {
            return Err(bad_request("score levels must not be null"));
        }
        questions.insert(id.clone(), parsed);
    }
    validate_questions(&questions).map_err(|e| bad_request(&e.to_string()))?;
    let req = SystemOneRequest {
        model,
        state: state.clone(),
        questions,
    };
    let size = serde_json::to_vec(&req)
        .map(|b| b.len())
        .unwrap_or(usize::MAX);
    if size > DECIDE_MAX_REQUEST_BYTES {
        return Err(bad_request("the request is too large"));
    }
    Ok(req)
}

/// Probabilities over exactly `keys`, each in [0, 1], summing to about 1.
fn distribution(v: Option<&Value>, keys: &[String]) -> R<BTreeMap<String, f64>> {
    let map = v
        .and_then(Value::as_object)
        .ok_or_else(|| bad_decide("an answer has no probabilities"))?;
    if map.len() != keys.len() {
        return Err(bad_decide("probabilities don't cover the options asked"));
    }
    let mut out = BTreeMap::new();
    for k in keys {
        let p = unit(map.get(k))
            .ok_or_else(|| bad_decide("a probability is missing or out of range"))?;
        out.insert(k.clone(), p);
    }
    let sum: f64 = out.values().sum();
    if (sum - 1.0).abs() > PROBABILITY_SUM_TOLERANCE {
        return Err(bad_decide("probabilities don't sum to 1"));
    }
    Ok(out)
}

/// Check a System One response against the request it answers and rebuild
/// it with only the documented fields.
pub fn check_decide_response(request: &SystemOneRequest, response: &Value) -> R<Value> {
    let obj = response
        .as_object()
        .ok_or_else(|| bad_decide("not an object"))?;
    let answers = obj
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| bad_decide("no answers"))?;
    if answers.len() != request.questions.len()
        || answers.keys().any(|k| !request.questions.contains_key(k))
    {
        return Err(bad_decide("the answers don't match the questions asked"));
    }
    let mut out = Map::new();
    for (id, question) in &request.questions {
        let a = answers
            .get(id)
            .and_then(Value::as_object)
            .ok_or_else(|| bad_decide("an answer is missing"))?;
        let ty = a.get("type").and_then(Value::as_str);
        let rebuilt = match question {
            Question::Noul { .. } => {
                if ty != Some("noul") {
                    return Err(bad_decide("an answer has the wrong type"));
                }
                let p = unit(a.get("noul")).ok_or_else(|| bad_decide("a noul is not in [0, 1]"))?;
                json!({"type": "noul", "noul": p})
            }
            Question::Choice { criteria, .. } => {
                if ty != Some("choice") {
                    return Err(bad_decide("an answer has the wrong type"));
                }
                let keys: Vec<String> = criteria.keys().cloned().collect();
                let choice = a
                    .get("choice")
                    .and_then(Value::as_str)
                    .filter(|c| criteria.contains_key(*c))
                    .ok_or_else(|| bad_decide("a choice is not one of the options asked"))?;
                let probs = distribution(a.get("probabilities"), &keys)?;
                let max = probs.values().copied().fold(0.0, f64::max);
                if probs[choice] + CHOICE_TIE_TOLERANCE < max {
                    return Err(bad_decide("a choice is not the most likely option"));
                }
                let confidence = unit(a.get("confidence"))
                    .ok_or_else(|| bad_decide("a confidence is missing or out of range"))?;
                json!({"type": "choice", "choice": choice, "confidence": confidence, "probabilities": probs})
            }
            Question::Score { criteria, .. } => {
                if ty != Some("score") {
                    return Err(bad_decide("an answer has the wrong type"));
                }
                let top = (criteria.len() - 1) as f64;
                let score = a
                    .get("score")
                    .and_then(Value::as_f64)
                    .filter(|s| s.is_finite() && (0.0..=top).contains(s))
                    .ok_or_else(|| bad_decide("a score is outside the levels asked"))?;
                let keys: Vec<String> = (0..criteria.len()).map(|i| i.to_string()).collect();
                let probs = distribution(a.get("probabilities"), &keys)?;
                let confidence = unit(a.get("confidence"))
                    .ok_or_else(|| bad_decide("a confidence is missing or out of range"))?;
                json!({"type": "score", "score": score, "confidence": confidence, "probabilities": probs})
            }
        };
        out.insert(id.clone(), rebuilt);
    }
    let mut result = Map::new();
    result.insert(
        "model".into(),
        Value::String(reported_model(obj, &request.model)),
    );
    result.insert("answers".into(), Value::Object(out));
    if let Some(u) = obj.get("usage").and_then(Value::as_object) {
        let input = u.get("input_tokens").and_then(Value::as_u64);
        let output = u.get("output_tokens").and_then(Value::as_u64);
        if let (Some(i), Some(o)) = (input, output) {
            result.insert(
                "usage".into(),
                json!({"input_tokens": i, "output_tokens": o}),
            );
        }
    }
    Ok(Value::Object(result))
}

// ---------------------------------------------------------------------------
// Embeddings.

/// A checked `embed` request.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbedRequest {
    pub model: String,
    pub input: Vec<String>,
    pub dimensions: Option<usize>,
}

impl EmbedRequest {
    fn to_json(&self) -> Value {
        let mut v = json!({"model": self.model, "input": self.input});
        if let Some(d) = self.dimensions {
            v["dimensions"] = json!(d);
        }
        v
    }
}

/// Check an `embed` request: `{model, input: [strings], dimensions?}`.
pub fn check_embed_request(request: &Value) -> R<EmbedRequest> {
    let obj = request
        .as_object()
        .ok_or_else(|| bad_request("the request must be an object"))?;
    only_keys(obj, &["model", "input", "dimensions"], "the request")?;
    let model = model_field(obj)?;
    let input = obj
        .get("input")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request("'input' must be an array of strings"))?;
    if input.is_empty() || input.len() > EMBED_MAX_INPUTS {
        return Err(bad_request(&format!(
            "'input' must have 1..={EMBED_MAX_INPUTS} strings"
        )));
    }
    let mut texts = Vec::with_capacity(input.len());
    let mut total = 0usize;
    for t in input {
        let s = t
            .as_str()
            .ok_or_else(|| bad_request("'input' must be an array of strings"))?;
        if s.is_empty() || s.len() > EMBED_MAX_INPUT_BYTES {
            return Err(bad_request(&format!(
                "each input must be 1..={EMBED_MAX_INPUT_BYTES} bytes"
            )));
        }
        total += s.len();
        texts.push(s.to_owned());
    }
    if total > EMBED_MAX_TOTAL_BYTES {
        return Err(bad_request("the inputs are too large together"));
    }
    let dimensions = match obj.get("dimensions") {
        None | Some(Value::Null) => None,
        Some(d) => Some(
            d.as_u64()
                .map(|d| d as usize)
                .filter(|d| (1..=EMBED_MAX_DIMENSIONS).contains(d))
                .ok_or_else(|| {
                    bad_request(&format!("'dimensions' must be 1..={EMBED_MAX_DIMENSIONS}"))
                })?,
        ),
    };
    Ok(EmbedRequest {
        model,
        input: texts,
        dimensions,
    })
}

/// Check an embeddings response against its request and rebuild it sorted
/// by index, with only the documented fields.
pub fn check_embed_response(request: &EmbedRequest, response: &Value) -> R<Value> {
    let obj = response
        .as_object()
        .ok_or_else(|| bad_embed("not an object"))?;
    let data = obj
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| bad_embed("no data"))?;
    let n = request.input.len();
    if data.len() != n {
        return Err(bad_embed("the number of vectors doesn't match the inputs"));
    }
    let mut slots: Vec<Option<Vec<f64>>> = vec![None; n];
    let mut dims: Option<usize> = None;
    for item in data {
        let item = item
            .as_object()
            .ok_or_else(|| bad_embed("an item is not an object"))?;
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|i| i as usize)
            .filter(|i| *i < n)
            .ok_or_else(|| bad_embed("an index is missing or out of range"))?;
        if slots[index].is_some() {
            return Err(bad_embed("an index appears twice"));
        }
        let values = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| bad_embed("an embedding is not a float array"))?;
        if values.is_empty() || values.len() > EMBED_MAX_DIMENSIONS {
            return Err(bad_embed("an embedding has an unsupported length"));
        }
        if dims.is_some_and(|d| d != values.len()) {
            return Err(bad_embed("the embeddings have different lengths"));
        }
        if request.dimensions.is_some_and(|d| d != values.len()) {
            return Err(bad_embed("an embedding doesn't have the dimensions asked"));
        }
        dims = Some(values.len());
        let mut vector = Vec::with_capacity(values.len());
        let mut norm = 0f64;
        for v in values {
            let x = v
                .as_f64()
                .filter(|x| x.is_finite() && x.abs() <= f32::MAX as f64)
                .ok_or_else(|| bad_embed("an embedding has a non-finite value"))?;
            norm += x * x;
            vector.push(x);
        }
        if norm.sqrt() < EMBED_MIN_NORM {
            return Err(bad_embed("an embedding is all zeros"));
        }
        slots[index] = Some(vector);
    }
    let data: Vec<Value> = slots
        .into_iter()
        .enumerate()
        .map(
            |(i, v)| json!({"object": "embedding", "index": i, "embedding": v.unwrap_or_default()}),
        )
        .collect();
    let mut out = Map::new();
    out.insert("object".into(), json!("list"));
    out.insert(
        "model".into(),
        Value::String(reported_model(obj, &request.model)),
    );
    out.insert("data".into(), Value::Array(data));
    if let Some(u) = obj.get("usage").and_then(Value::as_object) {
        let mut usage = Map::new();
        for k in ["prompt_tokens", "total_tokens"] {
            if let Some(n) = u.get(k).and_then(Value::as_u64) {
                usage.insert(k.into(), json!(n));
            }
        }
        if !usage.is_empty() {
            out.insert("usage".into(), Value::Object(usage));
        }
    }
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// HTTP.

/// The normalized base URL, or an error. Only `http`/`https`, no user
/// name, password, query or fragment.
fn base_url(base: &str) -> R<String> {
    let bad = || ClientError::new("the model endpoint URL is not usable");
    let url = url::Url::parse(base.trim()).map_err(|_| bad())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(bad());
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn http(timeout: Duration) -> R<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10).min(timeout))
        .timeout(timeout)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|_| ClientError::new("cannot set up the HTTP client"))
}

fn send_error(e: &reqwest::Error) -> ClientError {
    ClientError::new(if e.is_timeout() {
        "the model endpoint did not answer in time"
    } else if e.is_connect() {
        "cannot reach the model endpoint"
    } else {
        "the request to the model endpoint failed"
    })
}

fn status_error(status: reqwest::StatusCode) -> ClientError {
    let hint = match status.as_u16() {
        300..=399 => " (redirects are not followed; check the base URL)",
        401 | 403 => " (check the API key)",
        404 => " (check the base URL)",
        413 => " (the request is too large)",
        422 => " (the endpoint rejected the request)",
        429 | 529 => " (rate limited or overloaded; try again later)",
        _ => "",
    };
    ClientError::new(format!(
        "HTTP {} from the model endpoint{hint}",
        status.as_u16()
    ))
}

async fn read_json(mut resp: reqwest::Response, cap: usize) -> R<Value> {
    let too_large = || ClientError::new("the model endpoint's response is too large");
    if resp.content_length().is_some_and(|l| l > cap as u64) {
        return Err(too_large());
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| send_error(&e))? {
        if buf.len() + chunk.len() > cap {
            return Err(too_large());
        }
        buf.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&buf)
        .map_err(|_| ClientError::new("the model endpoint's response is not JSON"))
}

pub(crate) async fn call(
    method: reqwest::Method,
    base: &str,
    path: &str,
    key: Option<&str>,
    body: Option<Value>,
    timeout: Duration,
    cap: usize,
) -> R<Value> {
    let bytes = match body {
        Some(b) => Some(serde_json::to_vec(&b).map_err(|_| bad_request("cannot encode"))?),
        None => None,
    };
    call_bytes(method, base, path, key, bytes, timeout, cap).await
}

/// [`call`] with a body already encoded as JSON.
pub(crate) async fn call_bytes(
    method: reqwest::Method,
    base: &str,
    path: &str,
    key: Option<&str>,
    body: Option<Vec<u8>>,
    timeout: Duration,
    cap: usize,
) -> R<Value> {
    let url = format!("{}{path}", base_url(base)?);
    let client = http(timeout)?;
    let mut rb = client
        .request(method, url)
        .header("Accept", "application/json");
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        if k.chars().any(|c| c.is_whitespace() || c.is_control()) || k.len() > 4096 {
            return Err(ClientError::new("the API key is malformed"));
        }
        rb = rb.bearer_auth(k);
    }
    if let Some(bytes) = body {
        rb = rb.header("Content-Type", "application/json").body(bytes);
    }
    let resp = rb.send().await.map_err(|e| send_error(&e))?;
    if !resp.status().is_success() {
        return Err(status_error(resp.status()));
    }
    read_json(resp, cap).await
}

/// One System One call: `request` is `{model, state, questions}`; the result
/// is the validated, rebuilt response (see [`check_decide_response`]).
pub async fn decide(base: &str, key: Option<&str>, request: Value) -> anyhow::Result<Value> {
    let req = check_decide_request(&request)?;
    let body = serde_json::to_value(&req).map_err(|_| bad_request("cannot encode"))?;
    let resp = call(
        reqwest::Method::POST,
        base,
        "/systemone",
        key,
        Some(body),
        DECIDE_TIMEOUT,
        DECIDE_MAX_RESPONSE_BYTES,
    )
    .await?;
    Ok(check_decide_response(&req, &resp)?)
}

/// One embeddings call: `request` is `{model, input:[strings], dimensions?}`;
/// the result is the validated response, sorted by index.
pub async fn embed(base: &str, key: Option<&str>, request: Value) -> anyhow::Result<Value> {
    let req = check_embed_request(&request)?;
    let resp = call(
        reqwest::Method::POST,
        base,
        "/embeddings",
        key,
        Some(req.to_json()),
        EMBED_TIMEOUT,
        EMBED_MAX_RESPONSE_BYTES,
    )
    .await?;
    Ok(check_embed_response(&req, &resp)?)
}

/// Model ids from `GET {base}/models`. `kind` is `system_one` (TypeSafe's
/// `{models:[{name}]}`, or `{data:[{id}]}`) or `embeddings`
/// (`{data:[{id}]}`).
pub async fn list_models(base: &str, key: Option<&str>, kind: &str) -> anyhow::Result<Vec<String>> {
    if !matches!(kind, "system_one" | "embeddings") {
        return Err(ClientError::new("unknown model kind").into());
    }
    let v = call(
        reqwest::Method::GET,
        base,
        "/models",
        key,
        None,
        MODELS_TIMEOUT,
        MAX_MODELS_BODY,
    )
    .await?;
    Ok(parse_model_list(kind, &v)?)
}

/// The ids in a model list body (see [`list_models`]).
pub fn parse_model_list(kind: &str, v: &Value) -> R<Vec<String>> {
    let bad = || ClientError::new("the model endpoint returned an invalid model list");
    let (items, field) = match (kind, v.get("models"), v.get("data")) {
        ("system_one", Some(Value::Array(a)), _) => (a, "name"),
        (_, _, Some(Value::Array(a))) => (a, "id"),
        _ => return Err(bad()),
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for item in items {
        let id = item.get(field).and_then(Value::as_str).ok_or_else(bad)?;
        if !valid_model_name(id) {
            continue;
        }
        if seen.insert(id.to_owned()) {
            out.push(id.to_owned());
            if out.len() >= MAX_LISTED_MODELS {
                break;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(id: &str) -> Value {
        match id {
            "billing" => json!({"type": "noul", "instructions": "Is this about billing?"}),
            "tone" => json!({"type": "choice", "instructions": "Tone?",
                             "criteria": {"angry": "upset", "calm": null}}),
            _ => json!({"type": "score", "instructions": "How urgent?",
                        "criteria": ["wait", "week", "today"]}),
        }
    }

    fn answer(id: &str) -> Value {
        match id {
            "billing" => json!({"type": "noul", "noul": 0.98}),
            "tone" => json!({"type": "choice", "choice": "calm", "confidence": 0.6,
                             "probabilities": {"angry": 0.2, "calm": 0.8}}),
            _ => json!({"type": "score", "score": 1.7, "confidence": 0.55,
                        "legend": {"0": "wait", "1": "week", "2": "today"},
                        "probabilities": {"0": 0.1, "1": 0.1, "2": 0.8}}),
        }
    }

    fn req(id: &str) -> Value {
        json!({"model": "jev-latest", "state": "I was charged twice.",
               "questions": {id: question(id)}})
    }

    fn resp(id: &str) -> Value {
        json!({"id": "shim-1", "model": "jev-1 T=0.85", "answers": {id: answer(id)},
               "usage": {"input_tokens": 120, "output_tokens": 0}})
    }

    #[test]
    fn decide_roundtrip() {
        for id in ["billing", "tone", "urgency"] {
            let r = check_decide_request(&req(id)).unwrap();
            let out = check_decide_response(&r, &resp(id)).unwrap();
            assert!(out.get("id").is_none());
            assert_eq!(out["model"], "jev-1 T=0.85");
            assert_eq!(out["usage"]["input_tokens"], 120);
            assert!(out["answers"][id].get("legend").is_none());
        }
        let r = check_decide_request(&req("billing")).unwrap();
        let out = check_decide_response(&r, &resp("billing")).unwrap();
        assert_eq!(
            out["answers"]["billing"],
            json!({"type": "noul", "noul": 0.98})
        );
        let r = check_decide_request(&req("tone")).unwrap();
        assert_eq!(
            check_decide_response(&r, &resp("tone")).unwrap()["answers"]["tone"]["choice"],
            "calm"
        );
    }

    #[test]
    fn decide_requests_are_strict() {
        let mut cases = Vec::new();
        // One question per request.
        let mut r = req("billing");
        r["questions"]["tone"] = question("tone");
        cases.push(r);
        let mut r = req("billing");
        r["assets"] = json!({"x": {"type": "image"}});
        cases.push(r);
        let mut r = req("billing");
        r["questions"] = json!({"where": {"type": "box", "instructions": "find"}});
        cases.push(r);
        let mut r = req("billing");
        r["questions"]["billing"]["extra"] = json!(1);
        cases.push(r);
        let mut r = req("billing");
        r["questions"]["billing"]
            .as_object_mut()
            .unwrap()
            .remove("instructions");
        cases.push(r);
        let mut r = req("billing");
        r["questions"] = json!({});
        cases.push(r);
        let mut r = req("urgency");
        r["questions"]["urgency"]["criteria"] = json!(["only one"]);
        cases.push(r);
        let mut r = req("tone");
        r["questions"]["tone"]["criteria"] = json!({});
        cases.push(r);
        let mut r = req("billing");
        r["questions"] = json!({"Bad-Id": {"type": "noul", "instructions": "x"}});
        cases.push(r);
        let mut r = req("billing");
        r["model"] = json!("has space");
        cases.push(r);
        let mut r = req("billing");
        r["state"] = json!(42);
        cases.push(r);
        let mut deep = json!("x");
        for _ in 0..70 {
            deep = json!([deep]);
        }
        let mut r = req("billing");
        r["state"] = deep;
        cases.push(r);
        for c in cases {
            assert!(check_decide_request(&c).is_err(), "accepted {c}");
        }
    }

    #[test]
    fn decide_responses_are_strict() {
        type Edit = Box<dyn Fn(&mut Value)>;
        let edits: Vec<(&str, &str, Edit)> = vec![
            (
                "billing",
                "missing",
                Box::new(|v| {
                    v["answers"].as_object_mut().unwrap().remove("billing");
                }),
            ),
            (
                "billing",
                "extra",
                Box::new(|v| v["answers"]["other"] = json!({"type": "noul", "noul": 0.1})),
            ),
            (
                "billing",
                "noul>1",
                Box::new(|v| v["answers"]["billing"]["noul"] = json!(1.2)),
            ),
            (
                "billing",
                "noul str",
                Box::new(|v| v["answers"]["billing"]["noul"] = json!("0.9")),
            ),
            (
                "billing",
                "type",
                Box::new(|v| v["answers"]["billing"]["type"] = json!("score")),
            ),
            (
                "billing",
                "answers array",
                Box::new(|v| v["answers"] = json!([])),
            ),
            (
                "tone",
                "unknown choice",
                Box::new(|v| v["answers"]["tone"]["choice"] = json!("excited")),
            ),
            (
                "tone",
                "not argmax",
                Box::new(|v| v["answers"]["tone"]["choice"] = json!("angry")),
            ),
            (
                "tone",
                "unknown prob",
                Box::new(|v| {
                    v["answers"]["tone"]["probabilities"] =
                        json!({"angry": 0.2, "calm": 0.7, "x": 0.1})
                }),
            ),
            (
                "tone",
                "missing prob",
                Box::new(|v| v["answers"]["tone"]["probabilities"] = json!({"calm": 1.0})),
            ),
            (
                "tone",
                "sum",
                Box::new(|v| {
                    v["answers"]["tone"]["probabilities"] = json!({"angry": 0.5, "calm": 0.9})
                }),
            ),
            (
                "tone",
                "confidence",
                Box::new(|v| v["answers"]["tone"]["confidence"] = json!(2)),
            ),
            (
                "urgency",
                "no confidence",
                Box::new(|v| {
                    v["answers"]["urgency"]
                        .as_object_mut()
                        .unwrap()
                        .remove("confidence");
                }),
            ),
            (
                "urgency",
                "score high",
                Box::new(|v| v["answers"]["urgency"]["score"] = json!(2.5)),
            ),
            (
                "urgency",
                "score neg",
                Box::new(|v| v["answers"]["urgency"]["score"] = json!(-0.1)),
            ),
            (
                "urgency",
                "level keys",
                Box::new(|v| {
                    v["answers"]["urgency"]["probabilities"] = json!({"0": 0.1, "1": 0.1, "3": 0.8})
                }),
            ),
        ];
        for (id, name, edit) in edits {
            let r = check_decide_request(&req(id)).unwrap();
            let mut v = resp(id);
            edit(&mut v);
            assert!(check_decide_response(&r, &v).is_err(), "{name}");
        }
    }

    fn ereq() -> EmbedRequest {
        check_embed_request(&json!({"model": "nomic", "input": ["a", "b"]})).unwrap()
    }

    #[test]
    fn embed_sorted_and_strict() {
        let ok = json!({"data": [
            {"object": "embedding", "index": 1, "embedding": [0.0, 1.0]},
            {"object": "embedding", "index": 0, "embedding": [1.0, 0.5]}
        ], "model": "nomic", "usage": {"prompt_tokens": 2, "total_tokens": 2}});
        let out = check_embed_response(&ereq(), &ok).unwrap();
        assert_eq!(out["data"][0]["index"], 0);
        assert_eq!(out["data"][0]["embedding"], json!([1.0, 0.5]));
        assert_eq!(out["data"][1]["embedding"], json!([0.0, 1.0]));
        // Values are relayed as sent, without f32 rounding noise.
        let precise = json!({"data": [{"index": 0, "embedding": [0.6, 0.8]}, {"index": 1, "embedding": [0.1, 0.2]}]});
        assert_eq!(
            check_embed_response(&ereq(), &precise).unwrap()["data"][0]["embedding"],
            json!([0.6, 0.8])
        );
        for bad in [
            json!({"data": [{"index": 0, "embedding": [1.0]}]}),
            json!({"data": [{"index": 0, "embedding": [1.0]}, {"index": 0, "embedding": [1.0]}]}),
            json!({"data": [{"index": 0, "embedding": [1.0]}, {"index": 2, "embedding": [1.0]}]}),
            json!({"data": [{"index": 0, "embedding": [1.0, 2.0]}, {"index": 1, "embedding": [1.0]}]}),
            json!({"data": [{"index": 0, "embedding": [0.0, 0.0]}, {"index": 1, "embedding": [1.0, 0.0]}]}),
            json!({"data": [{"index": 0, "embedding": "AAAA"}, {"index": 1, "embedding": [1.0]}]}),
            json!({"data": [{"index": 0, "embedding": [1e300]}, {"index": 1, "embedding": [1.0]}]}),
            json!({"data": [{"embedding": [1.0]}, {"index": 1, "embedding": [1.0]}]}),
            json!({"data": [{"index": 0, "embedding": []}, {"index": 1, "embedding": []}]}),
            json!({"vectors": [[1.0], [1.0]]}),
        ] {
            assert!(check_embed_response(&ereq(), &bad).is_err(), "{bad}");
        }
        let with_dims =
            check_embed_request(&json!({"model": "m", "input": ["a"], "dimensions": 3})).unwrap();
        assert!(
            check_embed_response(
                &with_dims,
                &json!({"data": [{"index": 0, "embedding": [1.0, 0.0]}]})
            )
            .is_err()
        );
        for bad in [
            json!({"model": "m", "input": []}),
            json!({"model": "m", "input": "one string"}),
            json!({"model": "m", "input": [""]}),
            json!({"model": "m", "input": [1]}),
            json!({"model": "m", "input": ["x"], "encoding_format": "base64"}),
            json!({"model": "m", "input": ["x"], "dimensions": 0}),
            json!({"model": "m", "input": ["x"], "dimensions": 100000}),
            json!({"model": "m", "input": ["x".repeat(EMBED_MAX_INPUT_BYTES + 1)]}),
            json!({"model": "m", "input": vec!["x"; EMBED_MAX_INPUTS + 1]}),
            json!({"input": ["x"]}),
        ] {
            assert!(check_embed_request(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn model_lists() {
        assert_eq!(
            parse_model_list(
                "system_one",
                &json!({"models": [{"name": "jev-latest", "description": "d", "release_date": "2026-09-15"}, {"name": "jev-latest"}]})
            )
            .unwrap(),
            vec!["jev-latest"]
        );
        assert_eq!(
            parse_model_list("system_one", &json!({"data": [{"id": "openjev"}]})).unwrap(),
            vec!["openjev"]
        );
        assert_eq!(
            parse_model_list(
                "embeddings",
                &json!({"object": "list", "data": [{"id": "text-embedding-3-small"}]})
            )
            .unwrap(),
            vec!["text-embedding-3-small"]
        );
        assert!(parse_model_list("embeddings", &json!({"models": [{"name": "x"}]})).is_err());
        assert!(parse_model_list("system_one", &json!({"models": [{"id": "x"}]})).is_err());
    }

    #[test]
    fn base_urls() {
        assert_eq!(
            base_url("https://api.typesafe.ai/v1/").unwrap(),
            "https://api.typesafe.ai/v1"
        );
        for bad in [
            "ftp://x",
            "https://u:p@x/v1",
            "https://x/v1?a=1",
            "https://x/v1#f",
            "not a url",
            "file:///etc",
        ] {
            assert!(base_url(bad).is_err(), "{bad}");
        }
    }
}
