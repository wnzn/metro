//! Chat Completions ⇄ Responses, constrained to what the ChatGPT plan usage
//! preview accepts (`preview-limitations`).
//!
//! It's strict: an unknown or unsupported field is an error, never a silent
//! drop. The only exception is assistant `reasoning_content`/`reasoning` in
//! history. That's model-internal text this route can't replay (no encrypted
//! reasoning with Chat history), so it's omitted.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};

use super::TOOL_NAMESPACE;

const ALLOWED_TOP: &[&str] = &[
    "model",
    "messages",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "reasoning_effort",
    "response_format",
    "stream",
    "store",
];

/// Fields the preview rejects or that have no equivalent on this route.
const UNSUPPORTED_TOP: &[(&str, &str)] = &[
    (
        "temperature",
        "temperature is not accepted for ChatGPT plan usage",
    ),
    ("top_p", "top_p is not accepted for ChatGPT plan usage"),
    (
        "max_tokens",
        "max output tokens is not accepted for ChatGPT plan usage",
    ),
    (
        "max_completion_tokens",
        "max output tokens is not accepted for ChatGPT plan usage",
    ),
    ("user", "user is not accepted for ChatGPT plan usage"),
    (
        "metadata",
        "metadata is not accepted for ChatGPT plan usage",
    ),
    ("logprobs", "logprobs are not supported on this route"),
    (
        "top_logprobs",
        "top_logprobs is not accepted for ChatGPT plan usage",
    ),
    ("n", "only one choice (n=1) is supported"),
    ("stop", "stop sequences are not supported on this route"),
    (
        "presence_penalty",
        "presence_penalty is not supported on this route",
    ),
    (
        "frequency_penalty",
        "frequency_penalty is not supported on this route",
    ),
    ("seed", "seed is not supported on this route"),
    ("logit_bias", "logit_bias is not supported on this route"),
    (
        "service_tier",
        "service tier overrides are not supported for ChatGPT plan usage",
    ),
    (
        "audio",
        "audio output is not supported for ChatGPT plan usage",
    ),
    (
        "modalities",
        "output modalities are not supported for ChatGPT plan usage",
    ),
    (
        "prediction",
        "predicted outputs are not supported on this route",
    ),
    (
        "web_search_options",
        "hosted web search is not enabled by this connector",
    ),
    ("functions", "legacy functions are not supported; use tools"),
    (
        "function_call",
        "legacy function_call is not supported; use tool_choice",
    ),
    (
        "stream_options",
        "streaming is not supported by this connector call",
    ),
    (
        "safety_identifier",
        "safety_identifier is not accepted for ChatGPT plan usage",
    ),
    (
        "prompt_cache_key",
        "prompt_cache_key is not supported by this connector",
    ),
    ("verbosity", "verbosity is not supported by this connector"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Function,
    Custom,
}

pub(crate) fn to_responses(req: &Value) -> Result<Value> {
    let obj = req
        .as_object()
        .ok_or_else(|| anyhow!("request must be a JSON object"))?;
    for (k, v) in obj {
        if v.is_null() {
            continue;
        }
        if let Some((_, why)) = UNSUPPORTED_TOP.iter().find(|(f, _)| f == k) {
            if k == "n" && v.as_u64() == Some(1) {
                continue;
            }
            bail!("unsupported request field `{k}`: {why}");
        }
        if !ALLOWED_TOP.contains(&k.as_str()) {
            bail!("unsupported request field `{k}`");
        }
    }
    if obj
        .get("stream")
        .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
    {
        bail!(
            "unsupported request field `stream`: this connector returns one completed chat.completion"
        );
    }
    if obj
        .get("store")
        .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
    {
        bail!("unsupported request field `store`: ChatGPT plan usage requires store:false");
    }
    let model = obj
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| anyhow!("`model` is required"))?;
    let messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| anyhow!("`messages` must be a non-empty array"))?;

    let (tools, kinds) = match obj.get("tools") {
        None | Some(Value::Null) => (Vec::new(), HashMap::new()),
        Some(Value::Array(t)) => tools(t)?,
        Some(_) => bail!("`tools` must be an array"),
    };

    let mut input = Vec::new();
    // Call ID → kind, so each tool result maps to the right output item.
    let mut calls: HashMap<String, Kind> = HashMap::new();
    for (i, m) in messages.iter().enumerate() {
        message(i, m, &kinds, &mut calls, &mut input)?;
    }

    let mut body = Map::new();
    body.insert("model".into(), Value::String(model.to_owned()));
    body.insert("input".into(), Value::Array(input));
    body.insert("store".into(), Value::Bool(false));
    body.insert("stream".into(), Value::Bool(true));
    if !tools.is_empty() {
        body.insert(
            "tools".into(),
            json!([{
                "type": "namespace",
                "name": TOOL_NAMESPACE,
                "description": "Tools provided by Metro for this conversation.",
                "tools": tools,
            }]),
        );
    }
    if let Some(tc) = obj.get("tool_choice").filter(|v| !v.is_null()) {
        match tc.as_str() {
            Some(s @ ("auto" | "none" | "required")) => {
                body.insert("tool_choice".into(), Value::String(s.into()));
            }
            _ => bail!(
                "unsupported tool_choice: only \"auto\", \"none\" and \"required\" are supported \
                 with namespaced tools on this route"
            ),
        }
    }
    if let Some(p) = obj.get("parallel_tool_calls").filter(|v| !v.is_null()) {
        let p = p
            .as_bool()
            .ok_or_else(|| anyhow!("`parallel_tool_calls` must be a boolean"))?;
        body.insert("parallel_tool_calls".into(), Value::Bool(p));
    }
    if let Some(e) = obj.get("reasoning_effort").filter(|v| !v.is_null()) {
        if !crate::policy::valid_reasoning_effort(e) {
            bail!("`reasoning_effort` must be none, minimal, low, medium, high, xhigh or max");
        }
        body.insert("reasoning".into(), json!({ "effort": e }));
    }
    if let Some(rf) = obj.get("response_format").filter(|v| !v.is_null()) {
        body.insert("text".into(), json!({ "format": response_format(rf)? }));
    }
    Ok(Value::Object(body))
}

