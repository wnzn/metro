//! Structured message addressing: who a room message is for.
//!
//! A message can be addressed to the room's agent, to specific human
//! members, to both, or to nobody in particular (the whole room). Recipients
//! come from three places, merged by [`resolve`]:
//!
//! 1. Structured mentions sent by the client ([`Mention`] JSON). These are
//!    authoritative and work for any member id, including opaque OIDC
//!    subjects such as `auth0|5f7c…` that nobody would type.
//! 2. The `invoke_agent` flag, which adds [`Mention::Agent`].
//! 3. `@agent` / `@<member-id>` written in the text, recognised only for
//!    *current members* and only as standalone handles (see [`resolve`]).
//!    Text in a validated explicit mention span is not scanned. Clients may
//!    also opt out of text scanning entirely.
//!
//! This module never reads membership itself: the caller passes the member
//! ids of the room's Space it has already authorised for this sender.

use serde::{Deserialize, Serialize};

/// Most distinct recipients one message may address.
pub const MAX_RECIPIENTS: usize = 32;
/// Repeated references to one recipient each take a span.
pub const MAX_SPANS: usize = 64;
/// Longest member id the text parser will match as an `@handle`.
const MAX_HANDLE_LEN: usize = 64;
/// Reserved text handle for the room agent (matched case-insensitively).
const AGENT_HANDLE: &str = "agent";

/// A half-open UTF-8 byte range over message text (not JSON or UTF-16).
/// In a POST, `index` addresses the submitted `mentions` array and the range
/// addresses the decoded `text` before trimming. In an event, both address
/// the corresponding returned fields: deduplicated `mentions` and trimmed
/// `text`. Events list spans in text order. A label is not an identity.
///
/// A span starts at a standalone `@` and ends at a token boundary (as for
/// legacy handles). The nonempty label may contain spaces between words,
/// but no control characters, line breaks or edge whitespace. An `@` is
/// allowed within the label (e.g. `@Support@Home`), not at either edge of
/// the label or after a character that would let it start a separate token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct MentionSpan {
    pub start: usize,
    pub end: usize,
    pub index: usize,
}

impl<'de> Deserialize<'de> for MentionSpan {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor, value::MapAccessDeserializer};

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            start: usize,
            end: usize,
            index: usize,
        }

        struct SpanVisitor;
        impl<'de> Visitor<'de> for SpanVisitor {
            type Value = MentionSpan;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a mention span object with start, end and index")
            }

            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<MentionSpan, M::Error> {
                let Fields { start, end, index } =
                    Fields::deserialize(MapAccessDeserializer::new(map))?;
                Ok(MentionSpan { start, end, index })
            }
        }

        d.deserialize_map(SpanVisitor)
    }
}

/// Sorted, non-overlapping ranges normalized to the stored (`trim()`ed)
/// text. Until resolution, indices still refer to the submitted list.
#[derive(Default)]
pub(crate) struct ValidatedSpans(Vec<MentionSpan>);

impl ValidatedSpans {
    pub(crate) fn new(
        raw_text: &str,
        explicit_count: usize,
        mut spans: Vec<MentionSpan>,
    ) -> Result<Self, &'static str> {
        const INVALID: &str = "invalid mention span";
        if spans.len() > MAX_SPANS {
            return Err("too many mention spans");
        }
        if spans.is_empty() {
            return Ok(Self(spans));
        }
        let trimmed = raw_text.trim();
        let offset = raw_text.len() - raw_text.trim_start().len();
        let trimmed_end = offset + trimmed.len();
        spans.sort_unstable_by_key(|s| s.start);
        let mut previous_end = offset;
        for span in &mut spans {
            if span.start < previous_end || span.end > trimmed_end || span.index >= explicit_count {
                return Err(INVALID);
            }
            let label = raw_text
                .get(span.start..span.end)
                .and_then(|s| s.strip_prefix('@'))
                .ok_or(INVALID)?;
            if !valid_span_label(label)
                || !opens_token(raw_text[..span.start].chars().next_back())
                || !closes_token(&raw_text[span.end..])
            {
                return Err(INVALID);
            }
            previous_end = span.end;
            span.start -= offset;
            span.end -= offset;
        }
        Ok(Self(spans))
    }
}

/// One addressed recipient.
///
/// Wire format: `{"kind":"agent"}` or `{"kind":"user","id":"<member id>"}`.
/// Unknown kinds and extra fields are rejected.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "WireMention")]
pub enum Mention {
    /// The Space's generic agent (`@agent`): its default agent, or the one
    /// the sender's opt-in classifier picks.
    Agent,
    /// One agent of the Space, by id (`{"kind":"agent","agent_id":"ag-…"}`),
    /// or by its handle in text (`@coder`).
    AgentId { agent_id: String },
    /// A human member, by stable identity id (`sub` or dev username), never
    /// by display name.
    User { id: String },
}

