//! The runner's tools: their model-facing definitions, argument checks, and
//! the room events a call leaves behind.
//!
//! Runner tool names contain a dot (`workspace.list`), which OpenAI-style
//! function names do not allow, so the model sees `workspace_list` and the
//! server translates.

use serde_json::{Map, Value, json};

use crate::AppState;
use crate::db::{Activity, Author, actor, kind};
use crate::jobs::{JobError, JobOutcome};

const MAX_PATH_LEN: usize = 4096;
const MAX_MCP_NAME_LEN: usize = 128;
const MAX_MCP_ARGS_BYTES: usize = 64 * 1024;

const MAX_WRITE_BYTES: usize = 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 16 * 1024;
pub const SHELL_DEFAULT_TIMEOUT_SECS: u64 = 120;
pub const SHELL_MAX_TIMEOUT_SECS: u64 = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerTool {
    List,
    Read,
    Write,
    Shell,
    Check,
    McpList,
    Mcp,
}

impl RunnerTool {
    pub const ALL: [RunnerTool; 7] = [
        RunnerTool::List,
        RunnerTool::Read,
        RunnerTool::Write,
        RunnerTool::Shell,
        RunnerTool::Check,
        RunnerTool::McpList,
        RunnerTool::Mcp,
    ];

    /// The tools an agent gets without an explicit list. Shell stays gated by
    /// the runner's shell policy.
    pub const DEFAULT: [RunnerTool; 7] = [
        RunnerTool::List,
        RunnerTool::Read,
        RunnerTool::Write,
        RunnerTool::Shell,
        RunnerTool::Check,
        RunnerTool::McpList,
        RunnerTool::Mcp,
    ];