fn tools(list: &[Value]) -> Result<(Vec<Value>, HashMap<String, Kind>)> {
    let mut out = Vec::new();
    let mut kinds = HashMap::new();
    for t in list {
        let ty = t.get("type").and_then(Value::as_str).unwrap_or("");
        let (kind, spec) = match ty {
            "function" => {
                let f = t
                    .get("function")
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow!("function tool without `function`"))?;
                let name = tool_name(f.get("name"))?;
                let mut spec = Map::new();
                spec.insert("type".into(), "function".into());
                spec.insert("name".into(), name.clone().into());
                // Responses can normalize omitted strictness and require
                // optional fields. Keep Chat Completions' non-strict default.
                spec.insert("strict".into(), Value::Bool(false));
                for (k, v) in f {
                    match k.as_str() {
                        "name" => {}
                        "description" | "parameters" | "strict" => {
                            if !v.is_null() {
                                spec.insert(k.clone(), v.clone());
                            }
                        }
                        other => bail!("unsupported field `{other}` on function tool `{name}`"),
                    }
                }
                (Kind::Function, (name, spec))
            }
            "custom" => {
                let c = t
                    .get("custom")
                    .and_then(Value::as_object)
                    .ok_or_else(|| anyhow!("custom tool without `custom`"))?;
                let name = tool_name(c.get("name"))?;
                let mut spec = Map::new();
                spec.insert("type".into(), "custom".into());
                spec.insert("name".into(), name.clone().into());
                for (k, v) in c {
                    match k.as_str() {
                        "name" => {}
                        "description" | "format" => {
                            if !v.is_null() {
                                spec.insert(k.clone(), v.clone());
                            }
                        }
                        other => bail!("unsupported field `{other}` on custom tool `{name}`"),
                    }
                }
                (Kind::Custom, (name, spec))
            }
            other => bail!(
                "unsupported tool type `{other}`: only function and custom tools are supported for \
                 ChatGPT plan usage (hosted tools such as file search, code interpreter, image \
                 generation, computer use, MCP and tool_search are not)"
            ),
        };
        let (name, spec) = spec;
        if kinds.insert(name.clone(), kind).is_some() {
            bail!("duplicate tool name `{name}`");
        }
        out.push(Value::Object(spec));
    }
    Ok((out, kinds))
}