impl Serialize for Mention {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(None)?;
        match self {
            Mention::Agent => m.serialize_entry("kind", "agent")?,
            Mention::AgentId { agent_id } => {
                m.serialize_entry("kind", "agent")?;
                m.serialize_entry("agent_id", agent_id)?;
            }
            Mention::User { id } => {
                m.serialize_entry("kind", "user")?;
                m.serialize_entry("id", id)?;
            }
        }
        m.end()
    }
}

impl Mention {
    /// The generic agent or one named agent.
    pub fn is_agent(&self) -> bool {
        matches!(self, Mention::Agent | Mention::AgentId { .. })
    }
}

/// Strict wire shape. serde's `deny_unknown_fields` does not reject extra
/// fields on a unit variant of an internally tagged enum, so deserialization
/// goes through this struct instead.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireMention {
    kind: WireKind,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireKind {
    Agent,
    User,
}

impl TryFrom<WireMention> for Mention {
    type Error = &'static str;
    fn try_from(w: WireMention) -> Result<Self, Self::Error> {
        match (w.kind, w.id, w.agent_id) {
            (WireKind::Agent, None, None) => Ok(Mention::Agent),
            (WireKind::Agent, None, Some(agent_id)) => Ok(Mention::AgentId { agent_id }),
            (WireKind::Agent, Some(_), _) => Err("agent mention takes agent_id, not id"),
            (WireKind::User, Some(id), None) => Ok(Mention::User { id }),
            (WireKind::User, _, Some(_)) => Err("user mention takes no agent_id"),
            (WireKind::User, None, None) => Err("user mention requires id"),
        }
    }
}

/// Validation failures. Messages are deliberately generic: they never echo
/// the submitted id or reveal whether someone exists outside the Space.
const ERR_UNKNOWN: &str = "unknown recipient";
const ERR_TOO_MANY: &str = "too many recipients";

/// Merge explicit mentions, the `invoke_agent` flag and `@handles` in `text`
/// into a deduplicated recipient list (first-seen order: explicit, then the
/// flag, then text).
///
/// * `member_ids`: stable ids of the current members of the Space, as
///   already authorised by the caller.
/// * An explicit [`Mention::User`] whose id is not in `member_ids` fails with
///   a generic error. Text handles that match nobody are ordinary text.
/// * Text matching: `@` must start a token (start of text, whitespace or
///   opening punctuation before it; never `\@`, never `user@domain`), and the
///   handle must end at a token boundary (end, whitespace, or trailing
///   punctuation such as `,.;:!?)]}'"` followed by end/whitespace). Code
///   spans (`` `…` ``) and fenced code blocks (```` ``` ```` / `~~~`) are
///   skipped. `@agent` (any ASCII case) always means the agent, even if a
///   member's id is `agent`; such a member can still be addressed with a
///   structured mention. Otherwise the longest exactly matching member id
///   wins, so `@bob.smith` picks `bob.smith` over `bob`, and `@bob.smith`
///   never matches `bob` alone. Only ids made of `[A-Za-z0-9._-]`
///   (≤ 64 bytes, starting alphanumeric) are matched in text.
/// * More than [`MAX_RECIPIENTS`] explicit entries or distinct recipients is
///   an error.
pub fn resolve(
    text: &str,
    explicit: &[Mention],
    invoke_agent: bool,
    member_ids: &[String],
) -> Result<Vec<Mention>, String> {
    resolve_with_agents(text, explicit, invoke_agent, None, member_ids, &[])
}

/// [`resolve`] in a Space with agents but without typed spans.
pub fn resolve_with_agents(
    text: &str,
    explicit: &[Mention],
    invoke_agent: bool,
    invoke_agent_id: Option<&str>,
    member_ids: &[String],
    agents: &[(String, String)],
) -> Result<Vec<Mention>, String> {
    let (resolved, _) = resolve_with_spans(
        Some(text),
        explicit,
        invoke_agent,
        invoke_agent_id,
        member_ids,
        agents,
        ValidatedSpans::default(),
    )?;
    Ok(resolved)
}

