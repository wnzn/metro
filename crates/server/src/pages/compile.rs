//! Page TSX → PageSpec: the block catalogue, property types, scopes, keys,
//! bounds, and room/agent name resolution.
//!
//! [`compile`] never stops at the first problem it can recover from: it
//! returns every diagnostic (up to [`MAX_DIAGNOSTICS`]) so a model can fix
//! them all in one retry. A source with any diagnostic has no spec.
//!
//! Every block except `Page`, `Data` and `Column` has a `key`, unique in the
//! page. A missing key is not an error: the compiler adds a deterministic
//! one (`stat-1`, `button-2`, …) and returns the source with it inserted,
//! which is what gets stored.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::spec::{self, Action, Block, DataDecl, PageSpec, Target, Value};
use super::tsx::{self, Attr, AttrValue, Child, Diagnostic, Element, ExprKind};

pub const MAX_BLOCKS: usize = 400;
pub const MAX_DATA: usize = 16;
pub const MAX_ACTIONS: usize = 32;
pub const MAX_DIAGNOSTICS: usize = 25;
pub const MAX_TITLE_CHARS: usize = 120;
pub const MAX_TEXT_CHARS: usize = 4_000;
pub const MAX_PROMPT_CHARS: usize = 2_000;
const MAX_KEY_CHARS: usize = 64;

pub const REFRESH: &[&str] = &["30s", "1m", "5m", "15m", "1h", "off"];
pub const TONES: &[&str] = &["neutral", "info", "success", "warning", "danger"];
pub const FORMATS: &[&str] = &[
    "text", "number", "percent", "relative", "datetime", "status", "bytes",
];
pub const SOURCES: &[&str] = &[
    "runs",
    "messages",
    "agentAnswer",
    "schedules",
    "library",
    "agents",
];
pub const ACTIONS: &[&str] = &[
    spec::action::RUN_AGENT,
    spec::action::RUN_SCHEDULE,
    spec::action::OPEN_ROOM,
    spec::action::OPEN_PAGE,
    spec::action::REFRESH,
];

/// Why a personal page's button can't point at a Space.
pub const PERSONAL_ONLY: &str = "a personal page's buttons only start private runs shown on the \
     page: use action=\"runAgent\" room=\"chat\" (without space or agent); Space rooms can't be \
     a personal page's destination";

/// The personal Chat, as a room name on personal pages.
pub const CHAT_ROOM: &str = "chat";

/// Resolves names in a page's source to ids, as the person saving it.
/// Errors are shown to them (and to the model) as written.
pub trait Resolver {
    /// The page's own Space; `None` for a personal page.
    fn page_space(&self) -> Option<&str>;
    /// A Space a personal page names, by id: its name, if the saver is a
    /// member now.
    fn space(&mut self, id: &str) -> Result<String, String>;
    /// A room or thread of `space`, by name (or id): its id, its name, and
    /// whether it's a thread.
    fn room(&mut self, space: &str, name: &str) -> Result<(String, String, bool), String>;
    /// An agent of `space`, by handle: its id and handle.
    fn agent(&mut self, space: &str, handle: &str) -> Result<(String, String), String>;
}

/// The result of compiling a source.
#[derive(Clone, Debug)]
pub struct Compiled {
    /// The source with generated keys inserted.
    pub source: String,
    /// Present exactly when there are no diagnostics.
    pub spec: Option<PageSpec>,
    pub diagnostics: Vec<Diagnostic>,
}

// ---- Catalogue ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum P {
    /// A literal string.
    Str,
    /// A literal string or a data read.
    Text,
    /// A literal number.
    Num,
    /// A literal number or a data read.
    NumOrBind,
    Bool,
    Enum(&'static [&'static str]),
    /// A data read.
    Bind,
    /// A variable name.
    Ident,
    /// `when=`: a read, `!`, `??` or a comparison.
    Cond,
    /// A literal or a data read.
    Any,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kids {
    None,
    /// Blocks (and, for `Page`, `<Data>`).
    Blocks,
    /// Text and `{reads}`.
    Text,
    /// Literal text only.
    LiteralText,
    Columns,
}

struct Def {
    name: &'static str,
    props: &'static [(&'static str, P, bool)],
    kids: Kids,
    keyed: bool,
}

const LEVELS: &[&str] = &["1", "2", "3"];
const CHART_KINDS: &[&str] = &["line", "bar"];

