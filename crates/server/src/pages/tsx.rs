//! The Page TSX parser: a strict, hand-written recursive-descent parser for
//! the small declarative subset of TSX that pages are written in.
//!
//! It accepts exactly one `export default (<Page …>…</Page>);` made of JSX
//! elements, string/number/boolean/`null` literals, data paths
//! (`{runs.items}`), and in conditions `!x`, `a ?? b` and comparisons
//! against a literal. Everything else is rejected with a message that says
//! what to write instead, because the messages are fed back to the model
//! that wrote the page. Nothing parsed here is ever executed.

use super::spec::{CmpOp, Value};

/// Largest accepted source, in bytes.
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;
/// Deepest element nesting.
pub const MAX_DEPTH: usize = 12;
/// Deepest expression nesting (`!!x`, parentheses).
const MAX_EXPR_DEPTH: usize = 8;
/// Most operands of one `a ?? b ?? …` chain.
pub const MAX_COALESCE: usize = 8;
/// Most nodes (literals, reads, operators) in one `{…}` expression. With
/// the operand and nesting caps this bounds every expression tree, so the
/// compiler, the spec and its JSON never nest deeply.
pub const MAX_EXPR_NODES: usize = 32;
/// Most elements in one page, `<Data>` and `<Column>` included.
pub const MAX_ELEMENTS: usize = 600;

/// One problem in a page's source, at a 1-based line and column.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Diagnostic {
    pub line: usize,
    pub column: usize,
    pub message: String,
    /// The block the problem is in, when it has a key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

/// Line and column (1-based, in characters) of byte offset `at`.
pub fn line_col(src: &str, at: usize) -> (usize, usize) {
    let at = at.min(src.len());
    let before = &src[..floor_boundary(src, at)];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, col)
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub fn diag(src: &str, at: usize, message: impl Into<String>) -> Diagnostic {
    let (line, column) = line_col(src, at);
    Diagnostic {
        line,
        column,
        message: message.into(),
        key: None,
    }
}

/// A parsed element.
#[derive(Clone, Debug)]
pub struct Element {
    pub name: String,
    /// Byte offset of `<`.
    pub at: usize,
    /// Byte offset just after the tag name, where a missing `key` is inserted.
    pub name_end: usize,
    /// Byte offset just after the element's end (`/>` or its close tag).
    pub end: usize,
    pub attrs: Vec<Attr>,
    pub children: Vec<Child>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.name == name)
    }
}

#[derive(Clone, Debug)]
pub struct Attr {
    pub name: String,
    pub at: usize,
    pub value: AttrValue,
    /// Byte range of a quoted string value, quotes excluded.
    pub span: Option<(usize, usize)>,
}

#[derive(Clone, Debug)]
pub enum AttrValue {
    /// `<Show negate>`: `true`.
    Bare,
    Str(String),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub enum Child {
    Element(Element),
    Text(String, usize),
    Expr(Expr),
}

/// An expression with the byte offset it starts at.
#[derive(Clone, Debug)]
pub struct Expr {
    pub at: usize,
    pub kind: ExprKind,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Lit(serde_json::Value),
    Path(Vec<String>),
    Not(Box<Expr>),
    Coalesce(Box<Expr>, Box<Expr>),
    Cmp(CmpOp, Box<Expr>, serde_json::Value),
}

impl Expr {
    /// The expression as a spec value. Paths are checked by the compiler.
    pub fn to_value(&self) -> Value {
        match &self.kind {
            ExprKind::Lit(v) => Value::Lit { v: v.clone() },
            ExprKind::Path(p) => Value::Bind { path: p.clone() },
            ExprKind::Not(a) => Value::Not {
                a: Box::new(a.to_value()),
            },
            ExprKind::Coalesce(a, b) => Value::Coalesce {
                a: Box::new(a.to_value()),
                b: Box::new(b.to_value()),
            },
            ExprKind::Cmp(op, a, b) => Value::Cmp {
                op: *op,
                a: Box::new(a.to_value()),
                b: b.clone(),
            },
        }
    }

