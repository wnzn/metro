//! Library files attached to chat messages.
//!
//! A message may carry up to [`MAX_ATTACHMENTS`] files someone already
//! uploaded to the library (`POST /v1/library/assets`). The message stores
//! only links to them: the bytes stay in `library_blobs`, once, and are
//! opened through the library's own authenticated `/content` route, which
//! checks access again.
//!
//! Scope never changes hands, and only the uploader attaches a file. A
//! personal Chat message may attach only the sender's own personal files; a
//! Space room or thread message only files the sender uploaded to that same
//! Space. Other members read and open what was delivered, but can't attach
//! someone else's file. The sender's membership, the room and every file are
//! checked in the same `IMMEDIATE` transaction that writes the event and its
//! links, so a file deleted, a membership removed or a room gone in between
//! posts nothing. Every read filters the links again by what the reader may
//! read now. Deleting a file empties its links (`ON DELETE SET NULL`): the
//! message and the rest of the transcript stay, and the file's place shows
//! as `{"removed": true}`, with nothing that names it.
//!
//! Attachments are for people. The agent's history is built from event text
//! alone and never reads them (see `agent.rs`).

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use crate::db::{Addressed, Author, Db, Event, kind};
use crate::library::AssetKind;

/// Files one message may carry, at most.
pub const MAX_ATTACHMENTS: usize = 10;

/// One attachment as a reader of the message sees it: a file, from the
/// library's current metadata, or the place of a file that was deleted.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Attachment {
    File(AttachedFile),
    /// Always `{"removed": true}`: no id, name or type of what it was.
    Removed {
        removed: bool,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AttachedFile {
    pub asset_id: String,
    pub name: String,
    pub mime_type: String,
    pub kind: AssetKind,
    pub size_bytes: u64,
}

/// Where a message goes: a person's own Chat, or a room (or thread) of a Space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Personal { user_id: String },
    Space { space_id: String, room_id: String },
}

impl Place {
    /// The link table's `scope`: personal and Space event ids are separate
    /// sequences, so an id alone could name either.
    fn scope(&self) -> &'static str {
        match self {
            Place::Personal { .. } => "personal",
            Place::Space { .. } => "space",
        }
    }
}

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS message_attachments (
             scope    TEXT NOT NULL CHECK (scope IN ('personal', 'space')),
             event_id INTEGER NOT NULL,
             position INTEGER NOT NULL,
             asset_id TEXT REFERENCES library_assets (id) ON DELETE SET NULL,
             PRIMARY KEY (scope, event_id, position),
             UNIQUE (scope, event_id, asset_id)
         );
         CREATE INDEX IF NOT EXISTS message_attachments_asset ON message_attachments (asset_id);",
    )
}

/// Why a list of asset ids is refused before anything is looked up.
#[derive(Debug, PartialEq, Eq)]
pub enum IdsError {
    TooMany,
    Duplicate,
    /// Not an id the library could have made: answered like a missing file.
    Unknown,
}

/// `ids` checked for count, duplicates and shape, in the order given.
pub fn check_ids(ids: &[String]) -> Result<(), IdsError> {
    if ids.len() > MAX_ATTACHMENTS {
        return Err(IdsError::TooMany);
    }
    let mut seen = HashSet::new();
    for id in ids {
        if !crate::library::plausible_id(id) {
            return Err(IdsError::Unknown);
        }
        if !seen.insert(id.as_str()) {
            return Err(IdsError::Duplicate);
        }
    }
    Ok(())
}

/// The outcome of [`append_message`].
#[derive(Debug)]
pub enum Append {
    Posted(Box<Event>),
    /// The sender is not a member of the Space now, or the room is gone.
    NoRoom,
    /// A file is missing, deleted, or not one this place may attach.
    NoAsset,
}