const DEFS: &[Def] = &[
    Def {
        name: "Page",
        props: &[
            ("title", P::Str, true),
            ("refresh", P::Enum(REFRESH), false),
        ],
        kids: Kids::Blocks,
        keyed: false,
    },
    Def {
        name: "Data",
        props: &[
            ("name", P::Ident, true),
            ("source", P::Enum(SOURCES), true),
            ("space", P::Str, false),
            ("room", P::Str, false),
            ("agent", P::Str, false),
            ("block", P::Str, false),
            ("limit", P::Num, false),
            ("query", P::Str, false),
            ("kind", P::Str, false),
        ],
        kids: Kids::None,
        keyed: false,
    },
    Def {
        name: "Section",
        props: &[("title", P::Text, false), ("collapsed", P::Bool, false)],
        kids: Kids::Blocks,
        keyed: true,
    },
    Def {
        name: "Row",
        props: &[],
        kids: Kids::Blocks,
        keyed: true,
    },
    Def {
        name: "Grid",
        props: &[("columns", P::Num, false)],
        kids: Kids::Blocks,
        keyed: true,
    },
    Def {
        name: "Divider",
        props: &[],
        kids: Kids::None,
        keyed: true,
    },
    Def {
        name: "Heading",
        props: &[("level", P::Enum(LEVELS), false)],
        kids: Kids::Text,
        keyed: true,
    },
    Def {
        name: "Text",
        props: &[("tone", P::Enum(TONES), false), ("muted", P::Bool, false)],
        kids: Kids::Text,
        keyed: true,
    },
    Def {
        name: "Markdown",
        props: &[],
        kids: Kids::LiteralText,
        keyed: true,
    },
    Def {
        name: "Badge",
        props: &[("tone", P::Enum(TONES), false)],
        kids: Kids::Text,
        keyed: true,
    },
    Def {
        name: "Empty",
        props: &[],
        kids: Kids::Text,
        keyed: true,
    },
    Def {
        name: "Stat",
        props: &[
            ("label", P::Text, true),
            ("value", P::Any, true),
            ("format", P::Enum(FORMATS), false),
            ("tone", P::Enum(TONES), false),
            ("hint", P::Text, false),
        ],
        kids: Kids::None,
        keyed: true,
    },
    Def {
        name: "Table",
        props: &[("rows", P::Bind, true), ("empty", P::Str, false)],
        kids: Kids::Columns,
        keyed: true,
    },
    Def {
        name: "Column",
        props: &[
            ("field", P::Str, true),
            ("label", P::Str, true),
            ("format", P::Enum(FORMATS), false),
        ],
        kids: Kids::None,
        keyed: false,
    },
    Def {
        name: "List",
        props: &[
            ("items", P::Bind, true),
            ("as", P::Ident, true),
            ("empty", P::Str, false),
        ],
        kids: Kids::Blocks,
        keyed: true,
    },
    Def {
        name: "Each",
        props: &[("items", P::Bind, true), ("as", P::Ident, true)],
        kids: Kids::Blocks,
        keyed: true,
    },
    Def {
        name: "Show",
        props: &[("when", P::Cond, true)],
        kids: Kids::Blocks,
        keyed: true,
    },
    Def {
        name: "Progress",
        props: &[
            ("value", P::NumOrBind, true),
            ("max", P::Num, false),
            ("label", P::Text, false),
            ("tone", P::Enum(TONES), false),
        ],
        kids: Kids::None,
        keyed: true,
    },
    Def {
        name: "Chart",
        props: &[
            ("kind", P::Enum(CHART_KINDS), true),
            ("data", P::Bind, true),
            ("x", P::Str, true),
            ("y", P::Str, true),
            ("label", P::Str, false),
        ],
        kids: Kids::None,
        keyed: true,
    },
    Def {
        name: "AgentAnswer",
        props: &[("from", P::Bind, true), ("empty", P::Str, false)],
        kids: Kids::None,
        keyed: true,
    },
    Def {
        name: "RunList",
        props: &[("from", P::Bind, true), ("empty", P::Str, false)],
        kids: Kids::None,
        keyed: true,
    },
    Def {
        name: "MessageFeed",
        props: &[("from", P::Bind, true), ("empty", P::Str, false)],
        kids: Kids::None,
        keyed: true,
    },
    Def {
        name: "ScheduleCard",
        props: &[("from", P::Bind, true), ("empty", P::Str, false)],
        kids: Kids::None,
        keyed: true,
    },
    Def {
        name: "FileList",
        props: &[("from", P::Bind, true), ("empty", P::Str, false)],
        kids: Kids::None,
        keyed: true,
    },
    Def {
        name: "Button",
        props: &[
            ("label", P::Text, true),
            ("action", P::Enum(ACTIONS), true),
            ("tone", P::Enum(TONES), false),
            ("space", P::Str, false),
            ("room", P::Str, false),
            ("agent", P::Str, false),
            ("prompt", P::Str, false),
            ("schedule", P::Str, false),
            ("page", P::Str, false),
        ],
        kids: Kids::None,
        keyed: true,
    },
    Def {
        name: "Link",
        props: &[
            ("label", P::Text, true),
            (
                "action",
                P::Enum(&[spec::action::OPEN_ROOM, spec::action::OPEN_PAGE]),
                true,
            ),
            ("space", P::Str, false),
            ("room", P::Str, false),
            ("page", P::Str, false),
        ],
        kids: Kids::None,
        keyed: true,
    },
];

fn def(name: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.name == name)
}

/// Block names, for messages and the generation prompt.
pub fn block_names() -> Vec<&'static str> {
    DEFS.iter().map(|d| d.name).collect()
}

/// Which `<Data>` element a block's `from=` must name.
fn from_source(block: &str) -> Option<&'static str> {
    Some(match block {
        "AgentAnswer" => "agentAnswer",
        "RunList" => "runs",
        "MessageFeed" => "messages",
        "ScheduleCard" => "schedules",
        "FileList" => "library",
        _ => return None,
    })
}

/// Top-level fields of each source's value.
fn source_fields(source: &str) -> &'static [&'static str] {
    match source {
        "runs" => &["active", "items"],
        "messages" => &["items", "room"],
        "agentAnswer" => &["text", "at", "agent", "status", "room", "error"],
        "schedules" | "library" | "agents" => &["items"],
        _ => &[],
    }
}

/// Fields of each source's `items`.
fn item_fields(source: &str) -> &'static [&'static str] {
    match source {
        "runs" => &[
            "id",
            "agent",
            "agent_handle",
            "room",
            "room_id",
            "status",
            "started",
            "sender",
            "task",
        ],
        "messages" => &["id", "author", "author_id", "text", "at", "kind", "agent"],
        "schedules" => &[
            "id",
            "name",
            "enabled",
            "next_run",
            "last_status",
            "last_run",
            "room",
            "owned",
        ],
        "library" => &[
            "id",
            "name",
            "kind",
            "mime_type",
            "size",
            "uploaded",
            "space_id",
        ],
        "agents" => &[
            "id",
            "name",
            "handle",
            "description",
            "is_default",
            "color",
            "initial",
            "avatar_version",
        ],
        _ => &[],
    }
}

/// Which params each source takes, and which it requires.
fn source_params(source: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match source {
        "runs" => (&["space", "room", "agent", "limit"], &[]),
        "messages" => (&["space", "room", "agent", "limit", "kind"], &["room"]),
        "agentAnswer" => (&["space", "room", "agent", "block"], &["room"]),
        "schedules" => (&["space", "room"], &[]),
        "library" => (&["space", "query", "kind", "limit"], &[]),
        "agents" => (&["space"], &[]),
        _ => (&[], &[]),
    }
}

fn source_limit(source: &str) -> u32 {
    match source {
        "library" => 20,
        _ => 50,
    }
}

const PROP_HINTS: &[(&str, &str)] = &[
    (
        "style",
        "styling properties aren't allowed; blocks are styled by Metro's theme (use tone=…)",
    ),
    (
        "className",
        "styling properties aren't allowed; blocks are styled by Metro's theme (use tone=…)",
    ),
    (
        "class",
        "styling properties aren't allowed; blocks are styled by Metro's theme (use tone=…)",
    ),
    (
        "dangerouslySetInnerHTML",
        "raw HTML isn't allowed; use <Markdown> or <Text>",
    ),
    ("ref", "refs aren't allowed"),
    (
        "href",
        "links go through <Link action=\"openRoom\"|\"openPage\">",
    ),
    ("src", "pages can't load images or files by URL"),
    ("html", "raw HTML isn't allowed; use <Markdown> or <Text>"),
];

