use std::collections::HashMap;

use unicode_segmentation::UnicodeSegmentation;

use crate::api::{CatalogRow, Choice};

const MAX_DRAFT: usize = 64 * 1024;

#[derive(Default)]
pub struct Draft {
    pub text: String,
    pub caret: usize,
    pub ask_on_enter: bool,
    pub notice: Option<String>,
}

impl Draft {
    pub fn insert(&mut self, text: &str, known_chat_only: bool) {
        let started = !self.text.trim().is_empty();
        let room = MAX_DRAFT - self.text.len();
        let chunk = &text[..text.floor_char_boundary(room.min(text.len()))];
        self.text.insert_str(self.caret, chunk);
        let end = self.caret + chunk.len();
        self.caret = self
            .text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .find(|&i| i >= end)
            .unwrap_or(self.text.len());
        if !started && !self.text.trim().is_empty() {
            self.ask_on_enter = known_chat_only;
        }
    }

    fn previous(&self) -> usize {
        self.text[..self.caret]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.caret
            + self.text[self.caret..]
                .graphemes(true)
                .next()
                .map_or(0, str::len)
    }

    pub fn left(&mut self) {
        self.caret = self.previous();
    }
    pub fn right(&mut self) {
        self.caret = self.next();
    }
    pub fn backspace(&mut self) {
        let start = self.previous();
        self.text.drain(start..self.caret);
        self.caret = start;
    }
    pub fn delete(&mut self) {
        self.text.drain(self.caret..self.next());
    }
}

#[derive(Default, Debug)]
pub struct Address {
    pub people: bool,
    pub agent: bool,
}

// Mirror the server's text-token and Markdown rules. The server remains the
// authority: we use these flags only for the Enter affordance and to decide
// whether an explicit model choice belongs on a Send.
pub fn address(text: &str, members: Option<&[String]>) -> Address {
    const TRAILING: &[char] = &[
        '.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '"', '\'', '*',
    ];
    let closes = |s: &str| {
        s.trim_start_matches(TRAILING)
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
    };
    let mut result = Address::default();
    if !text.contains('@') {
        return result;
    }
    let mut fences = Vec::new();
    let mut fence = None;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let body = line.trim_start_matches(' ');
        if line.len() - body.len() > 3 {
            continue;
        }
        let Some(b @ (b'`' | b'~')) = body.bytes().next() else {
            continue;
        };
        let len = body.bytes().take_while(|&c| c == b).count();
        if len < 3 {
            continue;
        }
        match fence {
            None => fence = Some((b, len, start)),
            Some((open, n, from)) if open == b && len >= n && body[len..].trim().is_empty() => {
                fences.push((from, pos));
                fence = None;
            }
            _ => {}
        }
    }
    if let Some((_, _, from)) = fence {
        fences.push((from, text.len()));
    }

    let mut ticks = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let n = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            ticks.push((i, n));
            i += n;
        } else {
            i += 1;
        }
    }
    let mut following = HashMap::new();
    let mut end_at = vec![None; ticks.len()];
    for (i, &(_, n)) in ticks.iter().enumerate().rev() {
        end_at[i] = following.insert(n, i);
    }
    let mut spans = Vec::new();
    let (mut i, mut f) = (0, 0);
    while i < ticks.len() {
        let (start, _) = ticks[i];
        while f < fences.len() && fences[f].1 <= start {
            f += 1;
        }
        if fences.get(f).is_none_or(|r| start < r.0)
            && let Some(j) = end_at[i]
        {
            spans.push((start, ticks[j].0 + ticks[j].1));
            i = j;
        }
        i += 1;
    }

    let (mut f, mut s) = (0, 0);
    let mut prev = None;
    for (i, c) in text.char_indices() {
        let before = prev.replace(c);
        if c != '@'
            || !before.is_none_or(|c: char| c.is_whitespace() || "([{<\"',;:!?*".contains(c))
        {
            continue;
        }
        while f < fences.len() && fences[f].1 <= i {
            f += 1;
        }
        while s < spans.len() && spans[s].1 <= i {
            s += 1;
        }
        if fences.get(f).is_some_and(|r| r.0 <= i) || spans.get(s).is_some_and(|r| r.0 <= i) {
            continue;
        }
        let rest = &text[i + 1..];
        let agent = rest
            .as_bytes()
            .get(..5)
            .is_some_and(|head| head.eq_ignore_ascii_case(b"agent"))
            && closes(&rest[5..]);
        let person = if let Some(ids) = members {
            ids.iter().any(|id| {
                !id.eq_ignore_ascii_case("agent")
                    && id.len() <= 64
                    && id.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
                    && id.bytes().all(handle_byte)
                    && (!agent || id.len() > 5)
                    && rest.strip_prefix(id).is_some_and(closes)
            })
        } else {
            // Personal Chat doesn't resolve people, but Enter must not
            // auto-promote a draft that looks like it's addressed to them.
            let n = rest.bytes().take_while(|&b| handle_byte(b)).count();
            !agent
                && rest
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphanumeric())
                && closes(&rest[n..])
        };
        result.people |= person;
        result.agent |= agent && !person;
    }
    result
}

