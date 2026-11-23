//! The data boundary between Metro and the person's provider.
//!
//! Metro may only ask for inference. Every `complete` request is rebuilt from
//! an allowlist of OpenAI Chat Completions fields:
//!
//! - `model` must be one of the profile's locally approved models;
//! - messages are `system`, `developer`, `user`, `assistant` or `tool`, with
//!   string content or `{"type":"text"}` parts only (no image, audio or file
//!   parts, which could make the provider fetch a remote resource);
//! - tools are `{"type":"function"}` only (no hosted web search, file
//!   search, code interpreter, MCP or computer-use tools), and a function's
//!   JSON Schema may only use local `#...` references;
//! - sampling options are type-checked; `stream` must be false and `n` 1;
//! - any other field is rejected, not dropped, so nothing changes silently.
//!
//! The provider's response is rebuilt the same way before it goes back, so a
//! vendor payload carries only the assistant message, finish reason and token
//! usage.
//!
//! Each profile kind serves one capability ([`operation_allowed`]): chat
//! kinds run `complete`, `device-systemone` runs only `decide` (System One)
//! and `device-embeddings` runs only `embed`; `device-embeddinggemma-2` runs
//! `embed` and is the only kind that runs `media_embed`; `models` works for
//! all. Typed
//! requests must name an approved model ([`check_typed`]) and are then
//! checked in full, and their responses rebuilt, by `metro-model-protocol`.

use serde_json::{Map, Value, json};

use crate::SafeError;
use crate::profile::{Capability, Kind};

pub const MAX_MESSAGES: usize = 2048;
pub const MAX_TOOLS: usize = 128;
pub const MAX_TOOL_CALLS: usize = 128;
pub const MAX_STOP: usize = 4;
pub const MAX_NAME: usize = 64;
pub const MAX_ID: usize = 256;
pub const MAX_SCHEMA_DEPTH: usize = 32;

type R<T> = Result<T, SafeError>;

fn bad(msg: impl AsRef<str>) -> SafeError {
    SafeError::new(format!("invalid request: {}", msg.as_ref()))
}

/// The operations a device job may ask for.
#[derive(Debug, Clone, PartialEq)]
pub enum Operation {
    Models,
    Complete(Value),
    /// A System One request `{model, state, questions}`.
    Decide(Value),
    /// An embeddings request `{model, input, dimensions?}`.
    Embed(Value),
    /// A media embeddings request `{model, query}` or `{model, asset}`.
    MediaEmbed(Value),
}

impl Operation {
    fn capability(&self) -> Option<Capability> {
        match self {
            Operation::Models => None,
            Operation::Complete(_) => Some(Capability::Chat),
            Operation::Decide(_) => Some(Capability::SystemOne),
            Operation::Embed(_) | Operation::MediaEmbed(_) => Some(Capability::Embeddings),
        }
    }
}

/// Whether a profile of `kind` may run `op`. A connection is never used for
/// a capability it wasn't set up for.
pub fn operation_allowed(kind: Kind, op: &Operation) -> R<()> {
    // Documents and media only on a media kind: a text embeddings endpoint
    // never stands in for one.
    if matches!(op, Operation::MediaEmbed(_)) && !kind.embeds_media() {
        return Err(SafeError::new(
            "unsupported operation: this connection embeds text only, not documents or media",
        ));
    }
    match op.capability() {
        None => Ok(()),
        Some(c) if c == kind.capability() => Ok(()),
        Some(_) => Err(SafeError::new(match kind.capability() {
            Capability::Chat => {
                "unsupported operation: this connection serves chat completions only"
            }
            Capability::SystemOne => {
                "unsupported operation: this connection serves System One decisions only"
            }
            Capability::Embeddings => {
                "unsupported operation: this connection serves embeddings only"
            }
        })),
    }
}

/// The model check for `decide` and `embed`: an object naming one of the
/// locally approved models. The shared client checks everything else.
pub fn check_typed(request: &Value, allowed_models: &[String]) -> R<Value> {
    let obj = request
        .as_object()
        .ok_or_else(|| bad("'request' must be an object"))?;
    let model = get_str(obj, "model", "request")?.ok_or_else(|| bad("'model' is required"))?;
    if !allowed_models.iter().any(|m| m == model) {
        return Err(SafeError::new(
            "model not allowed: this model is not approved in the connector profile on the device",
        ));
    }
    Ok(request.clone())
}

