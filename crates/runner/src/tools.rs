//! Tool trait, registry and built-in adapters.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::config::{Config, Credential, Sandbox};
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

    /// The standard tool set for a dedicated (legacy) credential: every tool
    /// works on `ws`, and check/MCP run under the configured sandbox (host
    /// execution stays the operator's opt-in, as before).
    pub fn standard(cfg: Arc<Config>, ws: Arc<Workspace>) -> Self {
        Self::build(cfg, ws, SubprocessPolicy::Allowed)
    }

    /// The tool set for one job of a reusable credential, bound to that
    /// Space's root only. Built fresh per job so nothing carries over from a
    /// previous Space.
    pub fn for_space(cfg: Arc<Config>, ws: Arc<Workspace>) -> Self {
        let policy = SubprocessPolicy::for_config(&cfg);
        Self::build(cfg, ws, policy)
    }

    fn build(cfg: Arc<Config>, ws: Arc<Workspace>, policy: SubprocessPolicy) -> Self {
        let ctx = Arc::new(ExecCtx {
            cfg,
            ws: ws.clone(),
            policy,
        });
        let mut r = Registry::new();
        r.register(Box::new(ListTool { ws: ws.clone() }));
        r.register(Box::new(ReadTool { ws }));
        r.register(Box::new(CheckTool { ctx: ctx.clone() }));
        r.register(Box::new(McpListTool { ctx: ctx.clone() }));
        r.register(Box::new(McpCallTool { ctx }));
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

/// Whether check/MCP subprocesses may run for a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubprocessPolicy {
    Allowed,
    Disabled(String),
}

/// Why check/MCP are off for a reusable credential, if they are.
///
/// Trust decision: a reusable runner serves several Spaces from one host. A
/// host subprocess (`METRO_SANDBOX=none`) runs as the runner user and could
/// read sibling Space roots, so per-Space isolation would rest on the tool
/// behaving. Reusable credentials therefore run check/MCP only inside the
/// docker sandbox, where the container sees just that Space's root (read-only,
/// no network, empty environment). The built-in file tools work in any mode.
pub const REUSABLE_HOST_EXEC_DISABLED: &str = "is disabled: this runner uses a reusable \
     (multi-Space) credential with METRO_SANDBOX=none, and a host subprocess could read other \
     Spaces' workspaces. Set METRO_SANDBOX=docker with a pinned METRO_SANDBOX_IMAGE to enable it, \
     or use a dedicated runner credential for this Space";

impl SubprocessPolicy {
    pub fn for_config(cfg: &Config) -> Self {
        match (cfg.credential, &cfg.sandbox) {
            (Credential::Dedicated, _) | (Credential::Reusable, Sandbox::Docker(_)) => {
                SubprocessPolicy::Allowed
            }
            (Credential::Reusable, Sandbox::None) => {
                SubprocessPolicy::Disabled(REUSABLE_HOST_EXEC_DISABLED.into())
            }
        }
    }
}

/// What check/MCP need: config, the job's workspace root and the policy.
pub struct ExecCtx {
    pub cfg: Arc<Config>,
    pub ws: Arc<Workspace>,
    pub policy: SubprocessPolicy,
}

impl ExecCtx {
    /// The directory to run in / bind-mount, after re-checking that its path
    /// still names the root we opened.
    fn workdir(&self, tool: &str) -> Result<&std::path::Path, ToolResult> {
        if let SubprocessPolicy::Disabled(why) = &self.policy {
            return Err(ToolResult::err(format!("{tool} {why}")));
        }
        self.ws.verify_root_path().map_err(|e| {
            ToolResult::err(format!(
                "{tool}: workspace root changed or is not a plain directory ({e}); refusing to run"
            ))
        })?;
        Ok(self.ws.root_path())
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
    pub ctx: Arc<ExecCtx>,
}

impl Tool for CheckTool {
    fn name(&self) -> &'static str {
        "workspace.check"
    }
    fn call(&self, args: &Map<String, Value>) -> ToolResult {
        if let Err(e) = parse_args::<NoArgs>(args) {
            return e;
        }
        let cfg = &self.ctx.cfg;
        let Some(argv) = &cfg.check_argv else {
            return ToolResult::err(
                "workspace.check is not configured on this runner (METRO_CHECK_ARGV unset)",
            );
        };
        let dir = match self.ctx.workdir("workspace.check") {
            Ok(d) => d,
            Err(e) => return e,
        };
        let p = match exec::prepare(argv, dir, &cfg.sandbox, false) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        match exec::run_bounded(p, cfg.check_timeout) {
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

fn mcp_session(ctx: &ExecCtx, tool: &str) -> Result<McpSession, ToolResult> {
    let cfg = &ctx.cfg;
    let Some(argv) = &cfg.mcp_argv else {
        return Err(ToolResult::err(
            "MCP is not configured on this runner (METRO_MCP_ARGV unset)",
        ));
    };
    let dir = ctx.workdir(tool)?;
    McpSession::connect(argv, dir, &cfg.sandbox, cfg.mcp_timeout).map_err(ToolResult::err)
}

pub struct McpListTool {
    pub ctx: Arc<ExecCtx>,
}

impl Tool for McpListTool {
    fn name(&self) -> &'static str {
        "workspace.mcp_list"
    }
    fn call(&self, args: &Map<String, Value>) -> ToolResult {
        if let Err(e) = parse_args::<NoArgs>(args) {
            return e;
        }
        let mut s = match mcp_session(&self.ctx, "workspace.mcp_list") {
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
    pub ctx: Arc<ExecCtx>,
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
        let mut s = match mcp_session(&self.ctx, "workspace.mcp") {
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