fn handle_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

#[derive(Default)]
pub struct Catalog {
    pub targets: Vec<Target>,
    pub system_one: bool,
}

pub struct Target {
    pub provider: String,
    pub connection: String,
    pub model: String,
    pub detail: &'static str,
    pub choice: Choice,
    pub efforts: Vec<String>,
}

impl Catalog {
    pub fn from_rows(rows: Vec<CatalogRow>, space: bool, names: &HashMap<String, String>) -> Self {
        let mut catalog = Self::default();
        for row in rows {
            if row.ready == Some(false)
                || (row.runs_on == "your_own_sign_in" && row.ready != Some(true))
                || !(row.owned
                    || row.visibility == "organization"
                    || (space && row.visibility == "spaces"))
            {
                continue;
            }
            match row.capability.as_str() {
                "system_one" => catalog.system_one = true,
                "chat" => {
                    let provider = match row.provider_id.as_str() {
                        "openai" | "device-openai-oauth" => "OpenAI",
                        "anthropic" => "Anthropic",
                        "gcp.gemini" => "Google Gemini",
                        id => names.get(id).map_or(id, String::as_str),
                    };
                    let detail = match row.runs_on.as_str() {
                        "owner_connection" => "Owner can see requests and may pay for them",
                        "your_own_sign_in" => "Uses your own sign-in",
                        _ => "",
                    };
                    let models = match row.chat_models {
                        Some(models) => models
                            .into_iter()
                            .filter(|m| !m.id.is_empty())
                            .map(|m| (Some(m.id.clone()), m.id, m.reasoning_efforts))
                            .collect(),
                        None => vec![(None, row.model.unwrap_or_else(|| row.name.clone()), vec![])],
                    };
                    for (model_id, model, efforts) in models {
                        catalog.targets.push(Target {
                            provider: provider.to_owned(),
                            connection: row.name.clone(),
                            model,
                            detail,
                            choice: Choice {
                                provider_connection_id: row.connection_id.clone(),
                                model_id,
                                reasoning_effort: None,
                            },
                            efforts,
                        });
                    }
                }
                _ => {}
            }
        }
        catalog
            .targets
            .sort_by_cached_key(|t| (t.provider.to_lowercase(), t.provider.clone()));
        catalog
    }

    pub fn accepts(&self, choice: &Choice) -> bool {
        self.targets.iter().any(|target| {
            target.choice.provider_connection_id == choice.provider_connection_id
                && target.choice.model_id == choice.model_id
                && choice
                    .reasoning_effort
                    .as_ref()
                    .is_none_or(|e| target.efforts.contains(e))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_and_people_guard() {
        let ids = ["bob", "bob.smith", "agent."].map(str::to_owned);
        for (text, expected) in [
            ("mail@agent \\@agent `@agent`", (false, false)),
            ("```\n@agent\n```\n(@AGENT!), @bob.smith", (true, true)),
            ("~~~\n@agent", (false, false)),
            ("@agent.", (false, true)),
            ("@agent @bob", (true, true)),
            ("@bobby @agent.smith", (false, false)),
            ("hello @bob", (false, true)),
        ] {
            let a = address(text, Some(&ids));
            assert_eq!((a.agent, a.people), expected, "{text}");
        }
        assert!(address("hello @stranger", None).people);
        assert!(address("hello @agent.", None).agent);
    }

    #[test]
    fn primary_ask_is_latched_only_for_new_text() {
        let mut d = Draft::default();
        d.insert(" ", true);
        d.insert("a", false);
        d.insert("b", true);
        assert!(!d.ask_on_enter);
        d.backspace();
        d.backspace();
        d.insert("👩‍💻", true);
        assert!(d.ask_on_enter);
        d.backspace();
        assert_eq!(d.text, " ");
    }

    #[test]
    fn uses_only_scoped_usable_models_and_advertised_efforts() {
        let input = serde_json::json!([{
            "connection_id": "pc_1", "name": "QA", "model": "pinned", "provider_id": "openai",
            "capability": "chat", "owned": false, "visibility": "spaces", "runs_on": "owner_connection",
            "chat_models": [{"id": "available", "reasoning_efforts": ["low"]}]
        }, {
            "connection_id": "pc_2", "name": "Unready", "model": "pinned", "provider_id": "openai",
            "capability": "chat", "owned": true, "visibility": "personal", "runs_on": "your_connection",
            "chat_models": []
        }]);
        let parse = || serde_json::from_value(input.clone()).unwrap();
        assert!(
            Catalog::from_rows(parse(), false, &HashMap::new())
                .targets
                .is_empty()
        );
        let catalog = Catalog::from_rows(parse(), true, &HashMap::new());
        assert_eq!(catalog.targets.len(), 1);
        let mut pin = catalog.targets[0].choice.clone();
        assert_eq!(pin.model_id.as_deref(), Some("available"));
        pin.reasoning_effort = Some("low".into());
        assert!(catalog.accepts(&pin));
        pin.reasoning_effort = Some("high".into());
        assert!(!catalog.accepts(&pin));
    }
}