/// Parse a claimed job's `operation` and `request`. Anything else is refused.
pub fn parse_operation(operation: &str, request: &Value) -> R<Operation> {
    if !request.is_object() {
        return Err(bad("'request' must be an object"));
    }
    match operation {
        "models" => {
            if request.as_object().is_some_and(|m| !m.is_empty()) {
                return Err(bad("'models' takes an empty request"));
            }
            Ok(Operation::Models)
        }
        "complete" => Ok(Operation::Complete(request.clone())),
        "decide" => Ok(Operation::Decide(request.clone())),
        "embed" => Ok(Operation::Embed(request.clone())),
        "media_embed" => Ok(Operation::MediaEmbed(request.clone())),
        _ => Err(SafeError::new(
            "unsupported operation; this connector only runs 'models', 'complete', 'decide', 'embed' and 'media_embed'",
        )),
    }
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_NAME
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID && !s.chars().any(|c| c.is_control())
}

fn only_keys(obj: &Map<String, Value>, allowed: &[&str], what: &str) -> R<()> {
    for k in obj.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(bad(format!("{what}: contains an unsupported field")));
        }
    }
    Ok(())
}

fn get_str<'a>(obj: &'a Map<String, Value>, key: &str, what: &str) -> R<Option<&'a str>> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(bad(format!("{what}: '{key}' must be a string"))),
    }
}

/// Text content: a string or an array of `{"type":"text","text":...}`.
fn text_content(v: &Value, what: &str, allow_null: bool) -> R<Value> {
    match v {
        Value::Null if allow_null => Ok(Value::Null),
        Value::String(s) => Ok(Value::String(s.clone())),
        Value::Array(parts) => {
            let mut out = Vec::with_capacity(parts.len());
            for p in parts {
                let obj = p
                    .as_object()
                    .ok_or_else(|| bad(format!("{what}: content parts must be objects")))?;
                match obj.get("type").and_then(Value::as_str) {
                    Some("text") => {}
                    Some(_) => {
                        return Err(bad(format!(
                            "{what}: content part type is not allowed (text only)"
                        )));
                    }
                    None => return Err(bad(format!("{what}: content part has no 'type'"))),
                }
                only_keys(obj, &["type", "text"], what)?;
                let text = get_str(obj, "text", what)?
                    .ok_or_else(|| bad(format!("{what}: text part has no 'text'")))?;
                out.push(json!({"type": "text", "text": text}));
            }
            Ok(Value::Array(out))
        }
        _ => Err(bad(format!(
            "{what}: 'content' must be a string or text parts"
        ))),
    }
}

fn tool_calls(v: &Value, what: &str) -> R<Value> {
    let arr = v
        .as_array()
        .ok_or_else(|| bad(format!("{what}: 'tool_calls' must be an array")))?;
    if arr.len() > MAX_TOOL_CALLS {
        return Err(bad(format!("{what}: too many tool calls")));
    }
    let mut out = Vec::with_capacity(arr.len());
    for c in arr {
        let obj = c
            .as_object()
            .ok_or_else(|| bad(format!("{what}: tool call must be an object")))?;
        only_keys(obj, &["id", "type", "function", "index"], what)?;
        let id = get_str(obj, "id", what)?
            .filter(|s| valid_id(s))
            .ok_or_else(|| bad(format!("{what}: tool call needs a valid 'id'")))?;
        match get_str(obj, "type", what)? {
            Some("function") | None => {}
            Some(_) => return Err(bad(format!("{what}: only function tool calls are allowed"))),
        }
        let f = obj
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| bad(format!("{what}: tool call needs 'function'")))?;
        only_keys(f, &["name", "arguments"], what)?;
        let name = get_str(f, "name", what)?
            .filter(|s| valid_name(s))
            .ok_or_else(|| bad(format!("{what}: tool call needs a valid function name")))?;
        let args = match f.get("arguments") {
            None | Some(Value::Null) => "{}".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(o @ Value::Object(_)) => o.to_string(),
            Some(_) => return Err(bad(format!("{what}: tool call arguments must be a string"))),
        };
        out.push(
            json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}}),
        );
    }
    Ok(Value::Array(out))
}

