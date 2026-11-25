//! The PageSpec: the checked JSON tree a page's source compiles to. The
//! desktop app draws it with Metro's own components; nothing in it is code.
//!
//! Wire shape (stable, `format: 1`):
//!
//! ```json
//! {"format": 1, "title": "Release watch", "refresh": "5m",
//!  "data": [{"name": "runs", "source": "runs", "params": {"room_id": "r-…", "room": "releases"}}],
//!  "blocks": [{"type": "Stat", "key": "active",
//!              "props": {"label": {"k": "lit", "v": "Active runs"},
//!                        "value": {"k": "bind", "path": ["runs", "active"]}}}]}
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const FORMAT: u32 = 1;

/// Most items any data list holds (the largest `limit` a source allows),
/// and so the most times a `<List>`/`<Each>` repeats its blocks or a
/// `<Table>` has rows.
pub const MAX_ITEMS: usize = 50;
/// The most blocks and table cells a page can render with full data:
/// the compiler refuses pages above it (repetition can't nest), and the
/// renderer must stop drawing past it too.
pub const MAX_RENDERED: usize = 5_000;
/// Most columns in one `<Table>`.
pub const MAX_COLUMNS: usize = 12;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PageSpec {
    pub format: u32,
    pub title: String,
    /// `30s`, `1m`, `5m`, `15m`, `1h` or `off`.
    pub refresh: String,
    #[serde(default)]
    pub data: Vec<DataDecl>,
    /// The `<Page>`'s blocks, in order.
    #[serde(default)]
    pub blocks: Vec<Block>,
}

/// A `<Data name source …/>` declaration, resolved to ids.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DataDecl {
    pub name: String,
    /// `runs`, `messages`, `agentAnswer`, `schedules`, `library` or `agents`.
    pub source: String,
    #[serde(default)]
    pub params: Target,
}

/// Where a data source or action points, as resolved when the version was
/// saved: ids for the server, names as they were then for display.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Target {
    /// A Space a personal page names; absent on Space pages (their own
    /// Space is implied) and for personal Chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    /// The viewer's own personal Chat (`room="chat"` on a personal page).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub chat: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
    /// The room is a thread (never a button's destination).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub thread: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// `agentAnswer`: only answers started from this page's block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// `library`: name contains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// `library`: `document`, `image`, `audio` or `video`; `messages`:
    /// `all`, `messages` or `answers`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    #[serde(rename = "type")]
    pub kind: String,
    /// Unique in the page; absent only on `Column`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub props: BTreeMap<String, Value>,
    /// Text blocks: the pieces of their text, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Block>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
}

impl Block {
    /// This block and every block under it, depth first.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a Block>) {
        out.push(self);
        for c in &self.children {
            c.walk(out);
        }
    }
}

/// A value: a literal, a data read, or (in `when=`) a condition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Value {
    Lit {
        v: serde_json::Value,
    },
    /// A path from a `<Data>` name or an `<Each as>` variable.
    Bind {
        path: Vec<String>,
    },
    Not {
        a: Box<Value>,
    },
    Coalesce {
        a: Box<Value>,
        b: Box<Value>,
    },
    Cmp {
        op: CmpOp,
        a: Box<Value>,
        b: serde_json::Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

/// A button's or link's action, resolved to ids. The prompt is literal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    /// `runAgent`, `runSchedule`, `openRoom`, `openPage` or `refresh`.
    pub kind: String,
    #[serde(default, flatten)]
    pub target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_id: Option<String>,
}

pub mod action {
    pub const RUN_AGENT: &str = "runAgent";
    pub const RUN_SCHEDULE: &str = "runSchedule";
    pub const OPEN_ROOM: &str = "openRoom";
    pub const OPEN_PAGE: &str = "openPage";
    pub const REFRESH: &str = "refresh";
}

impl PageSpec {
    /// Every block, depth first.
    pub fn all_blocks(&self) -> Vec<&Block> {
        let mut out = Vec::new();
        for b in &self.blocks {
            b.walk(&mut out);
        }
        out
    }

    pub fn block(&self, key: &str) -> Option<&Block> {
        self.all_blocks()
            .into_iter()
            .find(|b| b.key.as_deref() == Some(key))
    }

    /// Blocks with an action that runs on the server (agent or schedule).
    pub fn server_actions(&self) -> Vec<&Block> {
        self.all_blocks()
            .into_iter()
            .filter(|b| {
                b.action
                    .as_ref()
                    .is_some_and(|a| a.kind == action::RUN_AGENT || a.kind == action::RUN_SCHEDULE)
            })
            .collect()
    }
}