/// Why `md` can't be a page's `<Markdown>`, if it can't. Links are
/// checked structurally: every inline link `[text](destination "title")`
/// is parsed (balanced parentheses, `<…>` destinations, backslash escapes)
/// and its destination must be a plain `https://` URL. Images, raw HTML,
/// `<…>` autolinks, reference-style links and definitions, and bare
/// non-https schemes are refused outright, so nothing the renderer's
/// Markdown parser could turn into a link escapes the check.
pub fn markdown_problem(md: &str) -> Option<&'static str> {
    const NO_IMAGES: &str =
        "<Markdown> can't show images (![…](…)); pages never load remote content";
    const HTTPS_ONLY: &str = "links in <Markdown> must be plain https:// links";
    const NO_REFS: &str =
        "reference-style links aren't supported in <Markdown>; write [text](https://…)";
    if md.contains("![") {
        return Some(NO_IMAGES);
    }
    let lower = md.to_ascii_lowercase();
    let b = lower.as_bytes();
    for (i, c) in b.iter().enumerate() {
        // `](<destination>)` is a link destination, checked below.
        let destination = i >= 2 && &b[i - 2..i] == b"](";
        if *c == b'<'
            && !destination
            && b.get(i + 1)
                .is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, b'/' | b'!' | b'?'))
        {
            return Some("raw HTML and <…> autolinks aren't allowed in <Markdown>");
        }
    }
    // Reference definitions, wherever they are (inside block quotes, list
    // items or after lazy continuation lines; labels may span lines), and
    // full or collapsed reference links. Without definitions a shortcut
    // reference (`[x]`) can never become a link.
    if lower.contains("]:") || lower.contains("][") {
        return Some(NO_REFS);
    }
    // Inline links.
    let mut i = 0;
    while let Some(p) = lower[i..].find("](") {
        let start = i + p + 2;
        match link_end(b, start) {
            Some((dest, end)) => {
                let ok = dest.starts_with(b"https://")
                    && dest.len() > 8
                    && dest.iter().all(|c| {
                        c.is_ascii_graphic()
                            && !matches!(c, b'\\' | b'&' | b'<' | b'>' | b'"' | b'\'')
                    });
                if !ok {
                    return Some(HTTPS_ONLY);
                }
                i = end;
            }
            None => return Some(HTTPS_ONLY),
        }
    }
    // Bare schemes a renderer might autolink.
    for scheme in [
        "http://",
        "javascript:",
        "data:",
        "file:",
        "vbscript:",
        "ftp://",
        "mailto:",
        "www.",
    ] {
        if lower.contains(scheme) {
            return Some(HTTPS_ONLY);
        }
    }
    None
}

/// Parse an inline link's `destination "title")` starting at `at` (just
/// after `](`): the destination bytes and the offset after `)`, or `None`
/// when it isn't a well-formed link this checker understands.
fn link_end(b: &[u8], mut at: usize) -> Option<(&[u8], usize)> {
    while b.get(at).is_some_and(|c| *c == b' ') {
        at += 1;
    }
    let (dest, mut at) = if b.get(at) == Some(&b'<') {
        let end = at + 1 + b[at + 1..].iter().position(|c| *c == b'>')?;
        (&b[at + 1..end], end + 1)
    } else {
        let start = at;
        let mut depth = 0usize;
        while let Some(&c) = b.get(at) {
            match c {
                b'\\' => return None,
                b'(' => depth += 1,
                b')' if depth == 0 => break,
                b')' => depth -= 1,
                c if c.is_ascii_whitespace() => break,
                _ => {}
            }
            at += 1;
        }
        (&b[start..at], at)
    };
    while b.get(at).is_some_and(|c| *c == b' ') {
        at += 1;
    }
    // An optional title in "…", '…' or (…).
    if let Some(&q) = b.get(at)
        && matches!(q, b'"' | b'\'' | b'(')
    {
        let close = if q == b'(' { b')' } else { q };
        at = at + 1 + b[at + 1..].iter().position(|c| *c == close)? + 1;
        while b.get(at).is_some_and(|c| *c == b' ') {
            at += 1;
        }
    }
    (b.get(at) == Some(&b')')).then_some((dest, at + 1))
}

// ---- Compiler ----

struct Scope {
    /// Data names and `as` variables, each with its source when known.
    names: Vec<(String, Option<&'static str>, bool)>,
}

impl Scope {
    fn get(&self, name: &str) -> Option<(Option<&'static str>, bool)> {
        self.names
            .iter()
            .rev()
            .find(|(n, _, _)| n == name)
            .map(|(_, s, item)| (*s, *item))
    }
}

struct Compiler<'a> {
    src: &'a str,
    resolver: &'a mut dyn Resolver,
    diags: Vec<Diagnostic>,
    keys: HashSet<String>,
    /// Generated keys, by the element's byte offset.
    generated: HashMap<usize, String>,
    data: Vec<(String, &'static str)>,
    blocks: usize,
    actions: usize,
    /// How many times blocks here repeat with full data (1, or
    /// [`spec::MAX_ITEMS`] inside a `<List>`/`<Each>`).
    multiplier: usize,
    /// Blocks and table cells rendered with full data.
    rendered: usize,
}

impl Compiler<'_> {
    /// Add `n` rendered blocks or cells, saturating just past the budget.
    fn cost(&mut self, n: usize) {
        self.rendered = self.rendered.saturating_add(n).min(spec::MAX_RENDERED + 1);
    }
}

fn static_str(list: &'static [&'static str], s: &str) -> Option<&'static str> {
    list.iter().copied().find(|x| *x == s)
}