/// [`resolve`] in a Space with agents: `agents` are `(id, handle)` of the
/// Space's agents. An explicit [`Mention::AgentId`] must name one of them;
/// `invoke_agent` with `invoke_agent_id` adds that agent instead of the
/// generic one. In text, `@handle` of an agent (any ASCII case) wins over a
/// member id of the same length; the handle `agent` always stays the
/// generic `@agent` outside explicit spans. Pass `None` for `infer_from` to
/// skip text scanning, or `Some` with the trimmed text against which the
/// `spans` were validated. Returned spans refer to the canonical
/// (deduplicated) list in either case.
pub(crate) fn resolve_with_spans(
    infer_from: Option<&str>,
    explicit: &[Mention],
    invoke_agent: bool,
    invoke_agent_id: Option<&str>,
    member_ids: &[String],
    agents: &[(String, String)],
    ValidatedSpans(mut spans): ValidatedSpans,
) -> Result<(Vec<Mention>, Vec<MentionSpan>), String> {
    if explicit.len() > MAX_RECIPIENTS {
        return Err(ERR_TOO_MANY.to_owned());
    }
    let mut out: Vec<Mention> = Vec::new();
    let push = |m: Mention, out: &mut Vec<Mention>| -> Result<(), String> {
        if !out.contains(&m) {
            if out.len() >= MAX_RECIPIENTS {
                return Err(ERR_TOO_MANY.to_owned());
            }
            out.push(m);
        }
        Ok(())
    };

    for m in explicit {
        if let Mention::User { id } = m
            && !member_ids.iter().any(|member| member == id)
        {
            return Err(ERR_UNKNOWN.to_owned());
        }
        if let Mention::AgentId { agent_id } = m
            && !agents.iter().any(|(id, _)| id == agent_id)
        {
            return Err(ERR_UNKNOWN.to_owned());
        }
        push(m.clone(), &mut out)?;
    }
    match (invoke_agent, invoke_agent_id) {
        (true, None) => push(Mention::Agent, &mut out)?,
        (true, Some(agent_id)) => {
            if !agents.iter().any(|(id, _)| id == agent_id) {
                return Err(ERR_UNKNOWN.to_owned());
            }
            push(
                Mention::AgentId {
                    agent_id: agent_id.to_owned(),
                },
                &mut out,
            )?;
        }
        (false, Some(_)) => return Err("agent_id needs invoke_agent".to_owned()),
        (false, None) => {}
    }

    if let Some(text) = infer_from {
        let handles: Vec<&str> = member_ids
            .iter()
            .map(String::as_str)
            .filter(|id| is_handle(id) && !id.eq_ignore_ascii_case(AGENT_HANDLE))
            .collect();
        let agent_handles: Vec<(&str, &str)> = agents
            .iter()
            .filter(|(_, h)| is_handle(h) && !h.eq_ignore_ascii_case(AGENT_HANDLE))
            .map(|(id, h)| (id.as_str(), h.as_str()))
            .collect();
        for m in scan_text(text, &handles, &agent_handles, &spans) {
            push(m, &mut out)?;
        }
    }
    for span in &mut spans {
        // Explicit recipients are validated and inserted first; the
        // invoke flag and text only append, never replace them.
        span.index = out
            .iter()
            .position(|m| m == &explicit[span.index])
            .expect("an explicit recipient stays in the canonical list");
    }
    Ok((out, spans))
}

/// `"room"` (no recipients), `"agent"`, `"users"` or `"mixed"`.
pub fn audience(mentions: &[Mention]) -> &'static str {
    let agent = mentions.iter().any(Mention::is_agent);
    let users = mentions.iter().any(|m| matches!(m, Mention::User { .. }));
    match (agent, users) {
        (false, false) => "room",
        (true, false) => "agent",
        (false, true) => "users",
        (true, true) => "mixed",
    }
}

/// True when the agent is among the recipients.
pub fn invokes_agent(mentions: &[Mention]) -> bool {
    mentions.iter().any(Mention::is_agent)
}

fn is_handle(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_HANDLE_LEN
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Characters allowed right before `@` for it to start a mention.
fn opens_token(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(c) => {
            c.is_whitespace()
                || matches!(
                    c,
                    '(' | '[' | '{' | '<' | '"' | '\'' | ',' | ';' | ':' | '!' | '?' | '*'
                )
        }
    }
}

fn valid_span_label(label: &str) -> bool {
    if label.is_empty() || label.trim() != label || label.ends_with('@') {
        return false;
    }
    let mut prev = None;
    for c in label.chars() {
        if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') || (c == '@' && opens_token(prev))
        {
            return false;
        }
        prev = Some(c);
    }
    true
}

/// True if `rest` (the text right after a matched handle) ends the token.
fn closes_token(rest: &str) -> bool {
    let trimmed = rest.trim_start_matches(|c: char| {
        matches!(
            c,
            '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '>' | '"' | '\'' | '*'
        )
    });
    trimmed.chars().next().is_none_or(char::is_whitespace)
}

