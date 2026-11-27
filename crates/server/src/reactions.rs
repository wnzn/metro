//! Emoji reactions on chat messages, in Space rooms and in personal Chat.
//!
//! # Model
//!
//! Reaction state is a set of `(target, reactor, emoji)` rows. Space rooms
//! and personal Chats use separate tables, because `space_events` and
//! `personal_events` have separate id sequences: event 7 of a room and event
//! 7 of someone's personal Chat are different messages, and a reaction to one
//! can never land on the other.
//!
//! Every change that actually happens also appends one `reaction` event to
//! the same stream as its target (the room, or the user's personal Chat), in
//! the same transaction. Its `text` is a [`ReactionPayload`] in JSON. Clients
//! therefore get reactions through the usual cursor (`events_after`) and
//! replay them as **set operations**: `present: true` adds the
//! `(target, reactor, emoji)` triple and `present: false` removes it.
//! Applying an event twice, or on top of a [`Db::reactions_for`] snapshot,
//! gives the same state, so nothing is double counted. Repeating an add or a
//! remove changes nothing and writes no event.
//!
//! # Who may react
//!
//! [`Reactor`] comes from the server's trusted session, never from a request
//! body or a model. [`Db::set_reaction`] re-checks access in its own
//! transaction:
//!
//! - A person (`Reactor::Human`) must be a member of the Space (Space scope)
//!   or the owner of the personal Chat (personal scope). They may react to
//!   `message` and `agent` events of that room or Chat.
//! - The agent (`Reactor::Agent { as_user }`) acts on behalf of the person
//!   who addressed it, who must still have access as above. It may react only
//!   to a `message` written by that person and, in a Space room, addressed to
//!   the agent (`audience` `agent` or `mixed`). [`ReactToMessageTool`] binds
//!   it to the one message that started the turn.
//!
//! Reactions never start agent work and never call a model.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rig_core::completion::ToolAnnotations;
use rig_core::tool::{Tool, ToolContext, ToolExecutionError};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Db, actor, kind, now_ms};

/// Event kind of a reaction change.
pub const KIND: &str = "reaction";

/// The emoji people and the agent may react with, in display order.
pub const PALETTE: [&str; 6] = ["👍", "👀", "✅", "🙏", "❤️", "🎉"];

/// What the model may answer with when its reaction alone answers the
/// message. The server should then post no text reply.
pub const REACTION_ONLY: &str = "METRO_REACTION_ONLY";

/// Most targets one [`Db::reactions_for`] call may ask about.
pub const MAX_SNAPSHOT_TARGETS: usize = 500;

/// Longest id the reaction code accepts, in bytes.
const MAX_ID_BYTES: usize = 512;

/// `reactor_id` of the agent. The scope is one room or one personal Chat,
/// which has one agent, so a fixed id is enough.
pub const AGENT_REACTOR_ID: &str = "agent";

/// Whether `emoji` is exactly one of [`PALETTE`].
pub fn valid_emoji(emoji: &str) -> bool {
    PALETTE.contains(&emoji)
}

/// The palette entry `emoji` stands for. Unlike [`valid_emoji`] it also
/// accepts a heart without the emoji variation selector (`U+2764` alone),
/// which models often produce.
pub fn canonical_emoji(emoji: &str) -> Option<&'static str> {
    let emoji = emoji.trim();
    if emoji == "\u{2764}" {
        return Some("❤️");
    }
    PALETTE.iter().copied().find(|p| *p == emoji)
}

/// Where a reaction lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReactionScope {
    /// A room (or thread) of a Space.
    Space { space_id: String, room_id: String },
    /// A user's personal Chat with the agent.
    Personal { user_id: String },
}

/// Who reacts. Built by the server from the authenticated session, never
/// from client or model input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reactor {
    /// A person: stable identity id (`sub` or dev username) and display
    /// name. Only `id` identifies the reaction; `name` labels the event.
    Human { id: String, name: String },
    /// The agent, answering a message from `as_user`.
    Agent { as_user: String },
}

impl Reactor {
    fn kind(&self) -> &'static str {
        match self {
            Reactor::Human { .. } => "user",
            Reactor::Agent { .. } => "agent",
        }
    }

    fn reactor_id(&self) -> &str {
        match self {
            Reactor::Human { id, .. } => id,
            Reactor::Agent { .. } => AGENT_REACTOR_ID,
        }
    }

    /// The user whose access is checked.
    fn user(&self) -> &str {
        match self {
            Reactor::Human { id, .. } => id,
            Reactor::Agent { as_user } => as_user,
        }
    }

    fn author(&self) -> (&str, Option<&str>) {
        match self {
            Reactor::Human { id, name } => (name, Some(id)),
            Reactor::Agent { .. } => (actor::AGENT, None),
        }
    }
}

