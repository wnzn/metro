//! The runner's tools: their model-facing definitions, argument checks, and
//! the room events a call leaves behind.
//!
//! Runner tool names contain a dot (`workspace.list`), which OpenAI-style
//! function names do not allow, so the model sees `workspace_list` and the
//! server translates.

use serde_json::{Map, Value, json};

use crate::AppState;
use crate::db::{Author, actor, kind};
use crate::jobs::{JobError, JobOutcome};

const MAX_PATH_LEN: usize = 4096;
const MAX_MCP_NAME_LEN: usize = 128;
const MAX_MCP_ARGS_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerTool {
    List,
    Read,
    Check,
    McpList,
    Mcp,
}

impl RunnerTool {
    pub const ALL: [RunnerTool; 5] = [
        RunnerTool::List,
        RunnerTool::Read,
        RunnerTool::Check,
        RunnerTool::McpList,
        RunnerTool::Mcp,
    ];

    /// The name on the runner wire.
    pub fn runner_name(self) -> &'static str {
        match self {
            RunnerTool::List => "workspace.list",
            RunnerTool::Read => "workspace.read",
            RunnerTool::Check => "workspace.check",
            RunnerTool::McpList => "workspace.mcp_list",
            RunnerTool::Mcp => "workspace.mcp",
        }
    }

    /// The name the model calls.
    pub fn model_name(self) -> &'static str {
        match self {
            RunnerTool::List => "workspace_list",
            RunnerTool::Read => "workspace_read",
            RunnerTool::Check => "workspace_check",
            RunnerTool::McpList => "workspace_mcp_list",
            RunnerTool::Mcp => "workspace_mcp",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            RunnerTool::List => {
                "List the entries of a directory in the shared workspace on the runner's machine. \
                 Paths are relative to the workspace root."
            }
            RunnerTool::Read => {
                "Read a text file from the shared workspace. Paths are relative to the workspace root."
            }
            RunnerTool::Check => {
                "Run the workspace's operator-configured check command (for example tests or a build) \
                 and return its output."
            }
            RunnerTool::McpList => {
                "List the MCP tools the runner offers, with their input schemas."
            }
            RunnerTool::Mcp => {
                "Call one MCP tool on the runner by name. Use workspace_mcp_list first to learn \
                 the names and argument schemas."
            }
        }
    }

    pub fn parameters(self) -> Value {
        match self {
            RunnerTool::List => json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory relative to the workspace root. Defaults to \".\"."}
                },
                "additionalProperties": false
            }),
            RunnerTool::Read => json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File relative to the workspace root."}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            RunnerTool::Check | RunnerTool::McpList => json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            RunnerTool::Mcp => json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "MCP tool name from workspace_mcp_list."},
                    "arguments": {"type": "object", "description": "Arguments for the MCP tool."}
                },
                "required": ["name", "arguments"],
                "additionalProperties": false
            }),
        }
    }

    /// Check model-supplied `args` and build the runner's arguments object.
    pub fn runner_arguments(self, args: &Value) -> Result<Value, String> {
        let empty = Map::new();
        let obj = match args {
            Value::Object(obj) => obj,
            // Some models send `null` for tools without parameters.
            Value::Null => &empty,
            _ => return Err("arguments must be a JSON object".into()),
        };
        let allowed: &[&str] = match self {
            RunnerTool::List | RunnerTool::Read => &["path"],
            RunnerTool::Check | RunnerTool::McpList => &[],
            RunnerTool::Mcp => &["name", "arguments"],
        };
        if let Some(extra) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(format!("unknown argument `{extra}`"));
        }
        match self {
            RunnerTool::List => {
                let path = match obj.get("path") {
                    None | Some(Value::Null) => ".".to_owned(),
                    Some(v) => check_path(v)?,
                };
                Ok(json!({"path": path}))
            }
            RunnerTool::Read => {
                let path = check_path(obj.get("path").ok_or("`path` is required")?)?;
                Ok(json!({"path": path}))
            }
            RunnerTool::Check | RunnerTool::McpList => Ok(json!({})),
            RunnerTool::Mcp => {
                let name = obj
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or("`name` must be a string")?;
                if name.is_empty()
                    || name.len() > MAX_MCP_NAME_LEN
                    || name.chars().any(char::is_control)
                {
                    return Err(format!(
                        "`name` must be 1..={MAX_MCP_NAME_LEN} printable characters"
                    ));
                }
                let arguments = match obj.get("arguments") {
                    None | Some(Value::Null) => Value::Object(Map::new()),
                    Some(v @ Value::Object(_)) => v.clone(),
                    Some(_) => return Err("`arguments` must be an object".into()),
                };
                if arguments.to_string().len() > MAX_MCP_ARGS_BYTES {
                    return Err(format!("`arguments` exceed {MAX_MCP_ARGS_BYTES} bytes"));
                }
                Ok(json!({"name": name, "arguments": arguments}))
            }
        }
    }
}