/// Write a person's message with its attachments, in one transaction with
/// every check (see the module docs). Nothing is written unless all pass.
pub fn append_message(
    db: &Db,
    place: &Place,
    author: &Author,
    text: &str,
    addressed: &Addressed,
    asset_ids: &[String],
) -> anyhow::Result<Append> {
    let Some(sender) = author.id.as_deref() else {
        return Ok(Append::NoRoom);
    };
    let mut conn = db.lock();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Place::Space { space_id, room_id } = place {
        let allowed: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)
                AND EXISTS (SELECT 1 FROM space_rooms WHERE space_id = ?1 AND id = ?3)",
            params![space_id, sender, room_id],
            |r| r.get(0),
        )?;
        if !allowed {
            return Ok(Append::NoRoom);
        }
    }
    let mut attachments = Vec::with_capacity(asset_ids.len());
    {
        // The sender's own upload, of exactly this place: personal for the
        // Chat, this Space for a room (membership was checked above). With bytes.
        let mut stmt = tx.prepare_cached(
            "SELECT a.id, a.name, a.mime_type, a.kind, a.size_bytes FROM library_assets a
             WHERE a.id = ?1 AND a.uploader_id = ?3
               AND ((?2 IS NULL AND a.space_id IS NULL) OR (?2 IS NOT NULL AND a.space_id = ?2))
               AND EXISTS (SELECT 1 FROM library_blobs b WHERE b.asset_id = a.id)",
        )?;
        let space = match place {
            Place::Personal { .. } => None,
            Place::Space { space_id, .. } => Some(space_id.as_str()),
        };
        for id in asset_ids {
            match stmt
                .query_row(params![id, space, sender], row_attachment)
                .optional()?
            {
                Some(a) => attachments.push(a),
                None => return Ok(Append::NoAsset),
            }
        }
    }
    let at = crate::db::now_ms();
    match place {
        Place::Personal { user_id } => tx.execute(
            "INSERT INTO personal_events (user_id, kind, actor, actor_id, text, mentions, audience, created_at, activity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
            params![
                user_id,
                kind::MESSAGE,
                author.name,
                author.id,
                text,
                addressed.mentions.to_string(),
                addressed.audience,
                at,
            ],
        )?,
        Place::Space { space_id, room_id } => tx.execute(
            "INSERT INTO space_events (space_id, room_id, kind, actor, actor_id, text, mentions, audience, created_at, activity)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)",
            params![
                space_id,
                room_id,
                kind::MESSAGE,
                author.name,
                author.id,
                text,
                addressed.mentions.to_string(),
                addressed.audience,
                at,
            ],
        )?,
    };
    let event_id = tx.last_insert_rowid();
    for (position, a) in attachments.iter().enumerate() {
        tx.execute(
            "INSERT INTO message_attachments (scope, event_id, position, asset_id) VALUES (?1, ?2, ?3, ?4)",
            params![place.scope(), event_id, position as i64, a.asset_id],
        )?;
    }
    tx.commit()?;
    let mut event =
        crate::db::new_event(event_id, kind::MESSAGE, author, text, addressed, None, at);
    event.attachments = attachments.into_iter().map(Attachment::File).collect();
    Ok(Append::Posted(Box::new(event)))
}

fn row_attachment(r: &rusqlite::Row<'_>) -> rusqlite::Result<AttachedFile> {
    let kind: String = r.get(3)?;
    let kind = AssetKind::parse(&kind).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            format!("unknown asset kind {kind:?}").into(),
        )
    })?;
    Ok(AttachedFile {
        asset_id: r.get(0)?,
        name: r.get(1)?,
        mime_type: r.get(2)?,
        kind,
        size_bytes: r.get::<_, i64>(4)?.max(0) as u64,
    })
}

/// Fill in the attachments of `events` (all of `place`) that `reader` may
/// read now, in their original order. A deleted file shows as
/// [`Attachment::Removed`]; a file the reader may not read is left out.
pub fn hydrate(db: &Db, reader: &str, place: &Place, events: &mut [Event]) -> anyhow::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let ids = serde_json::to_string(&events.iter().map(|e| e.id).collect::<Vec<_>>())?;
    let conn = db.lock();
    // ?4 is the Space for a room, NULL for the personal Chat.
    let (readable, space) = match place {
        Place::Personal { .. } => (
            "a.space_id IS NULL AND a.uploader_id = ?3 AND ?4 IS NULL",
            None,
        ),
        Place::Space { space_id, .. } => (
            "a.space_id = ?4 AND EXISTS (SELECT 1 FROM space_members m
                 WHERE m.space_id = ?4 AND m.user_id = ?3)",
            Some(space_id.as_str()),
        ),
    };
    let sql = format!(
        "SELECT a.id, a.name, a.mime_type, a.kind, a.size_bytes, l.event_id, l.asset_id IS NULL
         FROM message_attachments l LEFT JOIN library_assets a ON a.id = l.asset_id
         WHERE l.scope = ?1 AND l.event_id IN (SELECT value FROM json_each(?2))
           AND (l.asset_id IS NULL OR (a.id IS NOT NULL AND {readable}))
         ORDER BY l.event_id, l.position"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut found: HashMap<i64, Vec<Attachment>> = HashMap::new();
    let rows = stmt.query_map(params![place.scope(), ids, reader, space], |r| {
        let attachment = if r.get::<_, bool>(6)? {
            Attachment::Removed { removed: true }
        } else {
            Attachment::File(row_attachment(r)?)
        };
        Ok((r.get::<_, i64>(5)?, attachment))
    })?;
    for row in rows {
        let (event_id, a) = row?;
        found.entry(event_id).or_default().push(a);
    }
    for e in events.iter_mut() {
        e.attachments = found.remove(&e.id).unwrap_or_default();
    }
    Ok(())
}