/// Byte ranges of `text` that are code (fenced blocks and inline spans).
fn code_ranges(text: &str, spans: &[MentionSpan]) -> Vec<(usize, usize)> {
    // Typed labels are opaque to Markdown. Replace only their bytes in
    // this private view, preserving offsets, line breaks and every marker
    // outside the spans. Non-whitespace also keeps a label after a fence
    // marker from looking like an empty closing line.
    let masked = (!spans.is_empty()).then(|| {
        let mut bytes = text.as_bytes().to_vec();
        for span in spans {
            bytes[span.start..span.end].fill(b'x');
        }
        String::from_utf8(bytes).expect("validated spans align with UTF-8 boundaries")
    });
    let text = masked.as_deref().unwrap_or(text);
    let mut ranges = Vec::new();
    let bytes = text.as_bytes();

    // Fenced blocks, line by line.
    let mut fence: Option<(u8, usize, usize)> = None; // (char, len, start)
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent > 3 {
            continue;
        }
        let body = &line[indent..];
        let ch = match body.as_bytes().first() {
            Some(&c @ (b'`' | b'~')) => c,
            _ => continue,
        };
        let run = body.bytes().take_while(|&b| b == ch).count();
        if run < 3 {
            continue;
        }
        match fence {
            None => fence = Some((ch, run, start)),
            Some((fc, flen, fstart))
                if fc == ch && run >= flen && body[run..].trim().is_empty() =>
            {
                ranges.push((fstart, pos));
                fence = None;
            }
            Some(_) => {}
        }
    }
    if let Some((_, _, fstart)) = fence {
        // Unclosed fence runs to the end, as in CommonMark.
        ranges.push((fstart, text.len()));
    }

    // Inline code spans outside fenced blocks: a run of N backticks closed by
    // the next run of exactly N backticks.
    let in_fence = |i: usize, r: &[(usize, usize)]| r.iter().any(|&(s, e)| i >= s && i < e);
    let fenced = ranges.clone();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' || in_fence(i, &fenced) {
            i += 1;
            continue;
        }
        let open = i;
        let n = bytes[i..].iter().take_while(|&&b| b == b'`').count();
        let mut j = open + n;
        let mut closed = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let m = bytes[j..].iter().take_while(|&&b| b == b'`').count();
                if m == n {
                    closed = Some(j + m);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match closed {
            Some(end) => {
                ranges.push((open, end));
                i = end;
            }
            None => i = open + n,
        }
    }
    ranges
}