fn tool_name(v: Option<&Value>) -> Result<String> {
    match v.and_then(Value::as_str) {
        Some(n) if !n.is_empty() && n.len() <= 128 => Ok(n.to_owned()),
        _ => bail!("tool `name` is missing or invalid"),
    }
}

fn message(
    i: usize,
    m: &Value,
    kinds: &HashMap<String, Kind>,
    calls: &mut HashMap<String, Kind>,
    input: &mut Vec<Value>,
) -> Result<()> {
    let o = m
        .as_object()
        .ok_or_else(|| anyhow!("messages[{i}] must be an object"))?;
    let role = o
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("messages[{i}].role missing"))?;
    let allowed: &[&str] = match role {
        "system" | "developer" => &["role", "content"],
        "user" => &["role", "content"],
        "assistant" => &[
            "role",
            "content",
            "tool_calls",
            "refusal",
            "reasoning_content",
            "reasoning",
        ],
        "tool" => &["role", "content", "tool_call_id"],
        "function" => {
            bail!("messages[{i}]: legacy `function` role is not supported; use tool messages")
        }
        other => bail!("messages[{i}]: unsupported role `{other}`"),
    };
    for (k, v) in o {
        if !v.is_null() && !allowed.contains(&k.as_str()) {
            bail!("messages[{i}]: unsupported field `{k}` on a {role} message");
        }
    }
    let content = o.get("content").filter(|v| !v.is_null());
    match role {
        // Explicit system items are rejected by this route, so send them as
        // developer messages.
        "system" | "developer" => {
            let text = text_only(content, i)?;
            input.push(json!({ "type": "message", "role": "developer", "content": text }));
        }
        "user" => {
            let parts = user_parts(content, i)?;
            input.push(json!({ "type": "message", "role": "user", "content": parts }));
        }
        "assistant" => {
            let mut text = match content {
                None => String::new(),
                Some(c) => text_only(Some(c), i)?,
            };
            if text.is_empty()
                && let Some(r) = o.get("refusal").and_then(Value::as_str)
            {
                text = r.to_owned();
            }
            if !text.is_empty() {
                input.push(json!({
                    "role": "assistant",
                    "content": text,
                }));
            }
            if let Some(tc) = o.get("tool_calls").filter(|v| !v.is_null()) {
                let tc = tc
                    .as_array()
                    .ok_or_else(|| anyhow!("messages[{i}].tool_calls must be an array"))?;
                for (j, c) in tc.iter().enumerate() {
                    let id = c
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| anyhow!("messages[{i}].tool_calls[{j}].id missing"))?;
                    match c.get("type").and_then(Value::as_str).unwrap_or("function") {
                        "function" => {
                            let f = c.get("function").ok_or_else(|| {
                                anyhow!("messages[{i}].tool_calls[{j}].function missing")
                            })?;
                            let name = tool_name(f.get("name"))?;
                            check_known(kinds, &name, Kind::Function)?;
                            let args = match f.get("arguments") {
                                Some(Value::String(s)) => s.clone(),
                                Some(Value::Null) | None => "{}".into(),
                                Some(v) => v.to_string(),
                            };
                            calls.insert(id.into(), Kind::Function);
                            input.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "namespace": TOOL_NAMESPACE,
                                "name": name,
                                "arguments": args,
                            }));
                        }
                        "custom" => {
                            let cu = c.get("custom").ok_or_else(|| {
                                anyhow!("messages[{i}].tool_calls[{j}].custom missing")
                            })?;
                            let name = tool_name(cu.get("name"))?;
                            check_known(kinds, &name, Kind::Custom)?;
                            let inp = cu.get("input").and_then(Value::as_str).unwrap_or("");
                            calls.insert(id.into(), Kind::Custom);
                            input.push(json!({
                                "type": "custom_tool_call",
                                "call_id": id,
                                "namespace": TOOL_NAMESPACE,
                                "name": name,
                                "input": inp,
                            }));
                        }
                        other => bail!("messages[{i}].tool_calls[{j}]: unsupported type `{other}`"),
                    }
                }
            }
        }
        "tool" => {
            let id = o
                .get("tool_call_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("messages[{i}].tool_call_id missing"))?;
            let kind = calls
                .get(id)
                .copied()
                .ok_or_else(|| anyhow!("messages[{i}]: tool result for unknown call id `{id}`"))?;
            let output = text_only(content, i)?;
            let ty = match kind {
                Kind::Function => "function_call_output",
                Kind::Custom => "custom_tool_call_output",
            };
            input.push(json!({ "type": ty, "call_id": id, "output": output }));
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// History may reference a tool that this request doesn't declare. That's
/// legitimate in a multi-round history, so it's allowed, but only with the
/// same kind as the declared tool.
fn check_known(kinds: &HashMap<String, Kind>, name: &str, kind: Kind) -> Result<()> {
    match kinds.get(name) {
        Some(k) if *k != kind => {
            bail!("history calls `{name}` as a different tool kind than declared")
        }
        _ => Ok(()),
    }
}

fn text_only(content: Option<&Value>, i: usize) -> Result<String> {
    match content {
        None => Ok(String::new()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => out += p.get("text").and_then(Value::as_str).unwrap_or(""),
                    Some(other) => bail!(
                        "messages[{i}]: content part `{other}` is not supported in this message role"
                    ),
                    None => bail!("messages[{i}]: content part without type"),
                }
            }
            Ok(out)
        }
        Some(_) => bail!("messages[{i}].content must be a string or an array"),
    }
}

