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
//!
//! This module never reads membership itself: the caller passes the member
//! ids of the room's Space it has already authorised for this sender.

use serde::{Deserialize, Serialize};

/// Most distinct recipients one message may address.
pub const MAX_RECIPIENTS: usize = 32;
/// Longest member id the text parser will match as an `@handle`.
const MAX_HANDLE_LEN: usize = 64;
/// Reserved text handle for the room agent (matched case-insensitively).
const AGENT_HANDLE: &str = "agent";

/// One addressed recipient.
///
/// Wire format: `{"kind":"agent"}` or `{"kind":"user","id":"<member id>"}`.
/// Unknown kinds and extra fields are rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", try_from = "WireMention")]
pub enum Mention {
    /// The room's agent.
    Agent,
    /// A human member, by stable identity id (`sub` or dev username), never
    /// by display name.
    User { id: String },
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
        match (w.kind, w.id) {
            (WireKind::Agent, None) => Ok(Mention::Agent),
            (WireKind::Agent, Some(_)) => Err("agent mention takes no id"),
            (WireKind::User, Some(id)) => Ok(Mention::User { id }),
            (WireKind::User, None) => Err("user mention requires id"),
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
        push(m.clone(), &mut out)?;
    }
    if invoke_agent {
        push(Mention::Agent, &mut out)?;
    }

    let handles: Vec<&str> = member_ids
        .iter()
        .map(String::as_str)
        .filter(|id| is_handle(id) && !id.eq_ignore_ascii_case(AGENT_HANDLE))
        .collect();
    for m in scan_text(text, &handles) {
        push(m, &mut out)?;
    }
    Ok(out)
}

/// `"room"` (no recipients), `"agent"`, `"users"` or `"mixed"`.
pub fn audience(mentions: &[Mention]) -> &'static str {
    let agent = mentions.iter().any(|m| matches!(m, Mention::Agent));
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
    mentions.iter().any(|m| matches!(m, Mention::Agent))
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
fn code_ranges(text: &str) -> Vec<(usize, usize)> {
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

fn scan_text(text: &str, handles: &[&str]) -> Vec<Mention> {
    let code = code_ranges(text);
    let in_code = |i: usize| code.iter().any(|&(s, e)| i >= s && i < e);
    let mut found = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        let p = prev;
        prev = Some(c);
        if c != '@' || in_code(i) || !opens_token(p) {
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

        let m = match (user, agent_match) {
            // A longer member id (e.g. `agent.smith`) beats the bare handle.
            (Some(h), _) if h.len() > AGENT_HANDLE.len() || !agent_match => Mention::User {
                id: (*h).to_owned(),
            },
            (_, true) => Mention::Agent,
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
            r#"{"id":"bob"}"#,
            r#"{"kind":"user","id":5}"#,
            r#"{"kind":"user","id":null}"#,
            r#"{"kind":"agent","kind":"user","id":"x"}"#,
        ] {
            assert!(serde_json::from_str::<Mention>(bad).is_err(), "{bad}");
        }
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
    fn multibyte_text_is_safe() {
        let r = text_only("¡Hola @alice! 日本語 @agent。", TEAM);
        assert_eq!(r, vec![user("alice")]);
        assert_eq!(text_only("café @bob — ok", TEAM), vec![user("bob")]);
    }
}