fn message(v: &Value, i: usize) -> R<Value> {
    let what = format!("messages[{i}]");
    let obj = v
        .as_object()
        .ok_or_else(|| bad(format!("{what} must be an object")))?;
    let role = get_str(obj, "role", &what)?.ok_or_else(|| bad(format!("{what} has no 'role'")))?;
    let mut out = Map::new();
    out.insert("role".into(), Value::String(role.into()));
    match role {
        "system" | "developer" | "user" => {
            only_keys(obj, &["role", "content", "name"], &what)?;
            let c = obj
                .get("content")
                .ok_or_else(|| bad(format!("{what} has no 'content'")))?;
            out.insert("content".into(), text_content(c, &what, false)?);
        }
        "assistant" => {
            only_keys(
                obj,
                &["role", "content", "name", "tool_calls", "refusal"],
                &what,
            )?;
            let c = obj.get("content").unwrap_or(&Value::Null);
            out.insert("content".into(), text_content(c, &what, true)?);
            if let Some(tc) = obj.get("tool_calls").filter(|v| !v.is_null()) {
                let tc = tool_calls(tc, &what)?;
                if tc.as_array().is_some_and(|a| !a.is_empty()) {
                    out.insert("tool_calls".into(), tc);
                }
            }
            if let Some(r) = get_str(obj, "refusal", &what)? {
                out.insert("refusal".into(), Value::String(r.into()));
            }
        }
        "tool" => {
            only_keys(obj, &["role", "content", "tool_call_id", "name"], &what)?;
            let id = get_str(obj, "tool_call_id", &what)?
                .filter(|s| valid_id(s))
                .ok_or_else(|| bad(format!("{what} needs a valid 'tool_call_id'")))?;
            out.insert("tool_call_id".into(), Value::String(id.into()));
            let c = obj
                .get("content")
                .ok_or_else(|| bad(format!("{what} has no 'content'")))?;
            out.insert("content".into(), text_content(c, &what, false)?);
        }
        _ => {
            return Err(bad(format!("{what}: role is not allowed")));
        }
    }
    if let Some(n) = get_str(obj, "name", &what)? {
        if !valid_name(n) {
            return Err(bad(format!("{what}: invalid 'name'")));
        }
        out.insert("name".into(), Value::String(n.into()));
    }
    Ok(Value::Object(out))
}

/// JSON Schema for a function tool: an object, bounded depth, and `$ref` /
/// `$id` / `$schema`-style keys may not point anywhere but inside the schema.
fn schema(v: &Value, depth: usize, what: &str) -> R<()> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(bad(format!("{what}: schema is nested too deeply")));
    }
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                if matches!(k.as_str(), "$ref" | "$dynamicRef" | "$recursiveRef") {
                    match val.as_str() {
                        Some(s) if s.starts_with('#') => {}
                        _ => {
                            return Err(bad(format!(
                                "{what}: only local '#...' schema references are allowed"
                            )));
                        }
                    }
                }
                if matches!(k.as_str(), "$id" | "$anchor" | "$dynamicAnchor")
                    && val
                        .as_str()
                        .is_some_and(|s| s.contains(':') || s.contains('/'))
                {
                    return Err(bad(format!("{what}: schema ids must not be URLs")));
                }
                schema(val, depth + 1, what)?;
            }
            Ok(())
        }
        Value::Array(a) => a.iter().try_for_each(|x| schema(x, depth + 1, what)),
        _ => Ok(()),
    }
}

fn tool(v: &Value, i: usize) -> R<Value> {
    let what = format!("tools[{i}]");
    let obj = v
        .as_object()
        .ok_or_else(|| bad(format!("{what} must be an object")))?;
    match get_str(obj, "type", &what)? {
        Some("function") => {}
        _ => {
            return Err(bad(format!(
                "{what}: only function tools are allowed (no hosted or remote tools)"
            )));
        }
    }
    only_keys(obj, &["type", "function"], &what)?;
    let f = obj
        .get("function")
        .and_then(Value::as_object)
        .ok_or_else(|| bad(format!("{what} needs 'function'")))?;
    only_keys(f, &["name", "description", "parameters", "strict"], &what)?;
    let name = get_str(f, "name", &what)?
        .filter(|s| valid_name(s))
        .ok_or_else(|| bad(format!("{what}: invalid function name")))?;
    let mut fo = Map::new();
    fo.insert("name".into(), Value::String(name.into()));
    if let Some(d) = get_str(f, "description", &what)? {
        fo.insert("description".into(), Value::String(d.into()));
    }
    match f.get("parameters") {
        None | Some(Value::Null) => {}
        Some(p @ Value::Object(_)) => {
            schema(p, 0, &what)?;
            fo.insert("parameters".into(), p.clone());
        }
        Some(_) => return Err(bad(format!("{what}: 'parameters' must be an object"))),
    }
    match f.get("strict") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => {
            fo.insert("strict".into(), Value::Bool(*b));
        }
        Some(_) => return Err(bad(format!("{what}: 'strict' must be a boolean"))),
    }
    Ok(json!({"type": "function", "function": Value::Object(fo)}))
}