fn check_path(v: &Value) -> Result<String, String> {
    let path = v.as_str().ok_or("`path` must be a string")?;
    if path.is_empty() || path.len() > MAX_PATH_LEN || path.contains('\0') {
        return Err(format!(
            "`path` must be 1..={MAX_PATH_LEN} bytes without NUL"
        ));
    }
    // The runner confines paths to its workspace; this is only a cheap first filter.
    Ok(path.to_owned())
}

/// Cut `text` to at most `max` bytes on a character boundary, saying so.
pub fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… [truncated {} of {} bytes]",
        &text[..end],
        text.len() - end,
        text.len()
    )
}

/// Run `tool` on the paired runner of `space`, recording the call and its
/// result as shared events in `room`. `caller` is the author of the call event.
pub async fn call_runner(
    state: &AppState,
    space: &str,
    room: &str,
    caller: &Author,
    tool: RunnerTool,
    arguments: Value,
) -> Result<JobOutcome, JobError> {
    let limits = &state.config.limits;
    let call_text = format!("{} {}", tool.runner_name(), arguments);
    state
        .append(
            space,
            room,
            kind::TOOL_CALL,
            caller,
            &truncate(&call_text, limits.max_event_text),
        )
        .await;

    let result = state.jobs.run(space, tool.runner_name(), arguments).await;
    match &result {
        Ok(outcome) => {
            let status = if outcome.ok { "ok" } else { "failed" };
            let text = format!("{} {status}\n{}", tool.runner_name(), outcome.output);
            state
                .append(
                    space,
                    room,
                    kind::TOOL_RESULT,
                    &Author::server(actor::RUNNER),
                    &truncate(&text, limits.max_event_text),
                )
                .await;
        }
        Err(e) => {
            let text = format!("{} failed: {e}", tool.runner_name());
            state
                .append(
                    space,
                    room,
                    kind::ERROR,
                    &Author::server(actor::SYSTEM),
                    &text,
                )
                .await;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_defaults_path() {
        assert_eq!(
            RunnerTool::List.runner_arguments(&json!({})).unwrap(),
            json!({"path": "."})
        );
        assert_eq!(
            RunnerTool::List.runner_arguments(&Value::Null).unwrap(),
            json!({"path": "."})
        );
        assert_eq!(
            RunnerTool::List
                .runner_arguments(&json!({"path": "src"}))
                .unwrap(),
            json!({"path": "src"})
        );
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(RunnerTool::Read.runner_arguments(&json!({})).is_err());
        assert!(
            RunnerTool::Read
                .runner_arguments(&json!({"path": 3}))
                .is_err()
        );
        assert!(
            RunnerTool::Check
                .runner_arguments(&json!({"x": 1}))
                .is_err()
        );
        assert!(
            RunnerTool::Mcp
                .runner_arguments(&json!({"name": "a", "arguments": []}))
                .is_err()
        );
        assert!(RunnerTool::List.runner_arguments(&json!([1])).is_err());
        assert_eq!(
            RunnerTool::Mcp
                .runner_arguments(&json!({"name": "t", "arguments": {"q": 1}}))
                .unwrap(),
            json!({"name": "t", "arguments": {"q": 1}})
        );
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate("abc", 5), "abc");
        let t = truncate("ééé", 3);
        assert!(t.starts_with("é\n"));
    }
}