fn user_parts(content: Option<&Value>, i: usize) -> Result<Value> {
    match content {
        None => bail!("messages[{i}]: user message without content"),
        Some(Value::String(s)) => Ok(json!([{ "type": "input_text", "text": s }])),
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => out.push(json!({
                        "type": "input_text",
                        "text": p.get("text").and_then(Value::as_str).unwrap_or(""),
                    })),
                    Some("image_url") => {
                        let iu = p.get("image_url").ok_or_else(|| {
                            anyhow!("messages[{i}]: image_url part without image_url")
                        })?;
                        let (url, detail) = match iu {
                            Value::String(s) => (s.as_str(), "auto"),
                            _ => (
                                iu.get("url").and_then(Value::as_str).ok_or_else(|| {
                                    anyhow!("messages[{i}]: image_url.url missing")
                                })?,
                                iu.get("detail").and_then(Value::as_str).unwrap_or("auto"),
                            ),
                        };
                        out.push(
                            json!({ "type": "input_image", "image_url": url, "detail": detail }),
                        );
                    }
                    Some("file") => {
                        let f = p
                            .get("file")
                            .ok_or_else(|| anyhow!("messages[{i}]: file part without file"))?;
                        if f.get("file_id").is_some_and(|v| !v.is_null()) {
                            bail!(
                                "messages[{i}]: file_id inputs need the Files API, which ChatGPT plan usage does not support; send file_data"
                            );
                        }
                        let data = f
                            .get("file_data")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow!("messages[{i}]: file part without file_data"))?;
                        let mut item = json!({ "type": "input_file", "file_data": data });
                        if let Some(n) = f.get("filename").and_then(Value::as_str) {
                            item["filename"] = n.into();
                        }
                        out.push(item);
                    }
                    Some("input_audio") => {
                        bail!("messages[{i}]: audio input is not supported for ChatGPT plan usage")
                    }
                    Some(other) => bail!("messages[{i}]: unsupported content part `{other}`"),
                    None => bail!("messages[{i}]: content part without type"),
                }
            }
            Ok(Value::Array(out))
        }
        Some(_) => bail!("messages[{i}].content must be a string or an array"),
    }
}