fn tool_choice(v: &Value) -> R<Value> {
    match v {
        Value::String(s) if matches!(s.as_str(), "none" | "auto" | "required") => Ok(v.clone()),
        Value::Object(o) => {
            only_keys(o, &["type", "function"], "tool_choice")?;
            if o.get("type").and_then(Value::as_str) != Some("function") {
                return Err(bad("tool_choice: only function choices are allowed"));
            }
            let f = o
                .get("function")
                .and_then(Value::as_object)
                .ok_or_else(|| bad("tool_choice needs 'function'"))?;
            only_keys(f, &["name"], "tool_choice")?;
            let name = get_str(f, "name", "tool_choice")?
                .filter(|s| valid_name(s))
                .ok_or_else(|| bad("tool_choice: invalid function name"))?;
            Ok(json!({"type": "function", "function": {"name": name}}))
        }
        _ => Err(bad(
            "tool_choice must be none, auto, required or a function",
        )),
    }
}

fn response_format(v: &Value) -> R<Value> {
    let o = v
        .as_object()
        .ok_or_else(|| bad("response_format must be an object"))?;
    match o.get("type").and_then(Value::as_str) {
        Some("text") | Some("json_object") => {
            only_keys(o, &["type"], "response_format")?;
            Ok(v.clone())
        }
        Some("json_schema") => {
            only_keys(o, &["type", "json_schema"], "response_format")?;
            let js = o
                .get("json_schema")
                .and_then(Value::as_object)
                .ok_or_else(|| bad("response_format needs 'json_schema'"))?;
            only_keys(
                js,
                &["name", "description", "schema", "strict"],
                "response_format",
            )?;
            get_str(js, "name", "response_format")?
                .filter(|s| valid_name(s))
                .ok_or_else(|| bad("response_format: invalid schema name"))?;
            get_str(js, "description", "response_format")?;
            if let Some(s) = js.get("schema") {
                if !s.is_object() {
                    return Err(bad("response_format: 'schema' must be an object"));
                }
                schema(s, 0, "response_format")?;
            }
            if js
                .get("strict")
                .is_some_and(|s| !s.is_boolean() && !s.is_null())
            {
                return Err(bad("response_format: 'strict' must be a boolean"));
            }
            Ok(v.clone())
        }
        _ => Err(bad(
            "response_format type must be text, json_object or json_schema",
        )),
    }
}

fn number(v: &Value, key: &str, min: f64, max: f64) -> R<Value> {
    match v.as_f64() {
        Some(x) if x.is_finite() && (min..=max).contains(&x) => Ok(v.clone()),
        _ => Err(bad(format!(
            "'{key}' must be a number between {min} and {max}"
        ))),
    }
}

fn integer(v: &Value, key: &str, min: i64, max: i64) -> R<Value> {
    match v.as_i64() {
        Some(x) if (min..=max).contains(&x) => Ok(v.clone()),
        _ => Err(bad(format!(
            "'{key}' must be an integer between {min} and {max}"
        ))),
    }
}

/// Responses wire vocabulary, not a promise that every model accepts all
/// seven levels. A rejection for the selected model is surfaced by the adapter.
pub(crate) fn valid_reasoning_effort(v: &Value) -> bool {
    matches!(
        v.as_str(),
        Some("none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max")
    )
}

/// Check and rebuild a `complete` request. `allowed_models` is the profile's
/// local allowlist.
pub fn sanitize_complete(request: &Value, allowed_models: &[String]) -> R<Value> {
    sanitize_chat(request, allowed_models, false)
}

/// As [`sanitize_complete`], with options explicitly supported by `kind`.
pub fn sanitize_complete_for_kind(
    request: &Value,
    allowed_models: &[String],
    kind: Kind,
) -> R<Value> {
    sanitize_chat(request, allowed_models, kind == Kind::OpenAiOAuth)
}

