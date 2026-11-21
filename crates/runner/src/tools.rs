//! Tool trait, registry and built-in adapters.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::config::Config;
use crate::confine::Workspace;
use crate::exec;
use crate::mcp::{McpSession, render_call_result};
use crate::util::lossy;

/// Upper bound for any tool output sent back to the server.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub ok: bool,
    pub output: String,
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            ok: true,
            output: output.into(),
        }
    }
    pub fn err(output: impl Into<String>) -> Self {
        Self {
            ok: false,
            output: output.into(),
        }
    }
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn call(&self, args: &Map<String, Value>) -> ToolResult;
}

#[derive(Default)]
pub struct Registry {
    tools: BTreeMap<&'static str, Box<dyn Tool>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name();
        assert!(
            self.tools.insert(name, tool).is_none(),
            "duplicate tool {name}"
        );
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.keys().copied().collect()
    }

    /// The standard tool set for a configuration.
    pub fn standard(cfg: Arc<Config>, ws: Arc<Workspace>) -> Self {
        let mut r = Registry::new();
        r.register(Box::new(ListTool { ws: ws.clone() }));
        r.register(Box::new(ReadTool { ws }));
        r.register(Box::new(CheckTool { cfg: cfg.clone() }));
        r.register(Box::new(McpListTool { cfg: cfg.clone() }));
        r.register(Box::new(McpCallTool { cfg }));
        r
    }

    /// Dispatch a job. Never panics outward; output is size-bounded.
    pub fn dispatch(&self, tool: &str, arguments: &Value) -> ToolResult {
        let Some(t) = self.tools.get(tool) else {
            return ToolResult::err(format!(
                "unknown tool {tool:?}; available: {}",
                self.names().join(", ")
            ));
        };
        let Some(args) = arguments.as_object() else {
            return ToolResult::err("arguments must be a JSON object");
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.call(args)))
            .unwrap_or_else(|_| ToolResult::err(format!("internal error in tool {tool}")));
        ToolResult {
            ok: r.ok,
            output: crate::util::truncate_utf8(r.output, MAX_OUTPUT_BYTES),
        }
    }
}

fn parse_args<T: DeserializeOwned>(args: &Map<String, Value>) -> Result<T, ToolResult> {
    serde_json::from_value(Value::Object(args.clone()))
        .map_err(|e| ToolResult::err(format!("invalid arguments: {e}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpCallArgs {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

pub struct ListTool {
    pub ws: Arc<Workspace>,
}

impl Tool for ListTool {
    fn name(&self) -> &'static str {
        "workspace.list"
    }
    fn call(&self, args: &Map<String, Value>) -> ToolResult {
        let a: PathArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let path = a.path.unwrap_or_else(|| ".".into());
        match self.ws.list(&path) {
            Ok(l) => ToolResult::ok(serde_json::to_string_pretty(&l).unwrap_or_default()),
            Err(e) => ToolResult::err(format!("workspace.list {path:?}: {e}")),
        }
    }
}

pub struct ReadTool {
    pub ws: Arc<Workspace>,
}

impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "workspace.read"
    }
    fn call(&self, args: &Map<String, Value>) -> ToolResult {
        let a: PathArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let Some(path) = a.path.filter(|p| !p.is_empty()) else {
            return ToolResult::err("invalid arguments: missing field `path`");
        };
        match self.ws.read(&path) {
            Ok(c) if c.truncated => ToolResult::ok(format!(
                "{}\n[metro-runner: file truncated, {} bytes total, first {} shown]",
                c.text,
                c.size,
                c.text.len()
            )),
            Ok(c) => ToolResult::ok(c.text),
            Err(e) => ToolResult::err(format!("workspace.read {path:?}: {e}")),
        }
    }
}

pub struct CheckTool {
    pub cfg: Arc<Config>,
}

impl Tool for CheckTool {
    fn name(&self) -> &'static str {
        "workspace.check"
    }
    fn call(&self, args: &Map<String, Value>) -> ToolResult {
        if let Err(e) = parse_args::<NoArgs>(args) {
            return e;
        }
        let Some(argv) = &self.cfg.check_argv else {
            return ToolResult::err(
                "workspace.check is not configured on this runner (METRO_CHECK_ARGV unset)",
            );
        };
        let p = match exec::prepare(argv, &self.cfg.workspace, &self.cfg.sandbox, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        match exec::run_bounded(p, self.cfg.check_timeout) {
            Ok(c) => {
                let code = match c.status.code() {
                    Some(n) => n.to_string(),
                    None => format!(
                        "signal {}",
                        std::os::unix::process::ExitStatusExt::signal(&c.status).unwrap_or(0)
                    ),
                };
                let mut out = format!("exit: {code}\n");
                section(&mut out, "stdout", &c.stdout, c.stdout_total);
                section(&mut out, "stderr", &c.stderr, c.stderr_total);
                ToolResult {
                    ok: c.status.success(),
                    output: out,
                }
            }
            Err(e) => ToolResult::err(format!("workspace.check: {e}")),
        }
    }
}

fn section(out: &mut String, name: &str, data: &[u8], total: u64) {
    out.push_str(&format!("--- {name} ---\n"));
    out.push_str(&lossy(data));
    if total > data.len() as u64 {
        out.push_str(&format!(
            "\n[metro-runner: {name} truncated, {total} bytes total]"
        ));
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

fn mcp_session(cfg: &Config) -> Result<McpSession, ToolResult> {
    let Some(argv) = &cfg.mcp_argv else {
        return Err(ToolResult::err(
            "MCP is not configured on this runner (METRO_MCP_ARGV unset)",
        ));
    };
    McpSession::connect(argv, &cfg.workspace, &cfg.sandbox, cfg.mcp_timeout)
        .map_err(ToolResult::err)
}

pub struct McpListTool {
    pub cfg: Arc<Config>,
}

impl Tool for McpListTool {
    fn name(&self) -> &'static str {
        "workspace.mcp_list"
    }
    fn call(&self, args: &Map<String, Value>) -> ToolResult {
        if let Err(e) = parse_args::<NoArgs>(args) {
            return e;
        }
        let mut s = match mcp_session(&self.cfg) {
            Ok(s) => s,
            Err(e) => return e,
        };
        let r = s.list_tools();
        s.close();
        match r {
            Ok(tools) => ToolResult::ok(serde_json::to_string_pretty(&tools).unwrap_or_default()),
            Err(e) => ToolResult::err(e),
        }
    }
}

pub struct McpCallTool {
    pub cfg: Arc<Config>,
}

impl Tool for McpCallTool {
    fn name(&self) -> &'static str {
        "workspace.mcp"
    }
    fn call(&self, args: &Map<String, Value>) -> ToolResult {
        let a: McpCallArgs = match parse_args(args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        if a.name.is_empty() || a.name.len() > 256 {
            return ToolResult::err("invalid arguments: `name` must be 1..=256 characters");
        }
        let arguments = a.arguments.unwrap_or_else(|| Value::Object(Map::new()));
        if !arguments.is_object() {
            return ToolResult::err("invalid arguments: `arguments` must be an object");
        }
        let mut s = match mcp_session(&self.cfg) {
            Ok(s) => s,
            Err(e) => return e,
        };
        let r = s.call_tool(&a.name, arguments);
        s.close();
        match r {
            Ok(v) => {
                let (ok, text) = render_call_result(&v);
                ToolResult { ok, output: text }
            }
            Err(e) => ToolResult::err(e),
        }
    }
}