fn scan_text(
    text: &str,
    handles: &[&str],
    agents: &[(&str, &str)],
    spans: &[MentionSpan],
) -> Vec<Mention> {
    let code = code_ranges(text, spans);
    let in_code = |i: usize| code.iter().any(|&(s, e)| i >= s && i < e);
    let mut spans = spans.iter().peekable();
    let mut found = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        let p = prev;
        prev = Some(c);
        if c != '@' {
            continue;
        }
        while spans.peek().is_some_and(|s| s.end <= i) {
            spans.next();
        }
        if spans.peek().is_some_and(|s| s.start <= i) || in_code(i) || !opens_token(p) {
            continue;
        }
        let rest = &text[i + 1..];

        // Reserved agent handle first: exact `@agent` (any ASCII case).
        let agent_match = rest.len() >= AGENT_HANDLE.len()
            && rest.as_bytes()[..AGENT_HANDLE.len()].eq_ignore_ascii_case(AGENT_HANDLE.as_bytes())
            && closes_token(&rest[AGENT_HANDLE.len()..]);

        // Longest member id that matches exactly and ends the token.
        let user = handles
            .iter()
            .filter(|h| rest.starts_with(**h) && closes_token(&rest[h.len()..]))
            .max_by_key(|h| h.len());

        // Longest agent handle that matches (ASCII case-insensitively).
        let named = agents
            .iter()
            .filter(|(_, h)| {
                rest.len() >= h.len()
                    && rest.as_bytes()[..h.len()].eq_ignore_ascii_case(h.as_bytes())
                    && closes_token(&rest[h.len()..])
            })
            .max_by_key(|(_, h)| h.len());

        let m = match (user, named, agent_match) {
            // An agent handle at least as long as any member id wins.
            (u, Some((id, h)), _) if u.is_none_or(|u| h.len() >= u.len()) => Mention::AgentId {
                agent_id: (*id).to_owned(),
            },
            // A longer member id (e.g. `agent.smith`) beats the bare handle.
            (Some(h), _, _) if h.len() > AGENT_HANDLE.len() || !agent_match => Mention::User {
                id: (*h).to_owned(),
            },
            (_, _, true) => Mention::Agent,
            _ => continue,
        };
        if !found.contains(&m) {
            found.push(m);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }
    fn user(id: &str) -> Mention {
        Mention::User { id: id.to_owned() }
    }
    fn text_only(text: &str, members: &[&str]) -> Vec<Mention> {
        resolve(text, &[], false, &ids(members)).unwrap()
    }

    const TEAM: &[&str] = &["alice", "bob", "bob.smith", "carol-ops", "agent.smith"];

    #[test]
    fn wire_format() {
        let a: Mention = serde_json::from_str(r#"{"kind":"agent"}"#).unwrap();
        assert_eq!(a, Mention::Agent);
        let u: Mention = serde_json::from_str(r#"{"kind":"user","id":"auth0|5f7c"}"#).unwrap();
        assert_eq!(u, user("auth0|5f7c"));
        assert_eq!(
            serde_json::to_string(&Mention::Agent).unwrap(),
            r#"{"kind":"agent"}"#
        );
        assert_eq!(
            serde_json::to_string(&user("bob")).unwrap(),
            r#"{"kind":"user","id":"bob"}"#
        );
        for bad in [
            r#"{"kind":"robot"}"#,
            r#"{"kind":"Agent"}"#,
            r#"{"kind":"user"}"#,
            r#"{"kind":"user","id":"bob","admin":true}"#,
            r#"{"kind":"agent","id":"bob"}"#,
            r#"{"kind":"user","id":"bob","agent_id":"ag-1"}"#,
            r#"{"id":"bob"}"#,
            r#"{"kind":"user","id":5}"#,
            r#"{"kind":"user","id":null}"#,
            r#"{"kind":"agent","kind":"user","id":"x"}"#,
        ] {
            assert!(serde_json::from_str::<Mention>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn span_wire_accepts_only_the_required_unsigned_object_fields() {
        let valid = r#"{"start":0,"end":9,"index":1}"#;
        let span = serde_json::from_str::<MentionSpan>(valid).unwrap();
        assert_eq!(serde_json::to_string(&span).unwrap(), valid);
        for bad in [
            r#"[0,9,1]"#,
            r#"{"start":0,"end":9}"#,
            r#"{"start":0,"end":9,"index":null}"#,
            r#"{"start":0,"end":"9","index":1}"#,
            r#"{"start":0,"end":9.0,"index":1}"#,
            r#"{"start":-1,"end":9,"index":1}"#,
            r#"{"start":0,"end":9,"index":1,"kind":"user"}"#,
            r#"{"start":0,"end":9,"index":1,"index":2}"#,
        ] {
            assert!(serde_json::from_str::<MentionSpan>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn spans_reject_invalid_ranges_and_non_tokens() {
        let span = |start, end| MentionSpan {
            start,
            end,
            index: 0,
        };
        for (text, spans) in [
            ("é @名", vec![span(1, 4)]), // UTF-8 start
            ("é @名", vec![span(3, 5)]), // UTF-8 end
            ("@x", vec![span(0, 0)]),    // empty
            ("@x", vec![span(2, 1)]),    // reversed
            ("@x", vec![span(0, 3)]),    // out of bounds
            (" @x", vec![span(0, 3)]),   // includes trimmed prefix
            ("@x ", vec![span(0, 3)]),   // includes trimmed suffix
            ("@x @y", vec![span(0, 4), span(3, 5)]),
            ("@x", vec![span(0, 2), span(0, 2)]),
            ("x @", vec![span(0, 1)]),    // no @ prefix
            ("x @", vec![span(2, 3)]),    // bare @
            ("@ x", vec![span(0, 3)]),    // space immediately after @
            ("@x\nY", vec![span(0, 4)]),  // across lines
            ("a@x", vec![span(1, 3)]),    // email-like
            (r"\@x", vec![span(1, 3)]),   // escaped
            ("@agent", vec![span(0, 3)]), // partial token
        ] {
            assert!(
                ValidatedSpans::new(text, 1, spans.clone()).is_err(),
                "{text:?}: {spans:?}"
            );
        }
        assert!(ValidatedSpans::new("@x", 0, vec![span(0, 2)]).is_err());
        let many = |n| (0..n).map(|i| span(i * 3, i * 3 + 2)).collect();
        assert!(ValidatedSpans::new(&"@x ".repeat(MAX_SPANS), 1, many(MAX_SPANS)).is_ok());
        assert!(ValidatedSpans::new(&"@x ".repeat(MAX_SPANS + 1), 1, many(MAX_SPANS + 1)).is_err());
    }

    #[test]
    fn mention_span_label_may_contain_at_signs_but_not_another_token() {
        let full = |text: &str| {
            ValidatedSpans::new(
                text,
                1,
                vec![MentionSpan {
                    start: 0,
                    end: text.len(),
                    index: 0,
                }],
            )
        };
        for label in ["@Support@Home", "@支援@自宅", "@Ops@Home@Work"] {
            assert!(full(label).is_ok(), "{label}");
        }
        for separate in [
            "@@agent",
            "@Support@",
            "@Support @agent",
            "@Support\u{00a0}@alice",
            "@Support (@agent)",
            "@Support:@alice",
        ] {
            assert!(full(separate).is_err(), "{separate}");
        }
    }

    #[test]
    fn mention_span_label_is_opaque_to_code_ranges_but_real_code_still_counts() {
        for label in ["@DevOps", "@Dev`Ops", "@Dév`Ops"] {
            let text = format!("👋 {label} @agent and `echo @alice literally` @carol-ops");
            let start = "👋 ".len();
            let span = MentionSpan {
                start,
                end: start + label.len(),
                index: 0,
            };
            let spans = ValidatedSpans::new(&text, 1, vec![span]).unwrap();
            let (recipients, spans) = resolve_with_spans(
                Some(&text),
                &[user("bob")],
                false,
                None,
                &ids(TEAM),
                &[],
                spans,
            )
            .unwrap();
            assert_eq!(
                recipients,
                [user("bob"), Mention::Agent, user("carol-ops")],
                "{text}"
            );
            assert_eq!(spans, [span]);
        }

        // A span does not count as whitespace after a real fence marker:
        // the second line is not a closer, and @alice stays inside code.
        let prefix = "```\n``` ";
        let label = "@Dev```Ops";
        let text = format!("{prefix}{label}\n@alice\n```\n@agent");
        let span = MentionSpan {
            start: prefix.len(),
            end: prefix.len() + label.len(),
            index: 0,
        };
        let spans = ValidatedSpans::new(&text, 1, vec![span]).unwrap();
        let (recipients, _) = resolve_with_spans(
            Some(&text),
            &[user("bob")],
            false,
            None,
            &ids(TEAM),
            &[],
            spans,
        )
        .unwrap();
        assert_eq!(recipients, [user("bob"), Mention::Agent]);

        // The same backticks still have their legacy meaning when not
        // covered by a typed span.
        assert_eq!(
            text_only(
                "👋 @Dev`Ops @agent and `echo @alice literally` @carol-ops",
                TEAM,
            ),
            [user("alice"), user("carol-ops")]
        );
        assert_eq!(
            text_only(
                "👋 @DevOps @agent and `echo @alice literally` @carol-ops",
                TEAM,
            ),
            [Mention::Agent, user("carol-ops")]
        );
    }

    #[test]
    fn audience_and_invokes_agent() {
        assert_eq!(audience(&[]), "room");
        assert_eq!(audience(&[Mention::Agent]), "agent");
        assert_eq!(audience(&[user("a"), user("b")]), "users");
        assert_eq!(audience(&[user("a"), Mention::Agent]), "mixed");
        assert!(invokes_agent(&[user("a"), Mention::Agent]));
        assert!(!invokes_agent(&[user("a")]));
        // Mentioning people never starts agent work.
        let r = resolve("@alice @bob over to you", &[], false, &ids(TEAM)).unwrap();
        assert!(!invokes_agent(&r));
        assert!(!invokes_agent(&[]));
    }

    #[test]
    fn plain_message_is_for_the_room() {
        let r = resolve("lunch at noon?", &[], false, &ids(TEAM)).unwrap();
        assert!(r.is_empty());
        assert_eq!(audience(&r), "room");
    }

    #[test]
    fn two_recipients_from_text() {
        let r = text_only("@alice @bob can you review the PR?", TEAM);
        assert_eq!(r, vec![user("alice"), user("bob")]);
        assert_eq!(audience(&r), "users");
    }

    #[test]
    fn agent_and_human_mixed() {
        let r = text_only("@agent summarise this for @carol-ops, please.", TEAM);
        assert_eq!(r, vec![Mention::Agent, user("carol-ops")]);
        assert_eq!(audience(&r), "mixed");
        let r = text_only("Hey @Agent!", TEAM);
        assert_eq!(r, vec![Mention::Agent]);
    }

    #[test]
    fn longest_exact_member_wins() {
        assert_eq!(text_only("ping @bob.smith", TEAM), vec![user("bob.smith")]);
        assert_eq!(text_only("ping @bob.", TEAM), vec![user("bob")]);
        assert_eq!(text_only("ping @bob. thanks", TEAM), vec![user("bob")]);
        assert_eq!(
            text_only("@agent.smith hi", TEAM),
            vec![user("agent.smith")]
        );
        assert_eq!(text_only("@agent. hi", TEAM), vec![Mention::Agent]);
        // With only `bob` a member, `@bob.smith` is a partial match: no mention.
        assert!(text_only("ping @bob.smith", &["bob"]).is_empty());
        assert!(text_only("@bobby @alice2 @ali", TEAM).is_empty());
        assert!(text_only("@agents and @agentx", TEAM).is_empty());
    }

    #[test]
    fn punctuation_around_handles() {
        let r = text_only("(@alice) [@bob], \"@carol-ops\": done; @agent?", TEAM);
        assert_eq!(
            r,
            vec![
                user("alice"),
                user("bob"),
                user("carol-ops"),
                Mention::Agent
            ]
        );
        assert!(text_only("@alice's notes", TEAM).is_empty());
        assert!(text_only("@alice/@bob", TEAM).is_empty());
        assert!(text_only("x=@alice", TEAM).is_empty());
    }

    #[test]
    fn emails_and_escapes_do_not_trigger() {
        assert!(text_only("mail alice@example.com or bob@agent", TEAM).is_empty());
        assert!(text_only("write to @alice@example.com", TEAM).is_empty());
        assert!(text_only(r"literal \@agent and \@bob", TEAM).is_empty());
        assert!(text_only("@ alice", TEAM).is_empty());
        assert!(text_only("@", TEAM).is_empty());
    }

    #[test]
    fn code_is_ignored() {
        assert!(text_only("run `git log --author=@bob` then `@agent`", TEAM).is_empty());
        assert!(text_only("``@agent ` still code``", TEAM).is_empty());
        let fenced = "see:\n```sh\nssh @alice\n@agent do it\n```\n@bob ok?";
        assert_eq!(text_only(fenced, TEAM), vec![user("bob")]);
        let tilde = "~~~\n@agent\n~~~\n";
        assert!(text_only(tilde, TEAM).is_empty());
        let unclosed = "```\n@agent\n@bob";
        assert!(text_only(unclosed, TEAM).is_empty());
        // An unmatched backtick is literal; the mention still counts.
        assert_eq!(text_only("it's ` odd @agent", TEAM), vec![Mention::Agent]);
    }

    #[test]
    fn reserved_agent_handle_vs_member_named_agent() {
        let members = ids(&["agent", "alice"]);
        let r = resolve("@agent help", &[], false, &members).unwrap();
        assert_eq!(r, vec![Mention::Agent]);
        // The human whose id is `agent` is reachable only structurally.
        let r = resolve("fyi", &[user("agent")], false, &members).unwrap();
        assert_eq!(r, vec![user("agent")]);
        assert_eq!(audience(&r), "users");
    }

    #[test]
    fn explicit_opaque_ids_and_dedup() {
        let oidc = ids(&["auth0|5f7c8ec7c33c6c004bbafe82", "248289761001", "alice"]);
        let explicit = [
            user("auth0|5f7c8ec7c33c6c004bbafe82"),
            user("248289761001"),
            user("248289761001"),
        ];
        let r = resolve("@alice and @agent, see above", &explicit, true, &oidc).unwrap();
        assert_eq!(
            r,
            vec![
                user("auth0|5f7c8ec7c33c6c004bbafe82"),
                user("248289761001"),
                Mention::Agent,
                user("alice"),
            ]
        );
        assert_eq!(audience(&r), "mixed");
        // Opaque ids are not matched from text.
        assert!(
            resolve("@auth0|5f7c8ec7c33c6c004bbafe82", &[], false, &oidc)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn invoke_agent_flag_adds_agent_once() {
        let r = resolve("@agent go", &[Mention::Agent], true, &ids(TEAM)).unwrap();
        assert_eq!(r, vec![Mention::Agent]);
        let r = resolve("no handles", &[], true, &ids(TEAM)).unwrap();
        assert_eq!(r, vec![Mention::Agent]);
        assert_eq!(audience(&r), "agent");
    }

    #[test]
    fn unknown_text_handle_is_plain_text() {
        let r = resolve("@mallory @alice", &[], false, &ids(TEAM)).unwrap();
        assert_eq!(r, vec![user("alice")]);
    }

    #[test]
    fn spoofed_or_nonmember_explicit_is_rejected_generically() {
        let members = ids(TEAM);
        for bad in ["mallory", "", "Alice", "alice ", "bob\u{202E}"] {
            let err = resolve("hi", &[user(bad)], false, &members).unwrap_err();
            assert_eq!(err, "unknown recipient");
            if !bad.is_empty() {
                assert!(!err.contains(bad.trim()), "error must not echo the id");
            }
        }
        // One bad entry fails the whole send, even alongside valid ones.
        assert!(resolve("hi", &[user("alice"), user("eve")], true, &members).is_err());
    }

    #[test]
    fn bounds() {
        let many: Vec<String> = (0..40).map(|i| format!("u{i}")).collect();
        let explicit: Vec<Mention> = many.iter().map(|id| user(id)).collect();
        assert_eq!(
            resolve("", &explicit, false, &many).unwrap_err(),
            "too many recipients"
        );
        let text: String = many.iter().map(|id| format!("@{id} ")).collect();
        assert_eq!(
            resolve(&text, &[], false, &many).unwrap_err(),
            "too many recipients"
        );
        let ok: String = many[..MAX_RECIPIENTS]
            .iter()
            .map(|id| format!("@{id} "))
            .collect();
        assert_eq!(
            resolve(&ok, &[], false, &many).unwrap().len(),
            MAX_RECIPIENTS
        );
        // Duplicates don't count against the cap.
        let dup = vec![user("u0"); MAX_RECIPIENTS];
        assert_eq!(resolve("", &dup, false, &many).unwrap(), vec![user("u0")]);
        // Over-long ids are never matched from text.
        let long = "a".repeat(MAX_HANDLE_LEN + 1);
        assert!(
            resolve(&format!("@{long}"), &[], false, std::slice::from_ref(&long))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn authoritative_recipients_still_count_ask_agent_toward_the_cap() {
        let members: Vec<String> = (0..MAX_RECIPIENTS).map(|i| format!("u{i}")).collect();
        let explicit: Vec<Mention> = members.iter().map(|id| user(id)).collect();
        let agents = [("ag-1".to_owned(), "coder".to_owned())];
        let resolve = |mentions: &[Mention], invoke, agent_id| {
            resolve_with_spans(
                None,
                mentions,
                invoke,
                agent_id,
                &members,
                &agents,
                ValidatedSpans::default(),
            )
            .map(|(recipients, _)| recipients)
        };
        assert_eq!(resolve(&explicit, false, None).unwrap(), explicit);
        for (agent_id, ask) in [
            (None, Mention::Agent),
            (
                Some("ag-1"),
                Mention::AgentId {
                    agent_id: "ag-1".to_owned(),
                },
            ),
        ] {
            assert_eq!(resolve(&explicit, true, agent_id), Err(ERR_TOO_MANY.into()));
            let mut at_cap = explicit[..MAX_RECIPIENTS - 1].to_vec();
            at_cap.push(ask.clone());
            assert_eq!(
                resolve(&at_cap[..MAX_RECIPIENTS - 1], true, agent_id).unwrap(),
                at_cap
            );
            assert_eq!(resolve(&at_cap, true, agent_id).unwrap(), at_cap);
            at_cap.push(ask);
            assert_eq!(resolve(&at_cap, false, None), Err(ERR_TOO_MANY.into()));
        }
    }

    #[test]
    fn named_agents() {
        let agents = vec![
            ("ag-1".to_owned(), "coder".to_owned()),
            ("ag-0".to_owned(), "agent".to_owned()),
        ];
        let members = ids(&["alice", "coder2"]);
        let coder = Mention::AgentId {
            agent_id: "ag-1".into(),
        };
        let r = resolve_with_agents(
            "@Coder and @agent, @coder2",
            &[],
            false,
            None,
            &members,
            &agents,
        )
        .unwrap();
        assert_eq!(r, vec![coder.clone(), Mention::Agent, user("coder2")]);
        assert_eq!(audience(&r), "mixed");
        assert_eq!(
            serde_json::to_string(&coder).unwrap(),
            r#"{"kind":"agent","agent_id":"ag-1"}"#
        );
        let wire: Mention = serde_json::from_str(r#"{"kind":"agent","agent_id":"ag-1"}"#).unwrap();
        assert_eq!(wire, coder);
        // Unknown agent ids are refused generically.
        let bad = Mention::AgentId {
            agent_id: "ag-9".into(),
        };
        assert!(resolve_with_agents("hi", &[bad], false, None, &members, &agents).is_err());
        assert!(resolve_with_agents("hi", &[], true, Some("ag-9"), &members, &agents).is_err());
        assert!(resolve_with_agents("hi", &[], false, Some("ag-1"), &members, &agents).is_err());
        let r = resolve_with_agents("hi", &[], true, Some("ag-1"), &members, &agents).unwrap();
        assert_eq!(r, vec![coder]);
        // A member whose id equals a handle loses the text handle.
        let r = resolve_with_agents("@coder", &[], false, None, &ids(&["coder"]), &agents).unwrap();
        assert!(matches!(r[0], Mention::AgentId { .. }));
    }

    #[test]
    fn multibyte_text_is_safe() {
        let r = text_only("¡Hola @alice! 日本語 @agent。", TEAM);
        assert_eq!(r, vec![user("alice")]);
        assert_eq!(text_only("café @bob — ok", TEAM), vec![user("bob")]);
    }
}