fn sanitize_chat(request: &Value, allowed_models: &[String], reasoning: bool) -> R<Value> {
    let obj = request
        .as_object()
        .ok_or_else(|| bad("'request' must be an object"))?;
    if !reasoning && obj.contains_key("reasoning_effort") {
        return Err(bad(
            "'reasoning_effort' is not supported by this connection",
        ));
    }
    only_keys(
        obj,
        &[
            "model",
            "messages",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "temperature",
            "top_p",
            "max_tokens",
            "max_completion_tokens",
            "stop",
            "seed",
            "presence_penalty",
            "frequency_penalty",
            "response_format",
            "reasoning_effort",
            "n",
            "stream",
        ],
        "request",
    )?;
    let mut out = Map::new();

    let model = get_str(obj, "model", "request")?.ok_or_else(|| bad("'model' is required"))?;
    if !allowed_models.iter().any(|m| m == model) {
        return Err(SafeError::new(
            "model not allowed: this model is not approved in the connector profile on the device",
        ));
    }
    out.insert("model".into(), Value::String(model.into()));

    let msgs = obj
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("'messages' must be an array"))?;
    if msgs.is_empty() || msgs.len() > MAX_MESSAGES {
        return Err(bad(format!(
            "'messages' must have 1-{MAX_MESSAGES} entries"
        )));
    }
    let msgs = msgs
        .iter()
        .enumerate()
        .map(|(i, m)| message(m, i))
        .collect::<R<Vec<_>>>()?;
    out.insert("messages".into(), Value::Array(msgs));

    if let Some(t) = obj.get("tools").filter(|v| !v.is_null()) {
        let arr = t
            .as_array()
            .ok_or_else(|| bad("'tools' must be an array"))?;
        if arr.len() > MAX_TOOLS {
            return Err(bad(format!("at most {MAX_TOOLS} tools")));
        }
        if !arr.is_empty() {
            let tools = arr
                .iter()
                .enumerate()
                .map(|(i, t)| tool(t, i))
                .collect::<R<Vec<_>>>()?;
            out.insert("tools".into(), Value::Array(tools));
        }
    }
    if let Some(tc) = obj.get("tool_choice").filter(|v| !v.is_null()) {
        out.insert("tool_choice".into(), tool_choice(tc)?);
    }
    for (key, v) in obj {
        if v.is_null() {
            continue;
        }
        let checked = match key.as_str() {
            "parallel_tool_calls" => {
                if !v.is_boolean() {
                    return Err(bad("'parallel_tool_calls' must be a boolean"));
                }
                v.clone()
            }
            "temperature" => number(v, key, 0.0, 2.0)?,
            "top_p" => number(v, key, 0.0, 1.0)?,
            "presence_penalty" | "frequency_penalty" => number(v, key, -2.0, 2.0)?,
            "max_tokens" | "max_completion_tokens" => integer(v, key, 1, 1_000_000)?,
            "seed" => integer(v, key, i64::MIN, i64::MAX)?,
            "n" => integer(v, key, 1, 1)?,
            "stream" => {
                if v != &Value::Bool(false) {
                    return Err(bad("streaming is not supported"));
                }
                continue;
            }
            "stop" => match v {
                Value::String(_) => v.clone(),
                Value::Array(a) if a.len() <= MAX_STOP && a.iter().all(Value::is_string) => {
                    v.clone()
                }
                _ => return Err(bad("'stop' must be a string or up to 4 strings")),
            },
            "response_format" => response_format(v)?,
            "reasoning_effort" => {
                if !valid_reasoning_effort(v) {
                    return Err(bad(
                        "'reasoning_effort' must be none, minimal, low, medium, high, xhigh or max",
                    ));
                }
                v.clone()
            }
            _ => continue, // handled above
        };
        out.insert(key.clone(), checked);
    }
    Ok(Value::Object(out))
}

