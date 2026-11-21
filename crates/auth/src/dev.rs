//! Explicit dev-token mode (`METRO_DEV_USERS`).

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use subtle::{Choice, ConstantTimeEq};

use crate::{AuthError, ConfigError, Identity, env};

pub(crate) const MIN_TOKEN_LEN: usize = 16;
pub(crate) const MAX_TOKEN_LEN: usize = 512;
pub(crate) const MAX_USERS: usize = 256;
const MAX_ACTOR_LEN: usize = 64;

pub(crate) struct DevUsers {
    /// (actor, sha256(token)). Only digests are kept in memory.
    entries: Vec<(String, [u8; 32])>,
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn invalid(reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        var: env::DEV_USERS,
        reason: reason.into(),
    }
}

fn valid_actor_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ACTOR_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'))
}

fn valid_token_chars(token: &str) -> bool {
    // Visible ASCII only: no whitespace, no control characters.
    token.bytes().all(|b| b.is_ascii_graphic())
}

impl DevUsers {
    pub(crate) fn parse(raw: &str, runner_token: Option<&str>) -> Result<Self, ConfigError> {
        // serde_json rejects duplicate keys only when told to; BTreeMap would
        // silently keep the last one, so detect duplicates by parsing into a
        // Vec of pairs first.
        let pairs: Vec<(String, String)> = parse_object_pairs(raw)?;

        if pairs.is_empty() {
            return Err(invalid("must contain at least one user"));
        }
        if pairs.len() > MAX_USERS {
            return Err(invalid(format!("at most {MAX_USERS} users are allowed")));
        }

        let runner_digest = match runner_token {
            Some(t) if !t.is_empty() => Some(digest(t)),
            _ => None,
        };

        let mut seen_actors = BTreeMap::new();
        let mut entries: Vec<(String, [u8; 32])> = Vec::with_capacity(pairs.len());
        for (actor, token) in pairs {
            if !valid_actor_name(&actor) {
                return Err(invalid(format!(
                    "user name {actor:?} must be 1-{MAX_ACTOR_LEN} characters of [A-Za-z0-9._@-]"
                )));
            }
            if seen_actors.insert(actor.clone(), ()).is_some() {
                return Err(invalid(format!("user {actor:?} is listed more than once")));
            }
            if token.len() < MIN_TOKEN_LEN || token.len() > MAX_TOKEN_LEN {
                return Err(invalid(format!(
                    "token for {actor:?} must be {MIN_TOKEN_LEN}-{MAX_TOKEN_LEN} bytes"
                )));
            }
            if !valid_token_chars(&token) {
                return Err(invalid(format!(
                    "token for {actor:?} must be printable ASCII without spaces"
                )));
            }
            let d = digest(&token);
            if let Some((other, _)) = entries.iter().find(|(_, e)| bool::from(e.ct_eq(&d))) {
                return Err(invalid(format!(
                    "users {other:?} and {actor:?} share a token; every person needs a distinct token"
                )));
            }
            if runner_digest.is_some_and(|r| bool::from(r.ct_eq(&d))) {
                return Err(invalid(format!(
                    "token for {actor:?} equals {}; user and runner tokens must differ",
                    env::RUNNER_TOKEN
                )));
            }
            entries.push((actor, d));
        }
        Ok(Self { entries })
    }

    /// Dev identities are immutable: `id == actor == username`.
    pub(crate) fn identity(&self, token: &str) -> Result<Identity, AuthError> {
        if token.len() > MAX_TOKEN_LEN {
            return Err(AuthError::Invalid("token too long"));
        }
        let d = digest(token);
        // Compare against every entry so timing does not reveal which entry
        // (if any) matched. Tokens are distinct, so at most one matches.
        let mut found: Option<&str> = None;
        let mut any = Choice::from(0);
        for (actor, expected) in &self.entries {
            let eq = expected.ct_eq(&d);
            any |= eq;
            if bool::from(eq) {
                found = Some(actor);
            }
        }
        match (bool::from(any), found) {
            (true, Some(actor)) => Ok(Identity {
                id: actor.to_owned(),
                actor: actor.to_owned(),
            }),
            _ => Err(AuthError::Invalid("unknown dev token")),
        }
    }
}

/// Parse a JSON object of string -> string, preserving duplicates.
fn parse_object_pairs(raw: &str) -> Result<Vec<(String, String)>, ConfigError> {
    use serde::de::{Deserializer, MapAccess, Visitor};
    use std::fmt;

    struct Pairs;
    impl<'de> Visitor<'de> for Pairs {
        type Value = Vec<(String, String)>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a JSON object mapping user name to token string")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some((k, v)) = map.next_entry::<String, String>()? {
                out.push((k, v));
            }
            Ok(out)
        }
    }

    let mut de = serde_json::Deserializer::from_str(raw);
    let pairs = (&mut de)
        .deserialize_map(Pairs)
        // serde_json errors only carry position + category, never the value.
        .map_err(|e| {
            invalid(format!(
                "expected JSON object of user -> token ({})",
                e.classify_str()
            ))
        })?;
    de.end()
        .map_err(|_| invalid("trailing characters after JSON object"))?;
    Ok(pairs)
}

trait ClassifyStr {
    fn classify_str(&self) -> &'static str;
}

impl ClassifyStr for serde_json::Error {
    fn classify_str(&self) -> &'static str {
        match self.classify() {
            serde_json::error::Category::Io => "io error",
            serde_json::error::Category::Syntax => "syntax error",
            serde_json::error::Category::Data => "wrong value type",
            serde_json::error::Category::Eof => "unexpected end of input",
        }
    }
}