fn response_format(rf: &Value) -> Result<Value> {
    match rf.get("type").and_then(Value::as_str) {
        Some("text") => Ok(json!({ "type": "text" })),
        Some("json_object") => Ok(json!({ "type": "json_object" })),
        Some("json_schema") => {
            let js = rf
                .get("json_schema")
                .ok_or_else(|| anyhow!("response_format json_schema missing"))?;
            let mut f = Map::new();
            f.insert("type".into(), "json_schema".into());
            f.insert(
                "name".into(),
                js.get("name")
                    .cloned()
                    .ok_or_else(|| anyhow!("response_format json_schema.name missing"))?,
            );
            for k in ["schema", "strict", "description"] {
                if let Some(v) = js.get(k).filter(|v| !v.is_null()) {
                    f.insert(k.into(), v.clone());
                }
            }
            Ok(Value::Object(f))
        }
        _ => bail!("unsupported response_format"),
    }
}

/// Normalizes a completed Responses object into a `chat.completion`.
pub(crate) fn to_chat_completion(r: &Value) -> Result<Value> {
    if let Some(s) = r.get("status").and_then(Value::as_str)
        && s != "completed"
    {
        bail!("Responses status is `{s}`, not completed");
    }
    let output = r
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("completed response has no output"))?;
    let mut text = String::new();
    let mut refusal = String::new();
    let mut tool_calls = Vec::new();
    for item in output {
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "message" => {
                for c in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match c.get("type").and_then(Value::as_str) {
                        Some("output_text") => {
                            text += c.get("text").and_then(Value::as_str).unwrap_or("")
                        }
                        Some("refusal") => {
                            refusal += c.get("refusal").and_then(Value::as_str).unwrap_or("")
                        }
                        Some(other) => bail!("unsupported Responses message content `{other}`"),
                        None => {}
                    }
                }
            }
            "function_call" => {
                check_namespace(item)?;
                let id = call_id(item)?;
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": item.get("name").and_then(Value::as_str).ok_or_else(|| anyhow!("function_call without name"))?,
                        "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
                    },
                }));
            }
            "custom_tool_call" => {
                check_namespace(item)?;
                let id = call_id(item)?;
                tool_calls.push(json!({
                    "id": id,
                    "type": "custom",
                    "custom": {
                        "name": item.get("name").and_then(Value::as_str).ok_or_else(|| anyhow!("custom_tool_call without name"))?,
                        "input": item.get("input").and_then(Value::as_str).unwrap_or(""),
                    },
                }));
            }
            // Reasoning is model-internal. Only public text output is
            // surfaced, and encrypted or raw reasoning is never forwarded.
            "reasoning" => {}
            other => bail!(
                "unsupported Responses output item `{other}`; this connector only requests text and function/custom tool calls"
            ),
        }
    }
    if text.is_empty() && refusal.is_empty() && tool_calls.is_empty() {
        bail!("completed response has no public text, refusal or tool calls");
    }
    let mut message = Map::new();
    message.insert("role".into(), "assistant".into());
    message.insert(
        "content".into(),
        if text.is_empty() && !tool_calls.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        },
    );
    if !refusal.is_empty() {
        message.insert("refusal".into(), Value::String(refusal));
    }
    let finish = if tool_calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    let mut out = json!({
        "id": r.get("id").cloned().unwrap_or(Value::Null),
        "object": "chat.completion",
        "created": r.get("created_at").cloned().unwrap_or(Value::Null),
        "model": r.get("model").cloned().unwrap_or(Value::Null),
        "choices": [{ "index": 0, "message": message, "finish_reason": finish }],
    });
    if let Some(u) = r.get("usage").filter(|u| u.is_object()) {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let mut usage = json!({
            "prompt_tokens": n("input_tokens"),
            "completion_tokens": n("output_tokens"),
            "total_tokens": n("total_tokens"),
        });
        if let Some(c) = u
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
        {
            usage["prompt_tokens_details"] = json!({ "cached_tokens": c });
        }
        if let Some(c) = u
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
        {
            usage["completion_tokens_details"] = json!({ "reasoning_tokens": c });
        }
        out["usage"] = usage;
    }
    Ok(out)
}

fn check_namespace(item: &Value) -> Result<()> {
    match item.get("namespace") {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(n)) if n == TOOL_NAMESPACE => Ok(()),
        Some(_) => bail!("model called a tool outside the `{TOOL_NAMESPACE}` namespace"),
    }
}

fn call_id(item: &Value) -> Result<String> {
    item.get("call_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("tool call without call_id"))
}