fn usage(v: &Value) -> Option<Value> {
    let o = v.as_object()?;
    let mut out = Map::new();
    for k in ["prompt_tokens", "completion_tokens", "total_tokens"] {
        if let Some(n) = o.get(k).and_then(Value::as_u64) {
            out.insert(k.into(), Value::from(n));
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

/// Rebuild a provider's Chat Completions response with only the fields Metro
/// needs. `requested_model` is used when the provider doesn't name one.
pub fn sanitize_response(resp: &Value, requested_model: &str) -> R<Value> {
    let bad = |m: &str| SafeError::new(format!("provider returned an unexpected response: {m}"));
    let obj = resp.as_object().ok_or_else(|| bad("not an object"))?;
    let choices = obj
        .get("choices")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| bad("no choices"))?;
    let mut out_choices = Vec::new();
    for (i, c) in choices.iter().enumerate().take(1) {
        let c = c
            .as_object()
            .ok_or_else(|| bad("choice is not an object"))?;
        let m = c
            .get("message")
            .and_then(Value::as_object)
            .ok_or_else(|| bad("choice has no message"))?;
        let content = match m.get("content") {
            None | Some(Value::Null) => Value::Null,
            Some(Value::String(s)) => Value::String(s.clone()),
            Some(Value::Array(parts)) => {
                // Some servers return text parts; flatten them to a string.
                let mut s = String::new();
                for p in parts {
                    if let Some(t) = p.get("text").and_then(Value::as_str) {
                        s.push_str(t);
                    }
                }
                Value::String(s)
            }
            Some(_) => return Err(bad("message content is not text")),
        };
        let mut msg = Map::new();
        msg.insert("role".into(), Value::String("assistant".into()));
        msg.insert("content".into(), content);
        if let Some(tc) = m.get("tool_calls").filter(|v| !v.is_null()) {
            let tc = tool_calls(tc, "response").map_err(|_| bad("invalid tool calls"))?;
            if tc.as_array().is_some_and(|a| !a.is_empty()) {
                msg.insert("tool_calls".into(), tc);
            }
        }
        if let Some(r) = m.get("refusal").and_then(Value::as_str) {
            msg.insert("refusal".into(), Value::String(r.into()));
        }
        let finish = c
            .get("finish_reason")
            .and_then(Value::as_str)
            .filter(|f| {
                matches!(
                    *f,
                    "stop" | "length" | "tool_calls" | "content_filter" | "function_call"
                )
            })
            .map(|f| {
                if f == "function_call" {
                    "tool_calls"
                } else {
                    f
                }
            });
        let finish = match finish {
            Some(f) => Value::String(f.into()),
            None if msg.contains_key("tool_calls") => Value::String("tool_calls".into()),
            None => Value::String("stop".into()),
        };
        out_choices
            .push(json!({"index": i, "message": Value::Object(msg), "finish_reason": finish}));
    }
    let mut out = Map::new();
    if let Some(id) = obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| valid_id(s))
    {
        out.insert("id".into(), Value::String(id.into()));
    }
    out.insert("object".into(), Value::String("chat.completion".into()));
    if let Some(c) = obj.get("created").and_then(Value::as_u64) {
        out.insert("created".into(), Value::from(c));
    }
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| crate::profile::validate_model_name(m).is_ok())
        .unwrap_or(requested_model);
    out.insert("model".into(), Value::String(model.into()));
    out.insert("choices".into(), Value::Array(out_choices));
    if let Some(u) = obj.get("usage").and_then(usage) {
        out.insert("usage".into(), u);
    }
    Ok(Value::Object(out))
}