    /// Every path the expression reads, with where it starts.
    pub fn paths(&self, out: &mut Vec<(Vec<String>, usize)>) {
        match &self.kind {
            ExprKind::Lit(_) => {}
            ExprKind::Path(p) => out.push((p.clone(), self.at)),
            ExprKind::Not(a) => a.paths(out),
            ExprKind::Coalesce(a, b) => {
                a.paths(out);
                b.paths(out);
            }
            ExprKind::Cmp(_, a, _) => a.paths(out),
        }
    }

    pub fn is_simple(&self) -> bool {
        matches!(self.kind, ExprKind::Lit(_) | ExprKind::Path(_))
    }
}

/// Path segments that could reach an object's machinery in any JavaScript
/// realm; never valid in a page, even though no page code runs.
pub const RESERVED_NAMES: &[&str] = &[
    "__proto__",
    "constructor",
    "prototype",
    "toString",
    "toLocaleString",
    "valueOf",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
];

/// Whether `s` names an object's machinery in some JavaScript realm.
pub fn reserved(s: &str) -> bool {
    RESERVED_NAMES.contains(&s)
}

const FORBIDDEN_SEGMENTS: &[&str] = RESERVED_NAMES;

/// Words that mean the model tried to write code.
const CODE_WORDS: &[(&str, &str)] = &[
    ("function", "functions aren't allowed"),
    ("return", "statements aren't allowed"),
    ("const", "variables aren't allowed"),
    ("let", "variables aren't allowed"),
    ("var", "variables aren't allowed"),
    ("new", "constructors aren't allowed"),
    (
        "typeof",
        "operators other than !, ?? and comparisons aren't allowed",
    ),
    (
        "void",
        "operators other than !, ?? and comparisons aren't allowed",
    ),
    (
        "delete",
        "operators other than !, ?? and comparisons aren't allowed",
    ),
    ("await", "async code isn't allowed"),
    ("async", "async code isn't allowed"),
    ("yield", "generators aren't allowed"),
    ("import", "imports aren't allowed"),
    ("require", "imports aren't allowed"),
    ("this", "`this` isn't allowed"),
    ("class", "classes aren't allowed"),
    ("if", "statements aren't allowed; use <Show when={…}>"),
    (
        "for",
        "loops aren't allowed; use <Each items={…} as=\"item\">",
    ),
    (
        "while",
        "loops aren't allowed; use <Each items={…} as=\"item\">",
    ),
];

const NO_CODE: &str = "a page can only declare blocks, <Data> and actions; no page code runs";

pub struct Parser<'a> {
    src: &'a str,
    b: &'a [u8],
    i: usize,
    elements: usize,
    /// Nodes of the expression being parsed.
    expr_nodes: usize,
}