/// The JSON `text` of a `reaction` event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReactionPayload {
    pub target_event_id: i64,
    pub emoji: String,
    pub present: bool,
    /// `user` or `agent`.
    pub reactor_kind: String,
    /// A person's stable id, or `agent`.
    pub reactor_id: String,
}

/// One current reaction, as returned by [`Db::reactions_for`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Reaction {
    pub target_event_id: i64,
    pub emoji: String,
    pub reactor_kind: String,
    pub reactor_id: String,
}

/// Create the reaction tables. Idempotent; call it from `Db::init` after
/// `space_events` and `personal_events` exist.
pub fn init(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS space_reactions (
             space_id        TEXT NOT NULL,
             room_id         TEXT NOT NULL,
             target_event_id INTEGER NOT NULL REFERENCES space_events (id),
             reactor_kind    TEXT NOT NULL CHECK (reactor_kind IN ('user', 'agent')),
             reactor_id      TEXT NOT NULL,
             emoji           TEXT NOT NULL,
             created_at      INTEGER NOT NULL,
             PRIMARY KEY (space_id, room_id, target_event_id, reactor_kind, reactor_id, emoji)
         ) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS personal_reactions (
             user_id         TEXT NOT NULL,
             target_event_id INTEGER NOT NULL REFERENCES personal_events (id),
             reactor_kind    TEXT NOT NULL CHECK (reactor_kind IN ('user', 'agent')),
             reactor_id      TEXT NOT NULL,
             emoji           TEXT NOT NULL,
             created_at      INTEGER NOT NULL,
             PRIMARY KEY (user_id, target_event_id, reactor_kind, reactor_id, emoji)
         ) WITHOUT ROWID;",
    )?;
    Ok(())
}

fn ok_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES
}

impl ReactionScope {
    fn ids_ok(&self) -> bool {
        match self {
            ReactionScope::Space { space_id, room_id } => ok_id(space_id) && ok_id(room_id),
            ReactionScope::Personal { user_id } => ok_id(user_id),
        }
    }
}

/// The target's kind, author id and audience, if it is in `scope`.
fn target_row(
    conn: &Connection,
    scope: &ReactionScope,
    target: i64,
) -> rusqlite::Result<Option<(String, Option<String>, String)>> {
    let row = |r: &rusqlite::Row<'_>| Ok((r.get(0)?, r.get(1)?, r.get(2)?));
    match scope {
        ReactionScope::Space { space_id, room_id } => conn
            .query_row(
                "SELECT kind, actor_id, audience FROM space_events
                 WHERE id = ?1 AND space_id = ?2 AND room_id = ?3",
                params![target, space_id, room_id],
                row,
            )
            .optional(),
        ReactionScope::Personal { user_id } => conn
            .query_row(
                "SELECT kind, actor_id, audience FROM personal_events
                 WHERE id = ?1 AND user_id = ?2",
                params![target, user_id],
                row,
            )
            .optional(),
    }
}

fn has_access(conn: &Connection, scope: &ReactionScope, user: &str) -> rusqlite::Result<bool> {
    match scope {
        ReactionScope::Space { space_id, .. } => conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)",
            params![space_id, user],
            |r| r.get(0),
        ),
        ReactionScope::Personal { user_id } => Ok(user_id == user),
    }
}

/// Whether `reactor` may react to a target of `kind` written by `author`.
fn may_react(
    scope: &ReactionScope,
    reactor: &Reactor,
    target_kind: &str,
    author: Option<&str>,
    audience: &str,
) -> bool {
    match reactor {
        Reactor::Human { .. } => target_kind == kind::MESSAGE || target_kind == kind::AGENT,
        Reactor::Agent { as_user } => {
            target_kind == kind::MESSAGE
                && author == Some(as_user.as_str())
                && match scope {
                    ReactionScope::Space { .. } => matches!(audience, "agent" | "mixed"),
                    ReactionScope::Personal { .. } => true,
                }
        }
    }
}