    /// The name on the runner wire.
    pub fn runner_name(self) -> &'static str {
        match self {
            RunnerTool::List => "workspace.list",
            RunnerTool::Read => "workspace.read",
            RunnerTool::Write => "workspace.write",
            RunnerTool::Shell => "workspace.shell",
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
            RunnerTool::Write => "workspace_write",
            RunnerTool::Shell => "workspace_shell",
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
            RunnerTool::Write => {
                "Create or replace one UTF-8 text file in the shared workspace (at most 1 MiB). \
                 Missing parent directories are created. Paths are relative to the workspace root."
            }
            RunnerTool::Shell => {
                "Run one shell command in the workspace root on the runner's machine and return \
                 its exit code, stdout and stderr. Depending on the runner's policy a person may \
                 have to approve the command first; a denied or expired command returns an error."
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
            RunnerTool::Write => json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File relative to the workspace root."},
                    "content": {"type": "string", "description": "The file's full new text."}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            RunnerTool::Shell => json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The shell command line."},
                    "timeout_secs": {"type": "integer", "minimum": 1, "maximum": SHELL_MAX_TIMEOUT_SECS,
                                     "description": "Seconds before the command is killed. Defaults to 120."}
                },
                "required": ["command"],
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
            RunnerTool::Write => &["path", "content"],
            RunnerTool::Shell => &["command", "timeout_secs"],
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
            RunnerTool::Write => {
                let path = check_path(obj.get("path").ok_or("`path` is required")?)?;
                let content = obj
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or("`content` must be a string")?;
                if content.len() > MAX_WRITE_BYTES {
                    return Err(format!("`content` exceeds {MAX_WRITE_BYTES} bytes"));
                }
                Ok(json!({"path": path, "content": content}))
            }
            RunnerTool::Shell => {
                let command = obj
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or("`command` must be a string")?;
                if command.trim().is_empty()
                    || command.len() > MAX_COMMAND_BYTES
                    || command.contains('\0')
                {
                    return Err(format!(
                        "`command` must be 1..={MAX_COMMAND_BYTES} bytes without NUL"
                    ));
                }
                let timeout = match obj.get("timeout_secs") {
                    None | Some(Value::Null) => SHELL_DEFAULT_TIMEOUT_SECS,
                    Some(v) => v
                        .as_u64()
                        .filter(|n| (1..=SHELL_MAX_TIMEOUT_SECS).contains(n))
                        .ok_or(format!(
                            "`timeout_secs` must be an integer 1..={SHELL_MAX_TIMEOUT_SECS}"
                        ))?,
                };
                Ok(json!({"command": command, "timeout_secs": timeout}))
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

/// Time a shell job's runner has after its command's own timeout to
/// deliver the result.
const SHELL_LEASE_MARGIN_SECS: u64 = 60;

/// Record that `caller` calls `tool` with `arguments` in `room`.
pub async fn record_call(
    state: &AppState,
    space: &str,
    room: &str,
    caller: &Author,
    activity: Option<&Activity>,
    tool: RunnerTool,
    arguments: &Value,
) {
    let call_text = format!("{} {}", tool.runner_name(), arguments);
    state
        .append_with(
            space,
            room,
            kind::TOOL_CALL,
            caller,
            &truncate(&call_text, state.config.limits.max_event_text),
            activity,
        )
        .await;
}

/// Run `tool` on the runner serving `space` (dedicated or attached), recording the call and its
/// result as shared events in `room`. `caller` is the author of the call event;
/// `activity`, the agent turn or Demo run making the call, tags all of them.
pub async fn call_runner(
    state: &AppState,
    space: &str,
    room: &str,
    caller: &Author,
    activity: Option<&Activity>,
    tool: RunnerTool,
    arguments: Value,
) -> Result<JobOutcome, JobError> {
    record_call(state, space, room, caller, activity, tool, &arguments).await;
    run_recorded(state, space, room, activity, tool, arguments, None).await
}

/// The job of a call [`record_call`] already recorded. With `fence`, the
/// job is queued only while the Space's assignment fence is still that
/// value (an approved shell command never reaches a different runner).
pub async fn run_recorded(
    state: &AppState,
    space: &str,
    room: &str,
    activity: Option<&Activity>,
    tool: RunnerTool,
    arguments: Value,
    fence: Option<i64>,
) -> Result<JobOutcome, JobError> {
    let limits = &state.config.limits;
    let lease = (tool == RunnerTool::Shell).then(|| {
        let secs = arguments["timeout_secs"]
            .as_u64()
            .unwrap_or(SHELL_DEFAULT_TIMEOUT_SECS);
        std::time::Duration::from_secs(secs + SHELL_LEASE_MARGIN_SECS)
    });
    // The job is bound to whoever serves the Space right now; if that
    // changes before a runner answers, the job fails instead of crossing to
    // the new runner.
    let pending = {
        let space = space.to_owned();
        let jobs = state.jobs.clone();
        state
            .db
            .call(move |db| {
                db.with_runner_queue_version(&space, |version| {
                    if fence.is_some_and(|f| f as u64 != version.drop_before) {
                        return Err(JobError::CredentialChanged);
                    }
                    jobs.enqueue_leased(&space, version, tool.runner_name(), arguments, lease)
                })
            })
            .await
    };
    let result = match pending {
        Ok(Some(Ok(waiter))) => waiter.await,
        Ok(Some(Err(e))) => Err(e),
        Ok(None) => Err(JobError::CredentialChanged),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "queuing work for the Space's current runner");
            Err(JobError::CredentialChanged)
        }
    };
    match &result {
        Ok(outcome) => {
            let status = if outcome.ok { "ok" } else { "failed" };
            let text = format!("{} {status}\n{}", tool.runner_name(), outcome.output);
            state
                .append_with(
                    space,
                    room,
                    kind::TOOL_RESULT,
                    &Author::server(actor::RUNNER),
                    &truncate(&text, limits.max_event_text),
                    activity,
                )
                .await;
        }
        Err(e) => {
            state
                .monitor_space(space, crate::monitoring::Code::from(e))
                .await;
            // "Not picked up" says whether the runner is connected at all.
            let why = match e {
                JobError::NotClaimed => crate::runners::not_claimed_reason(state, space).await,
                e => e.to_string(),
            };
            let text = format!("{} failed: {why}", tool.runner_name());
            state
                .append_with(
                    space,
                    room,
                    kind::ERROR,
                    &Author::server(actor::SYSTEM),
                    &text,
                    activity,
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
    fn write_and_shell_arguments() {
        assert_eq!(
            RunnerTool::Shell
                .runner_arguments(&json!({"command": "ls"}))
                .unwrap(),
            json!({"command": "ls", "timeout_secs": SHELL_DEFAULT_TIMEOUT_SECS})
        );
        assert_eq!(
            RunnerTool::Shell
                .runner_arguments(&json!({"command": "ls", "timeout_secs": 600}))
                .unwrap()["timeout_secs"],
            600
        );
        for bad in [
            json!({}),
            json!({"command": " "}),
            json!({"command": "a\u{0}b"}),
            json!({"command": "ls", "timeout_secs": 0}),
            json!({"command": "ls", "timeout_secs": 601}),
            json!({"command": "ls", "timeout_secs": 1.5}),
            json!({"command": "ls", "cwd": "/"}),
            json!({"command": "x".repeat(MAX_COMMAND_BYTES + 1)}),
        ] {
            assert!(RunnerTool::Shell.runner_arguments(&bad).is_err(), "{bad}");
        }
        assert_eq!(
            RunnerTool::Write
                .runner_arguments(&json!({"path": "a.md", "content": ""}))
                .unwrap(),
            json!({"path": "a.md", "content": ""})
        );
        for bad in [
            json!({"path": "a.md"}),
            json!({"content": "x"}),
            json!({"path": "a.md", "content": 3}),
            json!({"path": "a.md", "content": "x".repeat(MAX_WRITE_BYTES + 1)}),
        ] {
            assert!(RunnerTool::Write.runner_arguments(&bad).is_err());
        }
        assert!(RunnerTool::DEFAULT.contains(&RunnerTool::Shell));
        assert!(RunnerTool::DEFAULT.contains(&RunnerTool::Write));
        assert!(
            RunnerTool::DEFAULT
                .iter()
                .all(|t| RunnerTool::ALL.contains(t))
        );
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate("abc", 5), "abc");
        let t = truncate("ééé", 3);
        assert!(t.starts_with("é\n"));
    }
}