type PResult<T> = Result<T, Diagnostic>;

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            b: src.as_bytes(),
            i: 0,
            elements: 0,
            expr_nodes: 0,
        }
    }

    fn err<T>(&self, at: usize, message: impl Into<String>) -> PResult<T> {
        Err(diag(self.src, at, message))
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.b.get(self.i + n).copied()
    }

    fn starts(&self, s: &str) -> bool {
        self.b[self.i..].starts_with(s.as_bytes())
    }

    /// Skip whitespace and `//` / `/* */` comments.
    fn skip_trivia(&mut self) -> PResult<()> {
        loop {
            match self.peek() {
                Some(c) if c.is_ascii_whitespace() => self.i += 1,
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.i += 1;
                    }
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let start = self.i;
                    match self.src[self.i + 2..].find("*/") {
                        Some(end) => self.i += 2 + end + 2,
                        None => return self.err(start, "unterminated /* comment"),
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn ident(&mut self) -> Option<String> {
        let start = self.i;
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() || c == b'_' || c == b'$' => self.i += 1,
            _ => return None,
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' {
                self.i += 1;
            } else {
                break;
            }
        }
        Some(self.src[start..self.i].to_owned())
    }

    fn expect_word(&mut self, word: &str, message: &str) -> PResult<()> {
        let at = self.i;
        match self.ident() {
            Some(w) if w == word => Ok(()),
            Some(w) if w == "import" => self.err(at, format!("imports aren't allowed: {NO_CODE}")),
            _ => self.err(at, message.to_owned()),
        }
    }

    fn program(&mut self) -> PResult<Element> {
        const SHAPE: &str = "a page is exactly one `export default (<Page title=\"…\">…</Page>);`";
        self.skip_trivia()?;
        self.expect_word("export", SHAPE)?;
        self.skip_trivia()?;
        self.expect_word("default", SHAPE)?;
        self.skip_trivia()?;
        let paren = self.peek() == Some(b'(');
        if paren {
            self.i += 1;
            self.skip_trivia()?;
        }
        if self.peek() != Some(b'<') {
            let at = self.i;
            return match self.ident() {
                Some(w) if w == "function" || w == "class" => {
                    self.err(at, format!("a page can't export code: {NO_CODE}. {SHAPE}"))
                }
                _ if self.peek() == Some(b'(')
                    || self.peek() == Some(b')')
                    || self.starts("=>") =>
                {
                    self.err(at, format!("functions aren't allowed: {NO_CODE}. {SHAPE}"))
                }
                _ => self.err(at, SHAPE),
            };
        }
        let root = self.element(1)?;
        self.skip_trivia()?;
        if paren {
            if self.peek() != Some(b')') {
                return self.err(self.i, "expected `)` after the page's root element");
            }
            self.i += 1;
            self.skip_trivia()?;
        }
        if self.peek() == Some(b';') {
            self.i += 1;
        }
        self.skip_trivia()?;
        if self.i < self.b.len() {
            let at = self.i;
            return match self.ident() {
                Some(w) if w == "import" || w == "export" || w == "function" || w == "const" => {
                    self.err(at, format!("only one default export is allowed: {NO_CODE}"))
                }
                _ => self.err(at, format!("unexpected text after the page: {SHAPE}")),
            };
        }
        Ok(root)
    }

    /// An element at `<`, at nesting `depth`.
    fn element(&mut self, depth: usize) -> PResult<Element> {
        let at = self.i;
        if depth > MAX_DEPTH {
            return self.err(at, format!("blocks are nested more than {MAX_DEPTH} deep"));
        }
        self.elements += 1;
        if self.elements > MAX_ELEMENTS {
            return self.err(at, format!("a page has at most {MAX_ELEMENTS} elements"));
        }
        self.i += 1; // `<`
        if self.peek() == Some(b'>') {
            return self.err(
                at,
                "fragments (<>…</>) aren't allowed; group blocks in <Section> or <Row>",
            );
        }
        let Some(name) = self.ident() else {
            return self.err(self.i, "expected a block name after `<`");
        };
        let name_end = self.i;
        match self.peek() {
            Some(b'.') => {
                return self.err(
                    at,
                    "member tags like <a.b> aren't allowed; use a Metro block",
                );
            }
            Some(b':') | Some(b'-') => {
                return self.err(
                    at,
                    "namespaced or custom tags aren't allowed; use a Metro block",
                );
            }
            _ => {}
        }
        if name.starts_with(|c: char| c.is_ascii_lowercase()) {
            return self.err(
                at,
                format!(
                    "HTML elements like <{name}> aren't allowed; use Metro blocks such as \
                     <Section>, <Text> or <Button>"
                ),
            );
        }
        let mut attrs: Vec<Attr> = Vec::new();
        loop {
            self.skip_trivia()?;
            match self.peek() {
                Some(b'/') if self.peek_at(1) == Some(b'>') => {
                    self.i += 2;
                    return Ok(Element {
                        name,
                        at,
                        name_end,
                        end: self.i,
                        attrs,
                        children: Vec::new(),
                    });
                }
                Some(b'>') => {
                    self.i += 1;
                    break;
                }
                Some(b'{') => {
                    return self.err(
                        self.i,
                        "spread attributes ({...x}) aren't allowed; write each property",
                    );
                }
                None => return self.err(at, format!("<{name}> is never closed")),
                _ => {}
            }
            let attr = self.attr()?;
            if attrs.iter().any(|a| a.name == attr.name) {
                return self.err(attr.at, format!("`{}` is set twice", attr.name));
            }
            attrs.push(attr);
        }
        let children = self.children(&name, depth)?;
        Ok(Element {
            name,
            at,
            name_end,
            end: self.i,
            attrs,
            children,
        })
    }

    fn attr(&mut self) -> PResult<Attr> {
        let at = self.i;
        let Some(name) = self.ident() else {
            return self.err(at, "expected a property name");
        };
        if matches!(self.peek(), Some(b':') | Some(b'-')) {
            return self.err(
                at,
                "namespaced or dashed properties (like aria-*, data-* or xlink:*) aren't allowed",
            );
        }
        self.skip_trivia()?;
        if self.peek() != Some(b'=') {
            return Ok(Attr {
                name,
                at,
                value: AttrValue::Bare,
                span: None,
            });
        }
        self.i += 1;
        self.skip_trivia()?;
        match self.peek() {
            Some(q @ (b'"' | b'\'')) => {
                let start = self.i + 1;
                let Some(len) = self.src[start..].find(q as char) else {
                    return self.err(self.i, "unterminated string");
                };
                let raw = &self.src[start..start + len];
                self.i = start + len + 1;
                let value = decode_entities(raw);
                self.check_chars(start, &value)?;
                Ok(Attr {
                    name,
                    at,
                    value: AttrValue::Str(value),
                    span: Some((start, start + len)),
                })
            }
            Some(b'{') => {
                let open = self.i;
                self.i += 1;
                self.skip_trivia()?;
                if self.peek() == Some(b'}') {
                    return self.err(open, "empty `{}`: give a value");
                }
                let expr = self.top_expr()?;
                self.skip_trivia()?;
                self.close_brace(open)?;
                Ok(Attr {
                    name,
                    at,
                    value: AttrValue::Expr(expr),
                    span: None,
                })
            }
            Some(b'<') => self.err(
                self.i,
                "elements as property values aren't allowed; put blocks inside as children",
            ),
            _ => self.err(self.i, "expected a \"string\" or {value} after `=`"),
        }
    }

    fn close_brace(&mut self, open: usize) -> PResult<()> {
        match self.peek() {
            Some(b'}') => {
                self.i += 1;
                Ok(())
            }
            None => self.err(open, "`{` is never closed"),
            _ => self.unsupported(),
        }
    }

    fn children(&mut self, name: &str, depth: usize) -> PResult<Vec<Child>> {
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None => return self.err(self.i, format!("<{name}> is never closed")),
                Some(b'<') if self.peek_at(1) == Some(b'/') => {
                    let at = self.i;
                    self.i += 2;
                    self.skip_trivia()?;
                    let close = self.ident().unwrap_or_default();
                    self.skip_trivia()?;
                    if close != name {
                        return self.err(at, format!("</{close}> doesn't match the open <{name}>"));
                    }
                    if self.peek() != Some(b'>') {
                        return self.err(self.i, "expected `>`");
                    }
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'<') => out.push(Child::Element(self.element(depth + 1)?)),
                Some(b'{') => {
                    let open = self.i;
                    self.i += 1;
                    self.skip_trivia()?;
                    // `{/* comment */}`
                    if self.peek() == Some(b'}') {
                        self.i += 1;
                        continue;
                    }
                    if self.peek() == Some(b'<') {
                        return self.err(
                            self.i,
                            "elements inside {…} aren't allowed; to show blocks conditionally \
                             use <Show when={…}>, to repeat them use <Each items={…} as=\"item\">",
                        );
                    }
                    let expr = self.top_expr()?;
                    self.skip_trivia()?;
                    if self.starts("&&") || self.starts("||") || self.peek() == Some(b'?') {
                        return self.err(
                            self.i,
                            "conditional rendering with &&, || or ?: isn't allowed; use \
                             <Show when={…}>",
                        );
                    }
                    self.close_brace(open)?;
                    out.push(Child::Expr(expr));
                }
                Some(_) => {
                    let start = self.i;
                    while let Some(c) = self.peek() {
                        if c == b'<' || c == b'{' {
                            break;
                        }
                        if c == b'}' || c == b'>' {
                            return self.err(
                                self.i,
                                format!(
                                    "`{}` in text must be written as {{\"{}\"}}",
                                    c as char, c as char
                                ),
                            );
                        }
                        self.i += 1;
                    }
                    let text = jsx_text(&self.src[start..self.i]);
                    if !text.is_empty() {
                        let text = decode_entities(&text);
                        self.check_chars(start, &text)?;
                        out.push(Child::Text(text, start));
                    }
                }
            }
        }
    }

    /// Expression grammar, lowest precedence first:
    /// `coalesce := cmp ("??" cmp)*`, `cmp := unary (op literal)?`,
    /// `unary := "!" unary | primary`.
    /// One `{…}` expression, with its own node budget.
    fn top_expr(&mut self) -> PResult<Expr> {
        self.expr_nodes = 0;
        self.expr(0)
    }

    /// Count one expression node; fails past [`MAX_EXPR_NODES`].
    fn node(&mut self, at: usize) -> PResult<()> {
        self.expr_nodes += 1;
        if self.expr_nodes > MAX_EXPR_NODES {
            return self.err(
                at,
                format!("the expression is too long (at most {MAX_EXPR_NODES} parts)"),
            );
        }
        Ok(())
    }

    fn expr(&mut self, depth: usize) -> PResult<Expr> {
        if depth > MAX_EXPR_DEPTH {
            return self.err(self.i, "the expression is nested too deeply");
        }
        let mut left = self.cmp(depth)?;
        let mut operands = 1;
        loop {
            self.skip_trivia()?;
            if self.starts("??") {
                let at = left.at;
                operands += 1;
                if operands > MAX_COALESCE {
                    return self.err(
                        self.i,
                        format!("a ?? chain has at most {MAX_COALESCE} values"),
                    );
                }
                self.node(at)?;
                self.i += 2;
                self.skip_trivia()?;
                let right = self.cmp(depth + 1)?;
                left = Expr {
                    at,
                    kind: ExprKind::Coalesce(Box::new(left), Box::new(right)),
                };
            } else {
                return Ok(left);
            }
        }
    }

    fn cmp(&mut self, depth: usize) -> PResult<Expr> {
        let left = self.unary(depth)?;
        self.skip_trivia()?;
        let op = if self.starts("===") {
            Some((CmpOp::Eq, 3))
        } else if self.starts("!==") {
            Some((CmpOp::Ne, 3))
        } else if self.starts("==") || self.starts("!=") {
            return self.err(self.i, "use === or !== to compare");
        } else if self.starts("<=") {
            Some((CmpOp::Le, 2))
        } else if self.starts(">=") {
            Some((CmpOp::Ge, 2))
        } else if self.peek() == Some(b'<') {
            Some((CmpOp::Lt, 1))
        } else if self.peek() == Some(b'>') {
            Some((CmpOp::Gt, 1))
        } else {
            None
        };
        let Some((op, len)) = op else {
            return Ok(left);
        };
        self.node(left.at)?;
        self.i += len;
        self.skip_trivia()?;
        let at = self.i;
        let right = self.unary(depth + 1)?;
        let ExprKind::Lit(lit) = right.kind else {
            return self.err(
                at,
                "compare against a literal value, like {runs.active > 0}",
            );
        };
        Ok(Expr {
            at: left.at,
            kind: ExprKind::Cmp(op, Box::new(left), lit),
        })
    }

    fn unary(&mut self, depth: usize) -> PResult<Expr> {
        if depth > MAX_EXPR_DEPTH {
            return self.err(self.i, "the expression is nested too deeply");
        }
        self.skip_trivia()?;
        let at = self.i;
        if self.peek() == Some(b'!') && self.peek_at(1) != Some(b'=') {
            self.node(at)?;
            self.i += 1;
            let inner = self.unary(depth + 1)?;
            return Ok(Expr {
                at,
                kind: ExprKind::Not(Box::new(inner)),
            });
        }
        self.primary(depth)
    }

    fn primary(&mut self, depth: usize) -> PResult<Expr> {
        let at = self.i;
        if self.peek() != Some(b'(') {
            self.node(at)?;
        }
        match self.peek() {
            Some(b'(') => {
                self.i += 1;
                self.skip_trivia()?;
                if self.peek() == Some(b')') {
                    return self.err(at, format!("functions aren't allowed: {NO_CODE}"));
                }
                let inner = self.expr(depth + 1)?;
                self.skip_trivia()?;
                if self.starts("=>") || self.peek() == Some(b',') {
                    return self.err(at, format!("functions aren't allowed: {NO_CODE}"));
                }
                if self.peek() != Some(b')') {
                    return self.unsupported();
                }
                self.i += 1;
                self.after_value()?;
                Ok(inner)
            }
            Some(q @ (b'"' | b'\'')) => {
                let s = self.js_string(q)?;
                self.check_chars(at, &s)?;
                self.after_value()?;
                Ok(Expr {
                    at,
                    kind: ExprKind::Lit(serde_json::Value::String(s)),
                })
            }
            Some(b'`') => {
                self.i += 1;
                let start = self.i;
                let Some(len) = self.src[start..].find('`') else {
                    return self.err(at, "unterminated template string");
                };
                let raw = &self.src[start..start + len];
                if raw.contains("${") {
                    return self.err(
                        at,
                        "template strings with ${…} aren't allowed; use a plain string or a \
                         separate {path}",
                    );
                }
                if raw.contains('\\') {
                    return self.err(at, "escapes in template strings aren't allowed");
                }
                self.check_chars(at, raw)?;
                self.i = start + len + 1;
                self.after_value()?;
                Ok(Expr {
                    at,
                    kind: ExprKind::Lit(serde_json::Value::String(raw.to_owned())),
                })
            }
            Some(b'.') if self.starts("...") => self.err(at, "spread (...) isn't allowed"),
            Some(c) if c.is_ascii_digit() || c == b'-' || c == b'.' => {
                let n = self.number()?;
                self.after_value()?;
                Ok(Expr {
                    at,
                    kind: ExprKind::Lit(n),
                })
            }
            Some(b'[') => self.err(at, "array literals aren't allowed"),
            Some(b'{') => self.err(at, "object literals aren't allowed"),
            Some(b'<') => self.err(
                at,
                "elements inside {…} aren't allowed; use <Show when={…}> or <Each>",
            ),
            Some(_) => {
                let Some(word) = self.ident() else {
                    return self.unsupported();
                };
                let lit = match word.as_str() {
                    "true" => Some(serde_json::Value::Bool(true)),
                    "false" => Some(serde_json::Value::Bool(false)),
                    "null" => Some(serde_json::Value::Null),
                    "undefined" => {
                        return self.err(at, "`undefined` isn't allowed; use null");
                    }
                    _ => None,
                };
                if let Some(lit) = lit {
                    self.after_value()?;
                    return Ok(Expr {
                        at,
                        kind: ExprKind::Lit(lit),
                    });
                }
                if let Some((_, why)) = CODE_WORDS.iter().find(|(w, _)| *w == word) {
                    return self.err(at, format!("{why}: {NO_CODE}"));
                }
                self.path(at, word)
            }
            None => self.err(at, "unexpected end of the page"),
        }
    }

    /// `first(.seg)*`, with `?.` read as `.` (reads are null-safe anyway).
    fn path(&mut self, at: usize, first: String) -> PResult<Expr> {
        let mut segs = vec![first];
        loop {
            self.skip_trivia()?;
            let dot = if self.starts("?.") {
                if self.peek_at(2) == Some(b'[') || self.peek_at(2) == Some(b'(') {
                    return self.unsupported();
                }
                2
            } else if self.peek() == Some(b'.') && self.peek_at(1) != Some(b'.') {
                1
            } else {
                break;
            };
            self.i += dot;
            self.skip_trivia()?;
            let seg_at = self.i;
            let Some(seg) = self.ident().or_else(|| self.index()) else {
                return self.err(seg_at, "expected a field name after `.`");
            };
            segs.push(seg);
        }
        for s in &segs {
            if FORBIDDEN_SEGMENTS.contains(&s.as_str()) {
                return self.err(at, format!("`{s}` can't be read in a page"));
            }
        }
        match self.peek() {
            Some(b'(') => {
                return self.err(
                    at,
                    format!("calls like {}(…) aren't allowed: {NO_CODE}", segs.join(".")),
                );
            }
            Some(b'[') => {
                return self.err(
                    self.i,
                    "computed access like x[…] isn't allowed; use x.field (or x.0 for the first \
                     item)",
                );
            }
            _ => {}
        }
        if self.starts("=>") {
            return self.err(at, format!("functions aren't allowed: {NO_CODE}"));
        }
        self.after_value()?;
        Ok(Expr {
            at,
            kind: ExprKind::Path(segs),
        })
    }

    /// A numeric path segment (`items.0`).
    fn index(&mut self) -> Option<String> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.i += 1;
            } else {
                break;
            }
        }
        (self.i > start && self.i - start <= 4).then(|| self.src[start..self.i].to_owned())
    }

    /// After a complete value: reject the operators the language lacks.
    fn after_value(&mut self) -> PResult<()> {
        let save = self.i;
        self.skip_trivia()?;
        let at = self.i;
        let bad = |s: &Self| -> Option<&'static str> {
            if s.starts("=>") {
                Some("functions aren't allowed")
            } else if s.starts("===") || s.starts("!==") || s.starts("??") {
                None
            } else if s.starts("==") || s.starts("!=") {
                Some("use === or !== to compare")
            } else if s.starts("&&") || s.starts("||") {
                Some(
                    "&& and || aren't allowed; use ?? for a default, or <Show when={…}> to show blocks conditionally",
                )
            } else {
                match s.peek() {
                    Some(b'=') => Some("assignments aren't allowed"),
                    Some(b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^') => Some(
                        "arithmetic and other operators aren't allowed; compute values in an agent",
                    ),
                    Some(b'?') => Some("?: isn't allowed; use <Show when={…}>"),
                    Some(b',') => Some("comma expressions aren't allowed"),
                    Some(b';') => Some("statements aren't allowed"),
                    Some(b'(') => Some("calls aren't allowed"),
                    Some(b'[') => Some("computed access like x[…] isn't allowed"),
                    Some(b'`') => Some("tagged templates aren't allowed"),
                    _ => None,
                }
            }
        };
        if let Some(why) = bad(self) {
            return self.err(at, format!("{why}: {NO_CODE}"));
        }
        self.i = save;
        Ok(())
    }

    /// Refuse control characters (other than line breaks and tabs) and
    /// invisible direction overrides in a string's decoded value, however
    /// they were written (raw, `\u…` escape or `&#…;` entity).
    fn check_chars(&self, at: usize, s: &str) -> PResult<()> {
        match s.chars().find(|c| forbidden_char(*c)) {
            Some(c) => self.err(
                at,
                format!(
                    "the character U+{:04X} isn't allowed in a page (control or text-direction character)",
                    c as u32
                ),
            ),
            None => Ok(()),
        }
    }

    fn unsupported<T>(&self) -> PResult<T> {
        self.err(
            self.i,
            format!(
                "unsupported syntax; inside {{…}} a page may use only literals, data paths like \
                 {{runs.items}}, and in when= also !, ?? and comparisons: {NO_CODE}"
            ),
        )
    }

    fn js_string(&mut self, q: u8) -> PResult<String> {
        let at = self.i;
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.src[self.i..].chars().next() else {
                return self.err(at, "unterminated string");
            };
            self.i += c.len_utf8();
            match c {
                c if c as u32 == q as u32 => return Ok(out),
                '\n' => return self.err(at, "unterminated string"),
                '\\' => {
                    let Some(e) = self.src[self.i..].chars().next() else {
                        return self.err(at, "unterminated string");
                    };
                    self.i += e.len_utf8();
                    match e {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        '\\' | '"' | '\'' | '`' => out.push(e),
                        'u' => {
                            let hex = self.src.get(self.i..self.i + 4).unwrap_or("");
                            let ch = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
                            match ch {
                                Some(ch) if hex.len() == 4 => {
                                    out.push(ch);
                                    self.i += 4;
                                }
                                _ => return self.err(self.i, "bad \\u escape; use \\uXXXX"),
                            }
                        }
                        _ => return self.err(self.i, format!("unsupported escape \\{e}")),
                    }
                }
                c => out.push(c),
            }
        }
    }

    fn number(&mut self) -> PResult<serde_json::Value> {
        let at = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return self.err(at, format!("operators aren't allowed: {NO_CODE}"));
            }
        }
        while let Some(c) = self.peek() {
            let exponent_sign =
                (c == b'+' || c == b'-') && matches!(self.b.get(self.i - 1), Some(b'e' | b'E'));
            if c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'_') || exponent_sign {
                self.i += 1;
            } else {
                break;
            }
        }
        let text = self.src[at..self.i].replace('_', "");
        if self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            return self.err(
                at,
                "unsupported number; write plain decimal numbers like 10 or 0.5",
            );
        }
        match text.parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(if f.fract() == 0.0 && f.abs() < 9e15 {
                serde_json::Value::from(f as i64)
            } else {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }),
            _ => self.err(
                at,
                "unsupported number; write plain decimal numbers like 10 or 0.5",
            ),
        }
    }
}