impl Db {
    /// Add (`present`) or remove one emoji reaction to event `target`.
    ///
    /// Returns `Some(true)` when the state changed (and a `reaction` event
    /// was appended), `Some(false)` when it already was as asked, and `None`
    /// when the reaction is not allowed: unknown, out-of-scope or
    /// non-chat target, a reactor without access, an emoji outside
    /// [`PALETTE`], or a malformed id. `None` does not say which, so it
    /// reveals nothing about other Spaces or Chats.
    pub fn set_reaction(
        &self,
        scope: &ReactionScope,
        target: i64,
        reactor: &Reactor,
        emoji: &str,
        present: bool,
    ) -> anyhow::Result<Option<bool>> {
        if target <= 0 || !valid_emoji(emoji) || !scope.ids_ok() || !ok_id(reactor.user()) {
            return Ok(None);
        }
        if let Reactor::Human { name, .. } = reactor
            && name.len() > MAX_ID_BYTES
        {
            return Ok(None);
        }

        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !has_access(&tx, scope, reactor.user())? {
            return Ok(None);
        }
        let Some((target_kind, target_author, audience)) = target_row(&tx, scope, target)? else {
            return Ok(None);
        };
        if !may_react(
            scope,
            reactor,
            &target_kind,
            target_author.as_deref(),
            &audience,
        ) {
            return Ok(None);
        }

        let (rk, rid) = (reactor.kind(), reactor.reactor_id());
        let changed = match (scope, present) {
            (ReactionScope::Space { space_id, room_id }, true) => tx.execute(
                "INSERT OR IGNORE INTO space_reactions
                     (space_id, room_id, target_event_id, reactor_kind, reactor_id, emoji, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![space_id, room_id, target, rk, rid, emoji, now_ms()],
            )?,
            (ReactionScope::Space { space_id, room_id }, false) => tx.execute(
                "DELETE FROM space_reactions
                 WHERE space_id = ?1 AND room_id = ?2 AND target_event_id = ?3
                   AND reactor_kind = ?4 AND reactor_id = ?5 AND emoji = ?6",
                params![space_id, room_id, target, rk, rid, emoji],
            )?,
            (ReactionScope::Personal { user_id }, true) => tx.execute(
                "INSERT OR IGNORE INTO personal_reactions
                     (user_id, target_event_id, reactor_kind, reactor_id, emoji, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![user_id, target, rk, rid, emoji, now_ms()],
            )?,
            (ReactionScope::Personal { user_id }, false) => tx.execute(
                "DELETE FROM personal_reactions
                 WHERE user_id = ?1 AND target_event_id = ?2
                   AND reactor_kind = ?3 AND reactor_id = ?4 AND emoji = ?5",
                params![user_id, target, rk, rid, emoji],
            )?,
        } == 1;
        if !changed {
            // Nothing to commit; dropping the transaction rolls it back.
            return Ok(Some(false));
        }

        let payload = ReactionPayload {
            target_event_id: target,
            emoji: emoji.to_owned(),
            present,
            reactor_kind: rk.to_owned(),
            reactor_id: rid.to_owned(),
        };
        let text = serde_json::to_string(&payload)?;
        let (name, author_id) = reactor.author();
        match scope {
            ReactionScope::Space { space_id, room_id } => tx.execute(
                "INSERT INTO space_events
                     (space_id, room_id, kind, actor, actor_id, text, mentions, audience, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, '[]', 'room', ?7)",
                params![space_id, room_id, KIND, name, author_id, text, now_ms()],
            )?,
            // In the target's own thread (or the main Chat).
            ReactionScope::Personal { user_id } => tx.execute(
                "INSERT INTO personal_events
                     (user_id, thread_id, kind, actor, actor_id, text, mentions, audience, created_at)
                 VALUES (?1, (SELECT thread_id FROM personal_events WHERE id = ?7 AND user_id = ?1),
                         ?2, ?3, ?4, ?5, '[]', 'room', ?6)",
                params![user_id, KIND, name, author_id, text, now_ms(), target],
            )?,
        };
        tx.commit()?;
        Ok(Some(true))
    }

    /// Current reactions on `targets` (event ids of `scope`), ordered by
    /// target, emoji and reactor. For the first page a client loads; later
    /// changes arrive as `reaction` events. The caller must already have
    /// authorized reading `scope`, as for `events_after`. Ids outside the
    /// scope simply have no reactions. At most [`MAX_SNAPSHOT_TARGETS`].
    pub fn reactions_for(
        &self,
        scope: &ReactionScope,
        targets: &[i64],
    ) -> anyhow::Result<Vec<Reaction>> {
        anyhow::ensure!(
            targets.len() <= MAX_SNAPSHOT_TARGETS,
            "at most {MAX_SNAPSHOT_TARGETS} targets per reaction snapshot"
        );
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let ids = serde_json::to_string(targets)?;
        let conn = self.lock();
        let row = |r: &rusqlite::Row<'_>| {
            Ok(Reaction {
                target_event_id: r.get(0)?,
                emoji: r.get(1)?,
                reactor_kind: r.get(2)?,
                reactor_id: r.get(3)?,
            })
        };
        let rows = match scope {
            ReactionScope::Space { space_id, room_id } => {
                let mut stmt = conn.prepare_cached(
                    "SELECT target_event_id, emoji, reactor_kind, reactor_id FROM space_reactions
                     WHERE space_id = ?1 AND room_id = ?2
                       AND target_event_id IN (SELECT value FROM json_each(?3))
                     ORDER BY target_event_id, emoji, reactor_kind, reactor_id",
                )?;
                stmt.query_map(params![space_id, room_id, ids], row)?
                    .collect::<Result<Vec<_>, _>>()?
            }
            ReactionScope::Personal { user_id } => {
                let mut stmt = conn.prepare_cached(
                    "SELECT target_event_id, emoji, reactor_kind, reactor_id FROM personal_reactions
                     WHERE user_id = ?1 AND target_event_id IN (SELECT value FROM json_each(?2))
                     ORDER BY target_event_id, emoji, reactor_kind, reactor_id",
                )?;
                stmt.query_map(params![user_id, ids], row)?
                    .collect::<Result<Vec<_>, _>>()?
            }
        };
        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// Model tool
// ---------------------------------------------------------------------------

/// Arguments of `react_to_message`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReactArgs {
    pub emoji: String,
}

/// `react_to_message`: the agent reacts to the message it is answering.
///
/// Built per turn and bound to that turn's trigger: the scope, target and
/// user come from the server, and the model chooses only the emoji. At most
/// one emoji per turn: repeating it succeeds without a change, another one is
/// refused. `reacted` is set once a reaction is in place.
pub struct ReactToMessageTool {
    db: Db,
    scope: ReactionScope,
    target: i64,
    as_user: String,
    reacted: Arc<AtomicBool>,
    /// The emoji placed this turn. Held across the database call so parallel
    /// calls are serialized.
    chosen: tokio::sync::Mutex<Option<&'static str>>,
}

impl ReactToMessageTool {
    pub fn new(
        db: Db,
        scope: ReactionScope,
        target: i64,
        as_user: String,
        reacted: Arc<AtomicBool>,
    ) -> Self {
        Self {
            db,
            scope,
            target,
            as_user,
            reacted,
            chosen: tokio::sync::Mutex::new(None),
        }
    }
}

fn done_text(emoji: &str, changed: bool) -> String {
    let what = if changed {
        "Reacted"
    } else {
        "Already reacted"
    };
    format!(
        "{what} with {emoji}. If this reaction fully answers the message, reply with exactly \
         {REACTION_ONLY} and nothing else; otherwise write your reply as usual."
    )
}

impl Tool for ReactToMessageTool {
    const NAME: &'static str = "react_to_message";
    type Args = ReactArgs;
    type Output = String;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "React with one emoji to the message you are answering. Allowed: {}. Use it sparingly, \
             only when a reaction is a natural, helpful answer, such as acknowledging thanks, \
             confirming a simple request or marking something done. It is not proof that you did \
             any work and does not replace an answer when one is needed. At most one reaction per \
             reply.",
            PALETTE.join(" ")
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "emoji": {
                    "type": "string",
                    "enum": PALETTE,
                    "description": "The emoji to react with."
                }
            },
            "required": ["emoji"],
            "additionalProperties": false
        })
    }

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations {
            title: Some("React to message".into()),
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
        }
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<String, ToolExecutionError> {
        let emoji = (args.emoji.len() <= 32)
            .then(|| canonical_emoji(&args.emoji))
            .flatten()
            .ok_or_else(|| {
                ToolExecutionError::invalid_args(format!(
                    "`emoji` must be one of: {}",
                    PALETTE.join(" ")
                ))
            })?;

        let mut chosen = self.chosen.lock().await;
        if let Some(previous) = *chosen {
            if previous == emoji {
                return Ok(done_text(emoji, false));
            }
            return Err(ToolExecutionError::other(format!(
                "You already reacted with {previous} in this reply; only one reaction is allowed."
            )));
        }

        let scope = self.scope.clone();
        let target = self.target;
        let reactor = Reactor::Agent {
            as_user: self.as_user.clone(),
        };
        match self
            .db
            .call(move |db| db.set_reaction(&scope, target, &reactor, emoji, true))
            .await
        {
            Ok(Some(changed)) => {
                *chosen = Some(emoji);
                self.reacted.store(true, Ordering::Release);
                Ok(done_text(emoji, changed))
            }
            Ok(None) => Err(ToolExecutionError::other(
                "This message cannot be reacted to.",
            )),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "saving agent reaction");
                Err(ToolExecutionError::other(
                    "The reaction could not be saved; continue without it.",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Addressed, Author, Event};
    use super::*;
    use std::collections::BTreeSet;

    fn db() -> Db {
        let db = Db::open_in_memory().unwrap();
        init(&db.lock()).unwrap();
        db
    }

    fn human(id: &str) -> Reactor {
        Reactor::Human {
            id: id.into(),
            name: format!("{id} display"),
        }
    }

    fn agent(as_user: &str) -> Reactor {
        Reactor::Agent {
            as_user: as_user.into(),
        }
    }

    fn to_agent() -> Addressed {
        Addressed {
            mentions: json!([{"kind": "agent"}]),
            audience: "agent".into(),
            spans: None,
        }
    }

    fn person(id: &str) -> Author {
        Author {
            id: Some(id.into()),
            name: format!("{id} display"),
        }
    }

    /// A Space owned by alice with member bob, and its lobby scope.
    fn space(db: &Db) -> (String, ReactionScope) {
        let s = db.create_space("alice", "team", 10).unwrap();
        db.add_member(&s.id, "bob", 10).unwrap();
        let scope = ReactionScope::Space {
            space_id: s.id.clone(),
            room_id: super::super::LOBBY_ID.into(),
        };
        (s.id, scope)
    }

    fn post(db: &Db, space_id: &str, kind: &str, author: &Author, addressed: &Addressed) -> i64 {
        db.append_event(
            space_id,
            super::super::LOBBY_ID,
            kind,
            author,
            "hi",
            addressed,
        )
        .unwrap()
        .id
    }

    fn reaction_events(events: &[Event]) -> Vec<(ReactionPayload, String, Option<String>)> {
        events
            .iter()
            .filter(|e| e.kind == KIND)
            .map(|e| {
                (
                    serde_json::from_str(&e.text).unwrap(),
                    e.actor.clone(),
                    e.actor_id.clone(),
                )
            })
            .collect()
    }

    fn all_space(db: &Db, space_id: &str) -> Vec<Event> {
        db.events_after(space_id, super::super::LOBBY_ID, 0, 1000)
            .unwrap()
    }

    #[test]
    fn palette_and_emoji_checks() {
        assert_eq!(PALETTE.join(" "), "👍 👀 ✅ 🙏 ❤️ 🎉");
        for e in PALETTE {
            assert!(valid_emoji(e));
            assert_eq!(canonical_emoji(e), Some(e));
        }
        assert!(!valid_emoji("\u{2764}"));
        assert_eq!(canonical_emoji("\u{2764}"), Some("❤️"));
        for bad in ["", "👎", "👍👍", ":+1:", "👍🏽", " 👍"] {
            assert!(!valid_emoji(bad), "{bad}");
        }
    }

    #[test]
    fn human_add_remove_is_idempotent_and_emits_once() {
        let db = db();
        let (sid, scope) = space(&db);
        let msg = post(
            &db,
            &sid,
            kind::MESSAGE,
            &person("alice"),
            &Addressed::room(),
        );

        assert_eq!(
            db.set_reaction(&scope, msg, &human("bob"), "👍", true)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            db.set_reaction(&scope, msg, &human("bob"), "👍", true)
                .unwrap(),
            Some(false)
        );
        // The display name is only a label: same id, same reaction.
        let renamed = Reactor::Human {
            id: "bob".into(),
            name: "Robert".into(),
        };
        assert_eq!(
            db.set_reaction(&scope, msg, &renamed, "👍", true).unwrap(),
            Some(false)
        );
        assert_eq!(
            db.set_reaction(&scope, msg, &human("bob"), "👍", false)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            db.set_reaction(&scope, msg, &human("bob"), "👍", false)
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            db.set_reaction(&scope, msg, &human("bob"), "🎉", false)
                .unwrap(),
            Some(false)
        );

        let events = reaction_events(&all_space(&db, &sid));
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].0,
            ReactionPayload {
                target_event_id: msg,
                emoji: "👍".into(),
                present: true,
                reactor_kind: "user".into(),
                reactor_id: "bob".into(),
            }
        );
        assert_eq!(
            (events[0].1.as_str(), events[0].2.as_deref()),
            ("bob display", Some("bob"))
        );
        assert!(!events[1].0.present);
        assert!(db.reactions_for(&scope, &[msg]).unwrap().is_empty());
    }

    #[test]
    fn rejects_outsiders_other_rooms_and_non_chat_targets() {
        let db = db();
        let (sid, scope) = space(&db);
        let (other_sid, other_scope) = {
            let s = db.create_space("carol", "other", 10).unwrap();
            let scope = ReactionScope::Space {
                space_id: s.id.clone(),
                room_id: super::super::LOBBY_ID.into(),
            };
            (s.id, scope)
        };
        let msg = post(
            &db,
            &sid,
            kind::MESSAGE,
            &person("alice"),
            &Addressed::room(),
        );
        let reply = post(
            &db,
            &sid,
            kind::AGENT,
            &Author::server(actor::AGENT),
            &Addressed::room(),
        );
        let err = post(
            &db,
            &sid,
            kind::ERROR,
            &Author::server(actor::SYSTEM),
            &Addressed::room(),
        );
        let other_msg = post(
            &db,
            &other_sid,
            kind::MESSAGE,
            &person("carol"),
            &Addressed::room(),
        );
        let room = db.create_room(&sid, "dev", None, 10, 10).unwrap();
        let dev_scope = ReactionScope::Space {
            space_id: sid.clone(),
            room_id: room.id,
        };

        // People may react to messages and agent replies.
        assert_eq!(
            db.set_reaction(&scope, reply, &human("alice"), "🙏", true)
                .unwrap(),
            Some(true)
        );
        let before = all_space(&db, &sid).len();

        let none = [
            db.set_reaction(&scope, msg, &human("carol"), "👍", true), // not a member
            db.set_reaction(&scope, err, &human("alice"), "👍", true), // not a chat event
            db.set_reaction(&scope, other_msg, &human("alice"), "👍", true), // other Space's event
            db.set_reaction(&other_scope, msg, &human("carol"), "👍", true), // event not in that Space
            db.set_reaction(&dev_scope, msg, &human("alice"), "👍", true),   // wrong room
            db.set_reaction(&scope, 9_999, &human("alice"), "👍", true),     // no such event
            db.set_reaction(&scope, 0, &human("alice"), "👍", true),
            db.set_reaction(&scope, -1, &human("alice"), "👍", true),
            db.set_reaction(&scope, msg, &human("alice"), "👎", true), // not in the palette
            db.set_reaction(&scope, msg, &human(""), "👍", true),
            db.set_reaction(
                &scope,
                msg,
                &human(&"x".repeat(MAX_ID_BYTES + 1)),
                "👍",
                true,
            ),
            db.set_reaction(
                &ReactionScope::Space {
                    space_id: String::new(),
                    room_id: "lobby".into(),
                },
                msg,
                &human("alice"),
                "👍",
                true,
            ),
        ];
        for (i, r) in none.into_iter().enumerate() {
            assert_eq!(r.unwrap(), None, "case {i}");
        }
        // A removed member loses access too.
        db.set_reaction(&scope, msg, &human("bob"), "👀", true)
            .unwrap();
        db.remove_member(&sid, "bob").unwrap();
        assert_eq!(
            db.set_reaction(&scope, msg, &human("bob"), "👀", false)
                .unwrap(),
            None
        );

        assert_eq!(all_space(&db, &sid).len(), before + 1);
        assert!(reaction_events(&db.events_after(&other_sid, "lobby", 0, 100).unwrap()).is_empty());
    }

    #[test]
    fn agent_reacts_only_to_its_addressed_trigger() {
        let db = db();
        let (sid, scope) = space(&db);
        let ask = post(&db, &sid, kind::MESSAGE, &person("bob"), &to_agent());
        let mixed = post(
            &db,
            &sid,
            kind::MESSAGE,
            &person("bob"),
            &Addressed {
                mentions: json!([{"kind":"agent"},{"kind":"user","id":"alice"}]),
                audience: "mixed".into(),
                spans: None,
            },
        );
        let to_room = post(&db, &sid, kind::MESSAGE, &person("bob"), &Addressed::room());
        let to_alice = post(
            &db,
            &sid,
            kind::MESSAGE,
            &person("bob"),
            &Addressed {
                mentions: json!([{"kind":"user","id":"alice"}]),
                audience: "users".into(),
                spans: None,
            },
        );
        let reply = post(
            &db,
            &sid,
            kind::AGENT,
            &Author::server(actor::AGENT),
            &Addressed::room(),
        );
        let by_agent = post(
            &db,
            &sid,
            kind::MESSAGE,
            &Author::server(actor::AGENT),
            &to_agent(),
        );

        assert_eq!(
            db.set_reaction(&scope, ask, &agent("bob"), "✅", true)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            db.set_reaction(&scope, mixed, &agent("bob"), "✅", true)
                .unwrap(),
            Some(true)
        );
        for target in [to_room, to_alice, reply, by_agent] {
            assert_eq!(
                db.set_reaction(&scope, target, &agent("bob"), "✅", true)
                    .unwrap(),
                None
            );
        }
        // Acting for someone else, or for an outsider.
        assert_eq!(
            db.set_reaction(&scope, ask, &agent("alice"), "👀", true)
                .unwrap(),
            None
        );
        assert_eq!(
            db.set_reaction(&scope, ask, &agent("carol"), "👀", true)
                .unwrap(),
            None
        );

        // Human and agent reactions with the same emoji are separate.
        assert_eq!(
            db.set_reaction(&scope, ask, &human("bob"), "✅", true)
                .unwrap(),
            Some(true)
        );
        let rows = db.reactions_for(&scope, &[ask]).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].reactor_kind.as_str(), rows[0].reactor_id.as_str()),
            ("agent", "agent")
        );
        assert_eq!(
            (rows[1].reactor_kind.as_str(), rows[1].reactor_id.as_str()),
            ("user", "bob")
        );

        let events = reaction_events(&all_space(&db, &sid));
        let (payload, name, id) = &events[0];
        assert_eq!(
            (payload.reactor_kind.as_str(), payload.reactor_id.as_str()),
            ("agent", "agent")
        );
        assert_eq!((name.as_str(), id.as_deref()), (actor::AGENT, None));
    }

    #[test]
    fn personal_scope_is_owner_only_and_separate_from_spaces() {
        let db = db();
        let (sid, space_scope) = space(&db);
        let space_msg = post(
            &db,
            &sid,
            kind::MESSAGE,
            &person("alice"),
            &Addressed::room(),
        );
        let mine = db
            .append_personal(
                "alice",
                kind::MESSAGE,
                &person("alice"),
                "hi",
                &Addressed::room(),
            )
            .unwrap()
            .id;
        let reply = db
            .append_personal(
                "alice",
                kind::AGENT,
                &Author::server(actor::AGENT),
                "hello",
                &Addressed::room(),
            )
            .unwrap()
            .id;
        let bobs = db
            .append_personal(
                "bob",
                kind::MESSAGE,
                &person("bob"),
                "hey",
                &Addressed::room(),
            )
            .unwrap()
            .id;
        // Separate sequences: the same number names different messages.
        assert_eq!(mine, space_msg);

        let alice = ReactionScope::Personal {
            user_id: "alice".into(),
        };
        let bob = ReactionScope::Personal {
            user_id: "bob".into(),
        };
        assert_eq!(
            db.set_reaction(&alice, mine, &human("alice"), "❤️", true)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            db.set_reaction(&alice, reply, &human("alice"), "👍", true)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            db.set_reaction(&alice, mine, &agent("alice"), "👀", true)
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            db.set_reaction(&alice, reply, &agent("alice"), "👀", true)
                .unwrap(),
            None
        );
        assert_eq!(
            db.set_reaction(&alice, mine, &human("bob"), "❤️", true)
                .unwrap(),
            None
        );
        assert_eq!(
            db.set_reaction(&alice, mine, &agent("bob"), "❤️", true)
                .unwrap(),
            None
        );
        assert_eq!(
            db.set_reaction(&alice, bobs, &human("alice"), "❤️", true)
                .unwrap(),
            None
        );
        assert_eq!(
            db.set_reaction(&bob, mine, &human("bob"), "❤️", true)
                .unwrap(),
            None
        );

        // The personal reaction did not touch the Space message with the same id.
        assert!(
            db.reactions_for(&space_scope, &[space_msg])
                .unwrap()
                .is_empty()
        );
        assert!(reaction_events(&all_space(&db, &sid)).is_empty());
        let personal = db.personal_after("alice", 0, 100).unwrap();
        assert_eq!(reaction_events(&personal).len(), 3);
        assert!(reaction_events(&db.personal_after("bob", 0, 100).unwrap()).is_empty());
        assert_eq!(db.reactions_for(&alice, &[mine, reply]).unwrap().len(), 3);
        assert!(db.reactions_for(&bob, &[mine]).unwrap().is_empty());
    }

    #[test]
    fn replay_after_a_cursor_matches_the_snapshot() {
        let db = db();
        let (sid, scope) = space(&db);
        let a = post(
            &db,
            &sid,
            kind::MESSAGE,
            &person("alice"),
            &Addressed::room(),
        );
        let b = post(&db, &sid, kind::MESSAGE, &person("bob"), &Addressed::room());
        db.set_reaction(&scope, a, &human("bob"), "👍", true)
            .unwrap();
        db.set_reaction(&scope, a, &human("alice"), "👍", true)
            .unwrap();

        // A client loads the page and the snapshot, then follows the cursor.
        let cursor = all_space(&db, &sid).last().unwrap().id;
        let key = |r: &Reaction| {
            (
                r.target_event_id,
                r.emoji.clone(),
                r.reactor_kind.clone(),
                r.reactor_id.clone(),
            )
        };
        let mut state: BTreeSet<_> = db
            .reactions_for(&scope, &[a, b])
            .unwrap()
            .iter()
            .map(key)
            .collect();
        assert_eq!(state.len(), 2);

        db.set_reaction(&scope, a, &human("bob"), "👍", false)
            .unwrap();
        db.set_reaction(&scope, b, &human("alice"), "🎉", true)
            .unwrap();
        db.set_reaction(&scope, b, &human("alice"), "🎉", true)
            .unwrap(); // no event
        db.set_reaction(&scope, a, &human("bob"), "👍", true)
            .unwrap();

        let newer = reaction_events(&db.events_after(&sid, "lobby", cursor, 100).unwrap());
        assert_eq!(newer.len(), 3);
        // Replay twice: set semantics make it idempotent.
        for _ in 0..2 {
            for (p, _, _) in &newer {
                let k = (
                    p.target_event_id,
                    p.emoji.clone(),
                    p.reactor_kind.clone(),
                    p.reactor_id.clone(),
                );
                if p.present {
                    state.insert(k);
                } else {
                    state.remove(&k);
                }
            }
        }
        let now: BTreeSet<_> = db
            .reactions_for(&scope, &[a, b])
            .unwrap()
            .iter()
            .map(key)
            .collect();
        assert_eq!(state, now);
        assert_eq!(now.len(), 3);

        assert!(db.reactions_for(&scope, &[]).unwrap().is_empty());
        assert!(
            db.reactions_for(&scope, &vec![1; MAX_SNAPSHOT_TARGETS + 1])
                .is_err()
        );
    }

    #[test]
    fn tool_args_reject_unknown_fields() {
        assert!(serde_json::from_value::<ReactArgs>(json!({"emoji": "👍"})).is_ok());
        assert!(serde_json::from_value::<ReactArgs>(json!({"emoji": "👍", "target": 1})).is_err());
        assert!(
            serde_json::from_value::<ReactArgs>(json!({"emoji": "👍", "user_id": "x"})).is_err()
        );
        assert!(serde_json::from_value::<ReactArgs>(json!({})).is_err());
    }

    #[tokio::test]
    async fn tool_places_one_reaction_per_turn() {
        let db = db();
        let (sid, scope) = space(&db);
        let ask = post(&db, &sid, kind::MESSAGE, &person("bob"), &to_agent());
        let reacted = Arc::new(AtomicBool::new(false));
        let tool = ReactToMessageTool::new(
            db.clone(),
            scope.clone(),
            ask,
            "bob".into(),
            reacted.clone(),
        );
        let mut cx = ToolContext::default();
        let call = |e: &str| ReactArgs { emoji: e.into() };

        assert_eq!(ReactToMessageTool::NAME, "react_to_message");
        assert_eq!(tool.parameters()["additionalProperties"], json!(false));
        assert!(tool.call(&mut cx, call("👎")).await.is_err());
        assert!(tool.call(&mut cx, call(&"👍".repeat(20))).await.is_err());
        assert!(!reacted.load(Ordering::Acquire));

        let out = tool.call(&mut cx, call("\u{2764}")).await.unwrap();
        assert!(out.starts_with("Reacted with ❤️") && out.contains(REACTION_ONLY));
        assert!(reacted.load(Ordering::Acquire));
        assert!(
            tool.call(&mut cx, call("❤️"))
                .await
                .unwrap()
                .starts_with("Already reacted")
        );
        assert!(tool.call(&mut cx, call("👍")).await.is_err());
        let rows = db.reactions_for(&scope, &[ask]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].emoji, "❤️");
        assert_eq!(reaction_events(&all_space(&db, &sid)).len(), 1);

        // A fresh turn whose earlier reaction is already there counts as done.
        let again = Arc::new(AtomicBool::new(false));
        let tool2 =
            ReactToMessageTool::new(db.clone(), scope.clone(), ask, "bob".into(), again.clone());
        assert!(
            tool2
                .call(&mut cx, call("❤️"))
                .await
                .unwrap()
                .starts_with("Already reacted")
        );
        assert!(again.load(Ordering::Acquire));

        // Bound to a target the agent may not react to: refused, flag untouched.
        let plain = post(&db, &sid, kind::MESSAGE, &person("bob"), &Addressed::room());
        let denied = Arc::new(AtomicBool::new(false));
        let tool3 = ReactToMessageTool::new(db.clone(), scope, plain, "bob".into(), denied.clone());
        assert!(tool3.call(&mut cx, call("👍")).await.is_err());
        assert!(!denied.load(Ordering::Acquire));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tool_concurrent_calls_place_one_emoji() {
        let db = db();
        let (sid, scope) = space(&db);
        let ask = post(&db, &sid, kind::MESSAGE, &person("bob"), &to_agent());
        let reacted = Arc::new(AtomicBool::new(false));
        let tool = Arc::new(ReactToMessageTool::new(
            db.clone(),
            scope.clone(),
            ask,
            "bob".into(),
            reacted.clone(),
        ));
        let tasks: Vec<_> = PALETTE
            .iter()
            .map(|e| {
                let tool = tool.clone();
                tokio::spawn(async move {
                    tool.call(
                        &mut ToolContext::default(),
                        ReactArgs { emoji: (*e).into() },
                    )
                    .await
                })
            })
            .collect();
        let mut ok = 0;
        for t in tasks {
            ok += t.await.unwrap().is_ok() as usize;
        }
        assert_eq!(ok, 1);
        assert!(reacted.load(Ordering::Acquire));
        assert_eq!(db.reactions_for(&scope, &[ask]).unwrap().len(), 1);
    }
}