pub fn valid_key(k: &str) -> bool {
    !tsx::reserved(k)
        && !k.is_empty()
        && k.len() <= MAX_KEY_CHARS
        && k.starts_with(|c: char| c.is_ascii_alphanumeric())
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn valid_ident(s: &str) -> bool {
    !tsx::reserved(s)
        && !s.is_empty()
        && s.len() <= 32
        && s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A `<Column field>` or `<Chart x|y>` path: `a` or `a.b.0`.
fn valid_field(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.split('.').all(|seg| {
            !seg.is_empty()
                && !tsx::reserved(seg)
                && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

pub fn valid_schedule_id(id: &str) -> bool {
    id.len() == 36
        && id.starts_with("sch_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn valid_page_id(id: &str) -> bool {
    id.len() == 35
        && id.starts_with("pg_")
        && id[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

impl<'a> Compiler<'a> {
    fn push(&mut self, at: usize, key: Option<&str>, message: impl Into<String>) {
        if self.diags.len() < MAX_DIAGNOSTICS {
            let mut d = tsx::diag(self.src, at, message);
            d.key = key.map(str::to_owned);
            self.diags.push(d);
        }
    }

    /// First pass: existing keys, then generated ones for keyed blocks
    /// without one, in document order.
    fn collect_keys(&mut self, root: &Element) {
        fn existing(e: &Element, out: &mut Vec<(String, usize)>) {
            if let Some(Attr {
                value: AttrValue::Str(k),
                at,
                ..
            }) = e.attr("key")
            {
                out.push((k.clone(), *at));
            }
            for c in &e.children {
                if let Child::Element(c) = c {
                    existing(c, out);
                }
            }
        }
        let mut found = Vec::new();
        existing(root, &mut found);
        for (k, at) in found {
            if !self.keys.insert(k.clone()) {
                self.push(
                    at,
                    Some(&k),
                    format!("the key \"{k}\" is used twice; keys must be unique in the page"),
                );
            }
        }
        let mut counters: HashMap<String, usize> = HashMap::new();
        fn missing(e: &Element, out: &mut Vec<(String, usize)>) {
            if def(&e.name).is_some_and(|d| d.keyed) && e.attr("key").is_none() {
                out.push((e.name.clone(), e.at));
            }
            for c in &e.children {
                if let Child::Element(c) = c {
                    missing(c, out);
                }
            }
        }
        let mut need = Vec::new();
        missing(root, &mut need);
        for (name, at) in need {
            let base = kebab(&name);
            let n = counters.entry(base.clone()).or_insert(0);
            let key = loop {
                *n += 1;
                let k = format!("{base}-{n}");
                if !self.keys.contains(&k) {
                    break k;
                }
            };
            self.keys.insert(key.clone());
            self.generated.insert(at, key);
        }
    }

    fn page(&mut self, root: &Element) -> Option<PageSpec> {
        if root.name != "Page" {
            self.push(
                root.at,
                None,
                format!(
                    "the root element must be <Page title=\"…\">, not <{}>",
                    root.name
                ),
            );
            return None;
        }
        let props = self.props(root, def("Page")?, None, &Scope { names: vec![] });
        let title = match props.get("title") {
            Some(Value::Lit { v }) => v.as_str().unwrap_or_default().trim().to_owned(),
            _ => String::new(),
        };
        if props.contains_key("title")
            && (title.is_empty() || title.chars().count() > MAX_TITLE_CHARS)
        {
            self.push(
                root.at,
                None,
                format!("the page title must be 1..={MAX_TITLE_CHARS} characters"),
            );
        }
        let refresh = match props.get("refresh") {
            Some(Value::Lit { v }) => v.as_str().unwrap_or("5m").to_owned(),
            _ => "5m".to_owned(),
        };
        // Data first, wherever it is among the page's children.
        let mut data = Vec::new();
        for c in &root.children {
            if let Child::Element(e) = c
                && e.name == "Data"
                && let Some(d) = self.data(e)
            {
                data.push(d);
            }
        }
        let scope = Scope {
            names: self
                .data
                .iter()
                .map(|(n, s)| (n.clone(), Some(*s), false))
                .collect(),
        };
        let mut blocks = Vec::new();
        for c in &root.children {
            match c {
                Child::Element(e) if e.name == "Data" => {}
                Child::Element(e) => {
                    if let Some(b) = self.block(e, &scope) {
                        blocks.push(b);
                    }
                }
                Child::Text(_, at) => self.push(
                    *at,
                    None,
                    "text directly inside <Page> isn't allowed; put it in <Text> or <Heading>",
                ),
                Child::Expr(e) => self.push(
                    e.at,
                    None,
                    "values directly inside <Page> aren't allowed; put them in <Text> or <Stat>",
                ),
            }
        }
        if self.rendered > spec::MAX_RENDERED {
            self.push(
                root.at,
                None,
                format!(
                    "with full data this page would draw more than {} blocks; put fewer blocks \
                     inside <List> and <Each>",
                    spec::MAX_RENDERED
                ),
            );
        }
        if self.blocks > MAX_BLOCKS {
            self.push(
                root.at,
                None,
                format!("a page has at most {MAX_BLOCKS} blocks"),
            );
        }
        if self.actions > MAX_ACTIONS {
            self.push(
                root.at,
                None,
                format!("a page has at most {MAX_ACTIONS} buttons and links"),
            );
        }
        Some(PageSpec {
            format: spec::FORMAT,
            title,
            refresh,
            data,
            blocks,
        })
    }

    /// Check an element's properties against its definition; returns the
    /// recognised ones as values.
    fn props(
        &mut self,
        e: &Element,
        def: &Def,
        key: Option<&str>,
        scope: &Scope,
    ) -> BTreeMap<String, Value> {
        let mut out = BTreeMap::new();
        for a in &e.attrs {
            if a.name == "key" {
                match &a.value {
                    AttrValue::Str(k) if valid_key(k) => {}
                    AttrValue::Str(_) => self.push(
                        a.at,
                        key,
                        "a key is 1..=64 letters, digits, - or _, starting with a letter or digit, and not a reserved name like constructor",
                    ),
                    _ => self.push(
                        a.at,
                        key,
                        "key must be a literal string, like key=\"summary\"",
                    ),
                }
                continue;
            }
            let Some((_, ty, _)) = def.props.iter().find(|(n, _, _)| *n == a.name) else {
                let hint = if a.name.starts_with("on")
                    && a.name[2..].starts_with(|c: char| c.is_ascii_uppercase())
                {
                    "event handlers aren't allowed; use action=… on a <Button> or <Link>".to_owned()
                } else if let Some((_, h)) = PROP_HINTS.iter().find(|(n, _)| *n == a.name) {
                    (*h).to_owned()
                } else {
                    let known: Vec<&str> = def.props.iter().map(|(n, _, _)| *n).collect();
                    if known.is_empty() {
                        format!("<{}> takes no properties except key", def.name)
                    } else {
                        format!("<{}> takes only: {}", def.name, known.join(", "))
                    }
                };
                self.push(a.at, key, format!("unknown property `{}`: {hint}", a.name));
                continue;
            };
            if let Some(v) = self.prop_value(a, *ty, def, key, scope) {
                out.insert(a.name.clone(), v);
            }
        }
        for (name, _, required) in def.props {
            if *required && e.attr(name).is_none() {
                self.push(e.at, key, format!("<{}> needs {name}=…", def.name));
            }
        }
        out
    }

    fn prop_value(
        &mut self,
        a: &Attr,
        ty: P,
        def: &Def,
        key: Option<&str>,
        scope: &Scope,
    ) -> Option<Value> {
        let name = a.name.as_str();
        let lit = |v: serde_json::Value| Some(Value::Lit { v });
        let as_lit: Option<serde_json::Value> = match &a.value {
            AttrValue::Str(s) => Some(serde_json::Value::String(s.clone())),
            AttrValue::Bare => Some(serde_json::Value::Bool(true)),
            AttrValue::Expr(x) => match &x.kind {
                ExprKind::Lit(v) => Some(v.clone()),
                _ => None,
            },
        };
        let expr = match &a.value {
            AttrValue::Expr(x) if !matches!(x.kind, ExprKind::Lit(_)) => Some(x),
            _ => None,
        };
        let complex = expr.is_some_and(|x| !x.is_simple());
        if complex && ty != P::Cond {
            self.push(
                a.at,
                key,
                format!("`{name}` takes a value or a data path; !, ?? and comparisons are only allowed in <Show when={{…}}>"),
            );
            return None;
        }
        match ty {
            P::Str | P::Ident | P::Enum(_) => {
                let Some(serde_json::Value::String(s)) = as_lit else {
                    let what = match ty {
                        P::Enum(opts) => format!("one of: {}", opts.join(", ")),
                        _ => "a literal string".to_owned(),
                    };
                    // Numbers for enums like level={2} are fine.
                    if let (P::Enum(opts), Some(serde_json::Value::Number(n))) = (ty, &as_lit)
                        && opts.contains(&n.to_string().as_str())
                    {
                        return lit(serde_json::Value::String(n.to_string()));
                    }
                    self.push(a.at, key, format!("`{name}` must be {what}"));
                    return None;
                };
                match ty {
                    P::Enum(opts) if !opts.contains(&s.as_str()) => {
                        self.push(
                            a.at,
                            key,
                            format!("`{name}` must be one of: {} (got \"{s}\")", opts.join(", ")),
                        );
                        None
                    }
                    P::Ident if !valid_ident(&s) => {
                        self.push(
                            a.at,
                            key,
                            format!("`{name}` must be a simple name like \"runs\" or \"item\" (not a reserved name like constructor)"),
                        );
                        None
                    }
                    _ => {
                        if s.chars().count() > MAX_TEXT_CHARS {
                            self.push(
                                a.at,
                                key,
                                format!("`{name}` is longer than {MAX_TEXT_CHARS} characters"),
                            );
                            return None;
                        }
                        let is_field = (def.name == "Column" && name == "field")
                            || (def.name == "Chart" && (name == "x" || name == "y"));
                        if is_field && !valid_field(&s) {
                            self.push(
                                a.at,
                                key,
                                "field must be a path like \"status\" or \"agent.name\"",
                            );
                            return None;
                        }
                        lit(serde_json::Value::String(s))
                    }
                }
            }
            P::Num => match as_lit {
                Some(serde_json::Value::Number(n)) => lit(serde_json::Value::Number(n)),
                Some(serde_json::Value::String(s)) if s.trim().parse::<f64>().is_ok() => {
                    let f: f64 = s.trim().parse().unwrap_or_default();
                    lit(serde_json::json!(if f.fract() == 0.0 {
                        serde_json::json!(f as i64)
                    } else {
                        serde_json::json!(f)
                    }))
                }
                _ => {
                    self.push(
                        a.at,
                        key,
                        format!("`{name}` must be a number, like {name}={{10}}"),
                    );
                    None
                }
            },
            P::Bool => match as_lit {
                Some(serde_json::Value::Bool(b)) => lit(serde_json::Value::Bool(b)),
                _ => {
                    self.push(a.at, key, format!("`{name}` must be true or false"));
                    None
                }
            },
            P::Text | P::Any | P::NumOrBind => {
                if let Some(x) = expr {
                    return self.read(x, key, scope).then(|| x.to_value());
                }
                match (ty, as_lit) {
                    (P::Text, Some(serde_json::Value::String(s))) => {
                        if s.chars().count() > MAX_TEXT_CHARS {
                            self.push(
                                a.at,
                                key,
                                format!("`{name}` is longer than {MAX_TEXT_CHARS} characters"),
                            );
                            return None;
                        }
                        lit(serde_json::Value::String(s))
                    }
                    (P::NumOrBind, Some(serde_json::Value::Number(n))) => {
                        lit(serde_json::Value::Number(n))
                    }
                    (P::Any, Some(v)) => lit(v),
                    _ => {
                        let what = if ty == P::Text {
                            "a string or a data path"
                        } else {
                            "a number or a data path"
                        };
                        self.push(a.at, key, format!("`{name}` must be {what}"));
                        None
                    }
                }
            }
            P::Bind => {
                let Some(x) = expr else {
                    self.push(
                        a.at,
                        key,
                        format!("`{name}` must read data, like {name}={{runs.items}}"),
                    );
                    return None;
                };
                self.read(x, key, scope).then(|| x.to_value())
            }
            P::Cond => {
                let Some(x) = expr else {
                    // A literal condition is pointless but harmless.
                    return as_lit.map(|v| Value::Lit { v });
                };
                self.read(x, key, scope).then(|| x.to_value())
            }
        }
    }

    /// Check every path `x` reads against the scope; false if any fails.
    fn read(&mut self, x: &tsx::Expr, key: Option<&str>, scope: &Scope) -> bool {
        let mut paths = Vec::new();
        x.paths(&mut paths);
        let mut ok = true;
        for (path, at) in paths {
            ok &= self.check_path(&path, at, key, scope);
        }
        ok
    }

    fn check_path(&mut self, path: &[String], at: usize, key: Option<&str>, scope: &Scope) -> bool {
        let root = &path[0];
        let Some((source, item)) = scope.get(root) else {
            let names: Vec<&str> = scope.names.iter().map(|(n, _, _)| n.as_str()).collect();
            let known = if names.is_empty() {
                "none are declared; add <Data name=\"…\" source=\"…\"/> inside <Page>".to_owned()
            } else {
                format!("declared: {}", names.join(", "))
            };
            self.push(
                at,
                key,
                format!("unknown name `{root}`: a page can read only its <Data> names and <Each as> variables ({known})"),
            );
            return false;
        };
        let (Some(source), Some(field)) = (source, path.get(1)) else {
            return true;
        };
        let fields = if item {
            item_fields(source)
        } else {
            source_fields(source)
        };
        if !fields.is_empty() && !fields.contains(&field.as_str()) {
            let what = if item {
                format!("an item of `{source}`")
            } else {
                format!("`{source}` data")
            };
            self.push(
                at,
                key,
                format!(
                    "{what} has no field `{field}`; it has: {}",
                    fields.join(", ")
                ),
            );
            return false;
        }
        true
    }

    /// A `<Data>` declaration. It never disappears silently: if it can't
    /// be used, the compile fails with a diagnostic.
    fn data(&mut self, e: &Element) -> Option<DataDecl> {
        let before = self.diags.len();
        let decl = self.data_inner(e);
        if decl.is_none() && self.diags.len() == before {
            self.push(
                e.at,
                None,
                "this <Data> can't be used; check its name, source and parameters",
            );
        }
        decl
    }

    fn data_inner(&mut self, e: &Element) -> Option<DataDecl> {
        let d = def("Data")?;
        let props = self.props(e, d, None, &Scope { names: vec![] });
        if !e.children.is_empty() {
            self.push(e.at, None, "<Data> has no children; write it as <Data … />");
        }
        let s = |k: &str| match props.get(k) {
            Some(Value::Lit { v }) => v.as_str().map(str::to_owned),
            _ => None,
        };
        let (name, source) = (s("name")?, s("source")?);
        let source = static_str(SOURCES, &source)?;
        if self.data.len() >= MAX_DATA {
            self.push(
                e.at,
                None,
                format!("a page has at most {MAX_DATA} <Data> sources"),
            );
            return None;
        }
        if self.data.iter().any(|(n, _)| *n == name) {
            self.push(
                e.at,
                None,
                format!("<Data name=\"{name}\"> is declared twice"),
            );
            return None;
        }
        self.data.push((name.clone(), source));
        let (allowed, required) = source_params(source);
        for a in &e.attrs {
            let n = a.name.as_str();
            if matches!(n, "name" | "source" | "key") {
                continue;
            }
            if !allowed.contains(&n) {
                self.push(
                    a.at,
                    None,
                    format!(
                        "source=\"{source}\" doesn't take `{n}`; it takes: {}",
                        if allowed.is_empty() {
                            "nothing".to_owned()
                        } else {
                            allowed.join(", ")
                        }
                    ),
                );
            }
        }
        for r in required {
            if e.attr(r).is_none() {
                self.push(e.at, None, format!("source=\"{source}\" needs {r}=\"…\""));
            }
        }
        let mut target = self.target(
            e,
            None,
            source == "library" || source == "runs" || source == "schedules" || source == "agents",
        )?;
        if let Some(Value::Lit { v }) = props.get("limit") {
            let max = source_limit(source);
            match v.as_u64() {
                Some(n) if (1..=max as u64).contains(&n) => target.limit = Some(n as u32),
                _ => self.push(
                    e.at,
                    None,
                    format!("limit must be 1..={max} for source=\"{source}\""),
                ),
            }
        }
        if let Some(q) = s("query") {
            if q.chars().count() > 100 {
                self.push(e.at, None, "query is at most 100 characters");
            }
            target.query = Some(q);
        }
        if let Some(k) = s("kind") {
            let kinds: &[&str] = if source == "library" {
                &["document", "image", "audio", "video"]
            } else {
                &["all", "messages", "answers"]
            };
            if kinds.contains(&k.as_str()) {
                target.kind = Some(k);
            } else {
                self.push(
                    e.at,
                    None,
                    format!("kind must be one of: {}", kinds.join(", ")),
                );
            }
        }
        if let Some(b) = s("block") {
            if valid_key(&b) {
                target.block = Some(b);
            } else {
                self.push(
                    e.at,
                    None,
                    "block must be the key of a <Button> on this page",
                );
            }
        }
        if source == "agents" && target.space_id.is_none() && self.resolver.page_space().is_none() {
            self.push(
                e.at,
                None,
                "on a personal page, source=\"agents\" needs space=\"s-…\"",
            );
        }
        Some(DataDecl {
            name,
            source: source.to_owned(),
            params: target,
        })
    }

    /// `space`, `room` and `agent` of a `<Data>` or an action, resolved.
    /// `room_optional`: the element may leave out a room.
    fn target(&mut self, e: &Element, key: Option<&str>, room_optional: bool) -> Option<Target> {
        // `room="x"` and `room={"x"}` alike; other values were already
        // reported by the property check.
        let lit = |n: &str| match e.attr(n).map(|a| &a.value) {
            Some(AttrValue::Str(s)) => Some(s.trim().to_owned()),
            Some(AttrValue::Expr(x)) => match &x.kind {
                ExprKind::Lit(serde_json::Value::String(s)) => Some(s.trim().to_owned()),
                _ => None,
            },
            _ => None,
        };
        let mut t = Target::default();
        let page_space = self.resolver.page_space().map(str::to_owned);
        let space = match (&page_space, lit("space")) {
            (Some(_), Some(_)) => {
                self.push(
                    e.at,
                    key,
                    "a Space page reads only its own Space; remove space=…",
                );
                return None;
            }
            (Some(own), None) => Some(own.clone()),
            (None, Some(s)) => match self.resolver.space(&s) {
                Ok(_) => {
                    t.space_id = Some(s.clone());
                    Some(s)
                }
                Err(why) => {
                    self.push(e.at, key, why);
                    return None;
                }
            },
            (None, None) => None,
        };
        let room = lit("room");
        match (&space, room) {
            (None, Some(r)) if r.eq_ignore_ascii_case(CHAT_ROOM) => {
                t.chat = true;
                t.room = Some(CHAT_ROOM.to_owned());
            }
            (Some(sid), Some(r)) => match self.resolver.room(sid, &r) {
                Ok((id, name, thread)) => {
                    t.room_id = Some(id);
                    t.room = Some(name);
                    t.thread = thread;
                }
                Err(why) => {
                    self.push(e.at, key, why);
                    return None;
                }
            },
            (None, Some(_)) => {
                self.push(
                    e.at,
                    key,
                    "on a personal page, name the Space too (space=\"s-…\"), or use room=\"chat\" for your personal Chat",
                );
                return None;
            }
            (_, None) if !room_optional => {
                // Reported as a missing parameter elsewhere for Data.
                if e.name != "Data" {
                    self.push(e.at, key, "this action needs room=\"…\"");
                }
                return None;
            }
            (_, None) => {}
        }
        if let Some(handle) = lit("agent") {
            if t.chat {
                self.push(
                    e.at,
                    key,
                    "personal Chat has only its own agent; remove agent=…",
                );
                return None;
            }
            let Some(sid) = &space else {
                self.push(
                    e.at,
                    key,
                    "on a personal page, name the Space of the agent (space=\"s-…\")",
                );
                return None;
            };
            match self.resolver.agent(sid, handle.trim_start_matches('@')) {
                Ok((id, handle)) => {
                    t.agent_id = Some(id);
                    t.agent = Some(handle);
                }
                Err(why) => {
                    self.push(e.at, key, why);
                    return None;
                }
            }
        }
        Some(t)
    }

    fn block(&mut self, e: &Element, scope: &Scope) -> Option<Block> {
        let Some(d) = def(&e.name) else {
            let mut names = block_names();
            names.retain(|n| *n != "Page");
            self.push(
                e.at,
                None,
                format!(
                    "unknown block <{}>; Metro blocks are: {}",
                    e.name,
                    names.join(", ")
                ),
            );
            return None;
        };
        if e.name == "Page" {
            self.push(e.at, None, "<Page> must be the root, and only once");
            return None;
        }
        if e.name == "Data" {
            self.push(e.at, None, "<Data> must be a direct child of <Page>");
            return None;
        }
        if e.name == "Column" {
            self.push(e.at, None, "<Column> belongs directly inside <Table>");
            return None;
        }
        self.blocks += 1;
        let key = match e.attr("key").map(|a| &a.value) {
            Some(AttrValue::Str(k)) => Some(k.clone()),
            _ => self.generated.get(&e.at).cloned(),
        };
        let k = key.as_deref();
        let props = self.props(e, d, k, scope);
        let mut block = Block {
            kind: e.name.clone(),
            key: key.clone(),
            props,
            content: Vec::new(),
            children: Vec::new(),
            action: None,
        };
        // Grid columns and Section depth.
        if let Some(Value::Lit { v }) = block.props.get("columns")
            && !v.as_u64().is_some_and(|n| (1..=4).contains(&n))
        {
            self.push(e.at, k, "columns must be 1..=4");
        }
        if let Some(src) = from_source(&e.name)
            && let Some(Value::Bind { path }) = block.props.get("from")
        {
            let ok = path.len() == 1 && self.data.iter().any(|(n, s)| *n == path[0] && *s == src);
            if !ok {
                self.push(
                    e.at,
                    k,
                    format!("<{}> from={{…}} must name a <Data source=\"{src}\"> directly, like from={{{}}}", e.name, path[0]),
                );
            }
        }
        // Repetition: counted at its worst case, and never nested.
        self.cost(self.multiplier);
        let repeats = matches!(e.name.as_str(), "List" | "Each" | "Table");
        let nested = repeats && self.multiplier > 1;
        if nested {
            self.push(
                e.at,
                k,
                "repeating blocks can't be nested: no <List>, <Each> or <Table> inside a <List> or <Each>",
            );
        }
        // Children.
        let mut inner = None;
        if matches!(e.name.as_str(), "List" | "Each")
            && let (Some(Value::Lit { v }), Some(Value::Bind { path })) =
                (block.props.get("as"), block.props.get("items"))
        {
            let var = v.as_str().unwrap_or_default().to_owned();
            let source = scope.get(&path[0]).and_then(|(s, _)| s);
            let item_source = match (source, path.get(1).map(String::as_str)) {
                (Some(s), Some("items")) if path.len() == 2 => Some(s),
                _ => None,
            };
            if scope.get(&var).is_some() {
                self.push(
                    e.at,
                    k,
                    format!("as=\"{var}\" hides another name; pick a different one"),
                );
            }
            let mut names = scope.names.clone();
            names.push((var, item_source, true));
            inner = Some(Scope { names });
        }
        let scope = inner.as_ref().unwrap_or(scope);
        let saved = self.multiplier;
        if matches!(e.name.as_str(), "List" | "Each") {
            // A rejected nested repeater isn't multiplied again: the page
            // already fails, and its cost stays bounded.
            if !nested {
                self.multiplier = saved
                    .saturating_mul(spec::MAX_ITEMS)
                    .min(spec::MAX_RENDERED + 1);
            }
        }
        match d.kids {
            Kids::None => {
                if let Some(c) = e.children.first() {
                    let at = match c {
                        Child::Element(x) => x.at,
                        Child::Text(_, at) => *at,
                        Child::Expr(x) => x.at,
                    };
                    self.push(
                        at,
                        k,
                        format!("<{}> has no children; write it as <{} … />", e.name, e.name),
                    );
                }
            }
            Kids::Blocks => {
                for c in &e.children {
                    match c {
                        Child::Element(x) => {
                            if let Some(b) = self.block(x, scope) {
                                block.children.push(b);
                            }
                        }
                        Child::Text(_, at) => self.push(
                            *at,
                            k,
                            format!(
                                "text directly inside <{}> isn't allowed; wrap it in <Text>",
                                e.name
                            ),
                        ),
                        Child::Expr(x) => self.push(
                            x.at,
                            k,
                            format!(
                                "values directly inside <{}> aren't allowed; wrap them in <Text>",
                                e.name
                            ),
                        ),
                    }
                }
            }
            Kids::Text | Kids::LiteralText => {
                let mut len = 0;
                for c in &e.children {
                    match c {
                        Child::Text(t, _) => {
                            len += t.chars().count();
                            block.content.push(Value::Lit {
                                v: serde_json::Value::String(t.clone()),
                            });
                        }
                        Child::Expr(x) if d.kids == Kids::Text => {
                            if !x.is_simple() {
                                self.push(x.at, k, "text may read data like {runs.active}; !, ?? and comparisons are only for <Show when={…}>");
                            } else if self.read(x, k, scope) {
                                block.content.push(x.to_value());
                            }
                        }
                        Child::Expr(x) => {
                            if let ExprKind::Lit(serde_json::Value::String(s)) = &x.kind {
                                len += s.chars().count();
                                block.content.push(Value::Lit {
                                    v: serde_json::Value::String(s.clone()),
                                });
                            } else {
                                self.push(x.at, k, "<Markdown> holds literal text only; to show an agent's answer use <AgentAnswer>");
                            }
                        }
                        Child::Element(x) => self.push(
                            x.at,
                            k,
                            format!(
                                "<{}> holds text, not blocks; put <{}> next to it",
                                e.name, x.name
                            ),
                        ),
                    }
                }
                if len > MAX_TEXT_CHARS * 4 {
                    self.push(e.at, k, "this text is too long");
                }
                if d.kids == Kids::LiteralText {
                    let text: String = block
                        .content
                        .iter()
                        .filter_map(|v| match v {
                            Value::Lit { v } => v.as_str().map(str::to_owned),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    if let Some(why) = markdown_problem(&text) {
                        self.push(e.at, k, why);
                    }
                }
            }
            Kids::Columns => {
                for c in &e.children {
                    match c {
                        Child::Element(x) if x.name == "Column" => {
                            if let Some(cd) = def("Column") {
                                let props = self.props(x, cd, k, scope);
                                if !x.children.is_empty() {
                                    self.push(x.at, k, "<Column> has no children; write it as <Column field=\"…\" label=\"…\" />");
                                }
                                block.children.push(Block {
                                    kind: "Column".to_owned(),
                                    key: None,
                                    props,
                                    content: Vec::new(),
                                    children: Vec::new(),
                                    action: None,
                                });
                            }
                        }
                        Child::Element(x) => self.push(
                            x.at,
                            k,
                            format!("<Table> holds only <Column> elements, not <{}>", x.name),
                        ),
                        Child::Text(_, at) => {
                            self.push(*at, k, "<Table> holds only <Column> elements")
                        }
                        Child::Expr(x) => {
                            self.push(x.at, k, "<Table> holds only <Column> elements")
                        }
                    }
                }
                if block.children.is_empty() {
                    self.push(
                        e.at,
                        k,
                        "<Table> needs at least one <Column field=\"…\" label=\"…\" />",
                    );
                }
            }
        }
        self.multiplier = saved;
        if e.name == "Table" {
            let columns = block.children.len();
            if columns > spec::MAX_COLUMNS {
                self.push(
                    e.at,
                    k,
                    format!("a <Table> has at most {} columns", spec::MAX_COLUMNS),
                );
            }
            if !nested {
                self.cost(
                    self.multiplier
                        .saturating_mul(spec::MAX_ITEMS)
                        .saturating_mul(columns),
                );
            }
        }
        if matches!(e.name.as_str(), "Button" | "Link") {
            self.actions += 1;
            let before = self.diags.len();
            block.action = self.action(e, k, &block.props);
            if block.action.is_none() && self.diags.len() == before {
                self.push(
                    e.at,
                    k,
                    "this button's action can't be used; check action= and its parameters",
                );
            }
            for p in ["space", "room", "agent", "prompt", "schedule", "page"] {
                block.props.remove(p);
            }
        }
        Some(block)
    }

    fn action(
        &mut self,
        e: &Element,
        key: Option<&str>,
        props: &BTreeMap<String, Value>,
    ) -> Option<Action> {
        let s = |n: &str| match props.get(n) {
            Some(Value::Lit { v }) => v.as_str().map(str::to_owned),
            _ => None,
        };
        let kind = s("action")?;
        let uses: &[&str] = match kind.as_str() {
            spec::action::RUN_AGENT => &["space", "room", "agent", "prompt"],
            spec::action::RUN_SCHEDULE => &["schedule"],
            spec::action::OPEN_ROOM => &["space", "room"],
            spec::action::OPEN_PAGE => &["page"],
            _ => &[],
        };
        let mut bad = false;
        for p in ["space", "room", "agent", "prompt", "schedule", "page"] {
            if e.attr(p).is_some() && !uses.contains(&p) {
                self.push(e.at, key, format!("action=\"{kind}\" doesn't take `{p}`"));
                bad = true;
            }
        }
        if bad {
            return None;
        }
        let mut action = Action {
            kind: kind.clone(),
            target: Target::default(),
            prompt: None,
            schedule_id: None,
            page_id: None,
        };
        match kind.as_str() {
            spec::action::RUN_AGENT => {
                let Some(prompt) = s("prompt").map(|p| p.trim().to_owned()) else {
                    self.push(
                        e.at,
                        key,
                        "action=\"runAgent\" needs prompt=\"…\" (the exact text sent to the agent)",
                    );
                    return None;
                };
                if prompt.is_empty() || prompt.chars().count() > MAX_PROMPT_CHARS {
                    self.push(
                        e.at,
                        key,
                        format!("prompt must be 1..={MAX_PROMPT_CHARS} characters"),
                    );
                    return None;
                }
                let target = self.target(e, key, false)?;
                // Personal pages are strictly page-only: their buttons start
                // private runs whose output appears only on the page.
                if self.resolver.page_space().is_none() && !target.chat {
                    self.push(e.at, key, PERSONAL_ONLY);
                    return None;
                }
                if target.thread {
                    self.push(
                        e.at,
                        key,
                        "a button can't send work to a thread; name one of the Space's top-level rooms",
                    );
                    return None;
                }
                if !target.chat && target.agent_id.is_none() {
                    self.push(
                        e.at,
                        key,
                        "action=\"runAgent\" needs agent=\"handle\" (one of the Space's agents)",
                    );
                    return None;
                }
                action.target = target;
                action.prompt = Some(prompt);
            }
            spec::action::RUN_SCHEDULE if self.resolver.page_space().is_none() => {
                self.push(
                    e.at,
                    key,
                    "a personal page can't run schedules: a scheduled task writes to your Chat or a \
                     Space room, and a personal page's buttons only start private runs shown on the page",
                );
                return None;
            }
            spec::action::RUN_SCHEDULE => match s("schedule") {
                Some(id) if valid_schedule_id(&id) => action.schedule_id = Some(id),
                Some(_) => {
                    self.push(e.at, key, "schedule must be a scheduled task id (sch_…)");
                    return None;
                }
                None => {
                    self.push(e.at, key, "action=\"runSchedule\" needs schedule=\"sch_…\"");
                    return None;
                }
            },
            spec::action::OPEN_ROOM => {
                action.target = self.target(e, key, false)?;
            }
            spec::action::OPEN_PAGE => match s("page") {
                Some(id) if valid_page_id(&id) => action.page_id = Some(id),
                _ => {
                    self.push(
                        e.at,
                        key,
                        "action=\"openPage\" needs page=\"pg_…\" (a page id)",
                    );
                    return None;
                }
            },
            _ => {}
        }
        Some(action)
    }
}

/// Compile `src` as the person `resolver` resolves names for.
pub fn compile(src: &str, resolver: &mut dyn Resolver) -> Compiled {
    let root = match tsx::parse(src) {
        Ok(root) => root,
        Err(d) => {
            return Compiled {
                source: src.to_owned(),
                spec: None,
                diagnostics: vec![d],
            };
        }
    };
    let mut c = Compiler {
        src,
        resolver,
        diags: Vec::new(),
        keys: HashSet::new(),
        generated: HashMap::new(),
        data: Vec::new(),
        blocks: 0,
        actions: 0,
        multiplier: 1,
        rendered: 0,
    };
    c.collect_keys(&root);
    let spec = c.page(&root);
    // agentAnswer block= must name a Button with runAgent on this page.
    if let Some(spec) = &spec {
        for d in &spec.data {
            if let Some(b) = &d.params.block {
                let ok = spec.block(b).is_some_and(|b| {
                    b.action
                        .as_ref()
                        .is_some_and(|a| a.kind == spec::action::RUN_AGENT)
                });
                if !ok {
                    c.push(
                        root.at,
                        None,
                        format!(
                            "<Data name=\"{}\"> block=\"{b}\" must be the key of a <Button action=\"runAgent\"> on this page",
                            d.name
                        ),
                    );
                }
            }
        }
    }
    // Invariant: what is stored can always be read back. The parser's caps
    // keep specs shallow; this refuses anything that slipped past them.
    if c.diags.is_empty()
        && let Some(spec) = &spec
    {
        let back = serde_json::to_string(spec)
            .ok()
            .and_then(|j| serde_json::from_str::<PageSpec>(&j).ok());
        if back.as_ref() != Some(spec) {
            c.push(
                root.at,
                None,
                "the page is too complex to store; simplify it",
            );
        }
    }
    let diagnostics = c.diags;
    let mut source = src.to_owned();
    if diagnostics.is_empty() && !c.generated.is_empty() {
        let mut inserts: Vec<(usize, String)> = Vec::new();
        fn offsets(e: &Element, gen_: &HashMap<usize, String>, out: &mut Vec<(usize, String)>) {
            if let Some(k) = gen_.get(&e.at) {
                out.push((e.name_end, format!(" key=\"{k}\"")));
            }
            for ch in &e.children {
                if let Child::Element(x) = ch {
                    offsets(x, gen_, out);
                }
            }
        }
        offsets(&root, &c.generated, &mut inserts);
        inserts.sort_by_key(|i| std::cmp::Reverse(i.0));
        for (at, text) in inserts {
            source.insert_str(at, &text);
        }
    }
    // The stored source, generated keys included, obeys the same limit.
    let mut diagnostics = diagnostics;
    if diagnostics.is_empty() && source.len() > tsx::MAX_SOURCE_BYTES {
        diagnostics.push(Diagnostic {
            line: 1,
            column: 1,
            message: format!(
                "with its keys added the page is over {} KB; write keys yourself or shorten it",
                tsx::MAX_SOURCE_BYTES / 1024
            ),
            key: None,
        });
    }
    Compiled {
        source,
        spec: if diagnostics.is_empty() { spec } else { None },
        diagnostics,
    }
}