/// C0/C1 controls except `\n` and `\t`, bidi embeddings, overrides and
/// isolates (U+202A–U+202E, U+2066–U+2069) and the LRM/RLM marks.
pub fn forbidden_char(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || ('\u{202A}'..='\u{202E}').contains(&c)
        || ('\u{2066}'..='\u{2069}').contains(&c)
        || c == '\u{200E}'
        || c == '\u{200F}'
}

/// JSX whitespace rules: lines are trimmed, whitespace-only lines that
/// contain a line break are dropped, and the rest are joined with spaces.
fn jsx_text(raw: &str) -> String {
    if !raw.contains('\n') {
        return raw.to_owned();
    }
    let lines: Vec<&str> = raw.split('\n').collect();
    let last = lines.len() - 1;
    let mut parts = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let mut l = *line;
        if n != 0 {
            l = l.trim_start();
        }
        if n != last {
            l = l.trim_end();
        }
        if !l.is_empty() {
            parts.push(l);
        }
    }
    parts.join(" ")
}

/// The few HTML entities JSX text and attribute strings commonly use.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let end = rest.find(';').filter(|e| *e <= 10);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ => name
                    .strip_prefix('#')
                    .and_then(|n| match n.strip_prefix(['x', 'X']) {
                        Some(h) => u32::from_str_radix(h, 16).ok(),
                        None => n.parse().ok(),
                    })
                    .and_then(char::from_u32)
                    .filter(|c| !c.is_control() || *c == '\n'),
            }?;
            Some((ch, e))
        });
        match decoded {
            Some((ch, e)) => {
                out.push(ch);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse `src` into its root element, or the first syntax error.
pub fn parse(src: &str) -> Result<Element, Diagnostic> {
    if src.len() > MAX_SOURCE_BYTES {
        return Err(Diagnostic {
            line: 1,
            column: 1,
            message: format!("a page's source is at most {} KB", MAX_SOURCE_BYTES / 1024),
            key: None,
        });
    }
    if let Some(p) = src.find('\0') {
        return Err(diag(src, p, "the source contains a NUL character"));
    }
    Parser::new(src).program()
}

/// The source text of the element with `key`, if any.
pub fn element_source<'s>(src: &'s str, key: &str) -> Option<&'s str> {
    fn find(e: &Element, key: &str) -> Option<(usize, usize)> {
        if let Some(Attr {
            value: AttrValue::Str(k),
            ..
        }) = e.attr("key")
            && k == key
        {
            return Some((e.at, e.end));
        }
        e.children.iter().find_map(|c| match c {
            Child::Element(x) => find(x, key),
            _ => None,
        })
    }
    let root = parse(src).ok()?;
    let (a, b) = find(&root, key)?;
    src.get(a..b)
}