/// The `models` answer: the locally approved models, in allowlist order,
/// limited to those the provider reports when it reports any.
pub fn approved_models(allowed: &[String], available: Option<&[String]>) -> Value {
    let models: Vec<&String> = allowed
        .iter()
        .filter(|m| available.is_none_or(|a| a.contains(m)))
        .collect();
    json!({ "models": models })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> Vec<String> {
        vec!["gpt-5.5".into(), "llama3".into()]
    }

    fn ok_request() -> Value {
        json!({
            "model": "gpt-5.5",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "file text"}
            ],
            "tools": [{"type": "function", "function": {"name": "read_file", "description": "Read", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "$defs": {"x": {"type": "string"}}, "items": {"$ref": "#/$defs/x"}}}}],
            "tool_choice": "auto",
            "temperature": 0.2,
            "max_tokens": 512,
            "stream": false
        })
    }

    #[test]
    fn accepts_plain_chat_with_function_tools() {
        let out = sanitize_complete(&ok_request(), &allowed()).unwrap();
        assert_eq!(out["model"], "gpt-5.5");
        assert_eq!(out["messages"].as_array().unwrap().len(), 4);
        assert!(out.get("stream").is_none());
        assert_eq!(out["tools"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn enforces_model_allowlist() {
        let mut r = ok_request();
        r["model"] = json!("o9-unapproved");
        let e = sanitize_complete(&r, &allowed()).unwrap_err();
        assert!(e.as_str().starts_with("model not allowed"), "{e}");
        r["model"] = json!("gpt-5.5");
        assert!(sanitize_complete(&r, &[]).is_err());
        let mut r = ok_request();
        r.as_object_mut().unwrap().remove("model");
        assert!(sanitize_complete(&r, &allowed()).is_err());
    }

    #[test]
    fn reasoning_effort_is_preserved_only_for_chatgpt_plan_usage() {
        use crate::openai_oauth::chat_to_responses;
        let mut r = json!({
            "model": "gpt-5.5",
            "messages": [{"role": "user", "content": "private prompt"}],
        });
        for level in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
            r["reasoning_effort"] = json!(level);
            let clean = sanitize_complete_for_kind(&r, &allowed(), Kind::OpenAiOAuth).unwrap();
            assert_eq!(clean["reasoning_effort"], level);
            assert_eq!(
                chat_to_responses(&clean).unwrap()["reasoning"]["effort"],
                level
            );
            for kind in [Kind::ClaudeCode, Kind::OpenAiCompatible] {
                assert!(sanitize_complete_for_kind(&r, &allowed(), kind).is_err());
            }
        }
        for v in [json!("private@example.test"), json!(6), json!({})] {
            r["reasoning_effort"] = v;
            let e = sanitize_complete_for_kind(&r, &allowed(), Kind::OpenAiOAuth)
                .unwrap_err()
                .to_string();
            assert!(e.contains("'reasoning_effort' must be"));
            assert!(!e.contains("private"));
            assert!(chat_to_responses(&r).is_err());
        }
        r["reasoning_effort"] = Value::Null;
        let clean = sanitize_complete_for_kind(&r, &allowed(), Kind::OpenAiOAuth).unwrap();
        assert!(
            chat_to_responses(&clean)
                .unwrap()
                .get("reasoning")
                .is_none()
        );
    }

    #[test]
    fn rejects_remote_fetch_and_hosted_tools() {
        let cases: Vec<(&str, Value)> = vec![
            (
                "image",
                json!({"role": "user", "content": [{"type": "image_url", "image_url": {"url": "https://evil/x.png"}}]}),
            ),
            (
                "file",
                json!({"role": "user", "content": [{"type": "file", "file": {"file_id": "f"}}]}),
            ),
            (
                "audio",
                json!({"role": "user", "content": [{"type": "input_audio", "input_audio": {}}]}),
            ),
            (
                "extra-key-in-part",
                json!({"role": "user", "content": [{"type": "text", "text": "a", "url": "x"}]}),
            ),
            ("bad-role", json!({"role": "function", "content": "x"})),
            (
                "audio-field",
                json!({"role": "assistant", "content": "x", "audio": {"id": "a"}}),
            ),
            (
                "legacy-function-call",
                json!({"role": "assistant", "content": "x", "function_call": {}}),
            ),
        ];
        for (name, msg) in cases {
            let mut r = ok_request();
            r["messages"] = json!([msg]);
            assert!(sanitize_complete(&r, &allowed()).is_err(), "{name}");
        }
        for t in [
            json!({"type": "web_search"}),
            json!({"type": "file_search", "vector_store_ids": ["v"]}),
            json!({"type": "mcp", "server_url": "https://evil"}),
            json!({"type": "code_interpreter"}),
            json!({"type": "function", "function": {"name": "f", "parameters": {"$ref": "https://evil/schema.json"}}}),
            json!({"type": "function", "function": {"name": "f", "parameters": {"$id": "https://evil/s"}}}),
            json!({"type": "function", "function": {"name": "../f"}}),
            json!({"type": "function", "function": {"name": "f"}, "server_url": "x"}),
        ] {
            let mut r = ok_request();
            r["tools"] = json!([t]);
            assert!(sanitize_complete(&r, &allowed()).is_err(), "{r}");
        }
    }

    #[test]
    fn rejects_unknown_and_unsafe_fields() {
        for (k, v) in [
            ("base_url", json!("https://evil")),
            ("api_key", json!("sk-x")),
            ("web_search_options", json!({})),
            ("store", json!(true)),
            ("metadata", json!({})),
            ("user", json!("bob")),
            ("stream", json!(true)),
            ("n", json!(3)),
            ("temperature", json!("hot")),
            ("max_tokens", json!(-1)),
            ("stop", json!(["a", "b", "c", "d", "e"])),
            (
                "response_format",
                json!({"type": "json_schema", "json_schema": {"name": "x", "schema": {"$ref": "http://evil"}}}),
            ),
            ("tool_choice", json!({"type": "web_search"})),
        ] {
            let mut r = ok_request();
            r[k] = v;
            assert!(sanitize_complete(&r, &allowed()).is_err(), "{k}");
        }
    }

    #[test]
    fn operations() {
        assert_eq!(
            parse_operation("models", &json!({})).unwrap(),
            Operation::Models
        );
        assert!(matches!(
            parse_operation("complete", &json!({})).unwrap(),
            Operation::Complete(_)
        ));
        for op in [
            "exec",
            "login",
            "fetch",
            "",
            "MODELS",
            "complete ",
            "Decide",
            "embeddings",
        ] {
            assert!(parse_operation(op, &json!({})).is_err(), "{op:?}");
        }
        assert!(parse_operation("complete", &json!([])).is_err());
        assert!(parse_operation("models", &json!({"url": "x"})).is_err());
    }

    #[test]
    fn each_kind_runs_only_its_capability() {
        let ops = [
            Operation::Models,
            Operation::Complete(json!({})),
            Operation::Decide(json!({})),
            Operation::Embed(json!({})),
            Operation::MediaEmbed(json!({})),
        ];
        let allowed = |k: Kind| -> Vec<bool> {
            ops.iter()
                .map(|o| operation_allowed(k, o).is_ok())
                .collect()
        };
        for k in [Kind::OpenAiOAuth, Kind::ClaudeCode, Kind::OpenAiCompatible] {
            assert_eq!(allowed(k), [true, true, false, false, false], "{k}");
        }
        assert_eq!(allowed(Kind::SystemOne), [true, false, true, false, false]);
        assert_eq!(allowed(Kind::Embeddings), [true, false, false, true, false]);
        assert_eq!(
            allowed(Kind::MediaEmbeddings),
            [true, false, false, true, true]
        );
        let e = operation_allowed(Kind::Embeddings, &ops[4]).unwrap_err();
        assert!(e.as_str().contains("text only"), "{e}");
        let e = operation_allowed(Kind::OpenAiOAuth, &ops[2]).unwrap_err();
        assert!(e.as_str().starts_with("unsupported operation"), "{e}");
    }

    #[test]
    fn typed_requests_need_an_approved_model() {
        let a = vec!["jev-latest".to_string()];
        check_typed(&json!({"model": "jev-latest", "state": "x"}), &a).unwrap();
        for r in [
            json!({"model": "other"}),
            json!({}),
            json!([]),
            json!({"model": 1}),
        ] {
            assert!(check_typed(&r, &a).is_err(), "{r}");
        }
        assert!(check_typed(&json!({"model": "jev-latest"}), &[]).is_err());
    }

    #[test]
    fn response_is_rebuilt() {
        let resp = json!({
            "id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": "gpt-5.5",
            "system_fingerprint": "fp", "service_tier": "x", "vendor_debug": {"token": "sk-secret"},
            "choices": [{"index": 0, "logprobs": {"a": 1}, "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": null, "annotations": [{"url": "x"}],
                "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "read_file", "arguments": {"path": "a"}}}]
            }}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7, "prompt_tokens_details": {}}
        });
        let out = sanitize_response(&resp, "gpt-5.5").unwrap();
        let s = out.to_string();
        assert!(
            !s.contains("sk-secret") && !s.contains("annotations") && !s.contains("fingerprint")
        );
        assert_eq!(
            out["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a\"}"
        );
        assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(out["usage"]["total_tokens"], 7);
        assert!(sanitize_response(&json!({"error": "x"}), "m").is_err());
    }

    #[test]
    fn models_answer() {
        let a = allowed();
        assert_eq!(
            approved_models(&a, None),
            json!({"models": ["gpt-5.5", "llama3"]})
        );
        let avail = vec!["llama3".to_string(), "other".to_string()];
        assert_eq!(
            approved_models(&a, Some(&avail)),
            json!({"models": ["llama3"]})
        );
    }
}
