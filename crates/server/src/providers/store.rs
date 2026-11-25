//! Model connections and their shares in SQLite. Management queries are
//! scoped by the owner's stable user id: a connection id alone never finds a
//! row there. The only owner-independent reads are [`find`] and the share
//! queries, which the access rules in [`super::sharing`] gate.

use rusqlite::{Connection, OptionalExtension, params};

use super::catalog::Capability;

/// A stored connection. `sealed_key` is the encrypted key, never plaintext.
#[derive(Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub provider_id: String,
    pub base_url: Option<String>,
    /// Empty when no model is chosen yet.
    pub model: String,
    /// Whether this is the owner's default for its capability.
    pub is_default: bool,
    pub sealed_key: Option<Vec<u8>>,
    /// Fixed when the connection is created; rows from before capabilities
    /// existed are `chat`.
    pub capability: Capability,
    /// Bumped on model or endpoint changes for search indexes, and on chat
    /// credential changes so an older authorization or listing can't be reused.
    pub config_rev: i64,
}

impl std::fmt::Debug for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Row")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field("provider_id", &self.provider_id)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("is_default", &self.is_default)
            .field("has_key", &self.sealed_key.is_some())
            .field("capability", &self.capability)
            .field("config_rev", &self.config_rev)
            .finish()
    }
}

/// Create the tables, and upgrade older ones additively: existing rows keep
/// every value and become `chat` connections.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS provider_connections (
             id           TEXT PRIMARY KEY NOT NULL,
             user_id      TEXT NOT NULL,
             name         TEXT NOT NULL,
             provider_id  TEXT NOT NULL,
             base_url     TEXT,
             model        TEXT NOT NULL DEFAULT '',
             is_default   INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
             sealed_key   BLOB,
             created_at   INTEGER NOT NULL,
             updated_at   INTEGER NOT NULL,
             CHECK (is_default = 0 OR model <> '')
         );
         CREATE INDEX IF NOT EXISTS provider_connections_user
             ON provider_connections (user_id, created_at);",
    )?;
    let has = |column: &str| -> rusqlite::Result<bool> {
        let mut stmt =
            conn.prepare("SELECT name FROM pragma_table_info('provider_connections')")?;
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(names.iter().any(|n| n == column))
    };
    if !has("capability")? {
        conn.execute_batch(
            "ALTER TABLE provider_connections ADD COLUMN capability TEXT NOT NULL DEFAULT 'chat'
                 CHECK (capability IN ('chat', 'system_one', 'embeddings'));",
        )?;
    }
    if !has("config_rev")? {
        conn.execute_batch(
            "ALTER TABLE provider_connections ADD COLUMN config_rev INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    conn.execute_batch(
        // One default per owner and capability. The old one-per-owner index
        // is equivalent while every row is `chat`; it is replaced, no row
        // changes.
        "DROP INDEX IF EXISTS provider_connections_one_default;
         CREATE UNIQUE INDEX IF NOT EXISTS provider_connections_one_default_per_capability
             ON provider_connections (user_id, capability) WHERE is_default = 1;
         CREATE TABLE IF NOT EXISTS provider_shares (
             connection_id TEXT PRIMARY KEY NOT NULL
                 REFERENCES provider_connections (id) ON DELETE CASCADE,
             visibility    TEXT NOT NULL CHECK (visibility IN ('spaces', 'organization')),
             updated_at    INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS provider_share_spaces (
             connection_id TEXT NOT NULL
                 REFERENCES provider_shares (connection_id) ON DELETE CASCADE,
             space_id      TEXT NOT NULL,
             PRIMARY KEY (connection_id, space_id)
         );
         CREATE INDEX IF NOT EXISTS provider_share_spaces_space
             ON provider_share_spaces (space_id);",
    )?;
    super::models::init(conn)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

const COLUMNS: &str = "id, user_id, name, provider_id, base_url, model, is_default, sealed_key, \
                       capability, config_rev";

fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let capability: String = r.get(8)?;
    Ok(Row {
        id: r.get(0)?,
        user_id: r.get(1)?,
        name: r.get(2)?,
        provider_id: r.get(3)?,
        base_url: r.get(4)?,
        model: r.get(5)?,
        is_default: r.get(6)?,
        sealed_key: r.get(7)?,
        capability: Capability::parse(&capability).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                "unknown connection capability".into(),
            )
        })?,
        config_rev: r.get(9)?,
    })
}

pub fn list(conn: &Connection, user_id: &str) -> rusqlite::Result<Vec<Row>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM provider_connections WHERE user_id = ?1
         ORDER BY created_at, rowid"
    ))?;
    stmt.query_map([user_id], read)?.collect()
}

pub fn get(conn: &Connection, user_id: &str, id: &str) -> rusqlite::Result<Option<Row>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM provider_connections WHERE user_id = ?1 AND id = ?2"),
        params![user_id, id],
        read,
    )
    .optional()
}

/// The connection `id` of whoever owns it. Only for the access checks of
/// [`super::sharing`], which decide whether the caller may use it.
pub fn find(conn: &Connection, id: &str) -> rusqlite::Result<Option<Row>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM provider_connections WHERE id = ?1"),
        [id],
        read,
    )
    .optional()
}

/// `user_id`'s default connection for `capability`.
pub fn default_for(
    conn: &Connection,
    user_id: &str,
    capability: Capability,
) -> rusqlite::Result<Option<Row>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM provider_connections
             WHERE user_id = ?1 AND capability = ?2 AND is_default = 1"
        ),
        params![user_id, capability.as_str()],
        read,
    )
    .optional()
}

pub fn count(conn: &Connection, user_id: &str) -> rusqlite::Result<usize> {
    conn.query_row(
        "SELECT COUNT(*) FROM provider_connections WHERE user_id = ?1",
        [user_id],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n as usize)
}

/// Insert `row`. When `row.is_default`, the owner's other default of the
/// same capability is cleared in the same transaction. `false` when the
/// owner already has `max` connections.
pub fn insert(conn: &mut Connection, row: &Row, max: usize) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    if count(&tx, &row.user_id)? >= max {
        return Ok(false);
    }
    if row.is_default {
        tx.execute(
            "UPDATE provider_connections SET is_default = 0
             WHERE user_id = ?1 AND capability = ?2 AND is_default = 1",
            params![row.user_id, row.capability.as_str()],
        )?;
    }
    let t = now();
    tx.execute(
        "INSERT INTO provider_connections
             (id, user_id, name, provider_id, base_url, model, is_default, sealed_key,
              capability, config_rev, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?10)",
        params![
            row.id,
            row.user_id,
            row.name,
            row.provider_id,
            row.base_url,
            row.model,
            row.is_default,
            row.sealed_key,
            row.capability.as_str(),
            t
        ],
    )?;
    // The connection's model is always one of its enabled models.
    super::models::ensure_enabled(&tx, &row.id, &row.model)?;
    tx.commit()?;
    Ok(true)
}

/// Write `row` back (matched by owner and id), clearing the owner's other
/// default of the same capability when `row.is_default`. A model change or
/// a chat credential change bumps `config_rev`. This does not write the
/// capability or base URL. `None` when the row no longer exists, otherwise the stored
/// `config_rev`.
pub fn update(conn: &mut Connection, row: &Row) -> rusqlite::Result<Option<i64>> {
    write_update(conn, row, None)
}

/// Save a patch only if the connection still matches the original snapshot.
/// A patch can include an externally tested endpoint. The check and update
/// share a transaction.
pub fn update_if_current(
    conn: &mut Connection,
    row: &Row,
    previous: &Row,
) -> rusqlite::Result<Option<i64>> {
    write_update(conn, row, Some(previous))
}

fn write_update(
    conn: &mut Connection,
    row: &Row,
    previous: Option<&Row>,
) -> rusqlite::Result<Option<i64>> {
    let tx = conn.transaction()?;
    let Some(old) = get(&tx, &row.user_id, &row.id)? else {
        return Ok(None);
    };
    if previous.is_some_and(|previous| previous != &old) {
        return Ok(None);
    }
    let endpoint_changed = previous.is_some() && old.base_url != row.base_url;
    if row.is_default {
        tx.execute(
            "UPDATE provider_connections SET is_default = 0
             WHERE user_id = ?1 AND capability = ?3 AND is_default = 1 AND id <> ?2",
            params![row.user_id, row.id, row.capability.as_str()],
        )?;
    }
    let rev: Option<i64> = tx
        .query_row(
            "UPDATE provider_connections
             SET name = ?3, is_default = ?5, sealed_key = ?6, updated_at = ?7,
                  config_rev = config_rev + (model <> ?4
                      OR (capability = 'chat' AND sealed_key IS NOT ?6)
                      OR ?8),
                  model = ?4, base_url = ?9
             WHERE user_id = ?1 AND id = ?2
             RETURNING config_rev",
            params![
                row.user_id,
                row.id,
                row.name,
                row.model,
                row.is_default,
                row.sealed_key,
                now(),
                endpoint_changed,
                if endpoint_changed {
                    &row.base_url
                } else {
                    &old.base_url
                },
            ],
            |r| r.get(0),
        )
        .optional()?;
    if rev.is_some() {
        super::models::ensure_enabled(&tx, &row.id, &row.model)?;
    }
    // A media connection that now computes something else: everyone's
    // library vectors built with it are stale, in this same transaction.
    // A rename, default flag or key rotation leaves them valid.
    if rev.is_some() && (old.model != row.model || endpoint_changed) && media(&row.provider_id) {
        crate::library_search::hooks::connection_changed(&tx, &row.id)?;
    }
    tx.commit()?;
    Ok(rev)
}

/// Whether `provider_id` is a media embeddings adapter (library vectors
/// may have been built with its connections).
fn media(provider_id: &str) -> bool {
    super::catalog::find(provider_id).is_some_and(|e| e.media_inputs)
}

/// Delete `id` of `user_id`; the deleted row, or `None` when there was none.
/// Its share goes with it, and so do the library vectors anyone built
/// with it when it was a media connection (same transaction).
pub fn delete(conn: &Connection, user_id: &str, id: &str) -> rusqlite::Result<Option<Row>> {
    let tx = conn.unchecked_transaction()?;
    let row = tx
        .query_row(
            &format!(
                "DELETE FROM provider_connections WHERE user_id = ?1 AND id = ?2 \
                 RETURNING {COLUMNS}"
            ),
            params![user_id, id],
            read,
        )
        .optional()?;
    if let Some(row) = &row
        && media(&row.provider_id)
    {
        crate::library_search::hooks::connection_changed(&tx, &row.id)?;
    }
    // The owner's default for that capability is gone: their oldest
    // remaining connection of it that has a model takes over.
    if let Some(row) = &row
        && row.is_default
    {
        tx.execute(
            "UPDATE provider_connections SET is_default = 1
             WHERE id = (SELECT id FROM provider_connections
                         WHERE user_id = ?1 AND capability = ?2 AND model <> ''
                         ORDER BY created_at, rowid LIMIT 1)
               AND NOT EXISTS (SELECT 1 FROM provider_connections
                               WHERE user_id = ?1 AND capability = ?2 AND is_default = 1)",
            params![row.user_id, row.capability.as_str()],
        )?;
    }
    tx.commit()?;
    Ok(row)
}

// ---- Shares ----

/// Who besides the owner may use a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Only the owner (no share row).
    Personal,
    /// Members of the listed Spaces, while the owner is a member too.
    Spaces,
    /// Every signed-in user of this server's organization.
    Organization,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Personal => "personal",
            Visibility::Spaces => "spaces",
            Visibility::Organization => "organization",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "spaces" => Some(Visibility::Spaces),
            "organization" => Some(Visibility::Organization),
            "personal" => Some(Visibility::Personal),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    pub visibility: Visibility,
    /// Sorted; empty unless `visibility` is `Spaces`.
    pub space_ids: Vec<String>,
}

impl Share {
    pub fn personal() -> Self {
        Self {
            visibility: Visibility::Personal,
            space_ids: Vec::new(),
        }
    }
}

/// The share of connection `id` (personal when it has none).
pub fn share_of(conn: &Connection, id: &str) -> rusqlite::Result<Share> {
    let visibility: Option<String> = conn
        .query_row(
            "SELECT visibility FROM provider_shares WHERE connection_id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(visibility) = visibility.as_deref().and_then(Visibility::parse) else {
        return Ok(Share::personal());
    };
    let mut stmt = conn.prepare_cached(
        "SELECT space_id FROM provider_share_spaces WHERE connection_id = ?1 ORDER BY space_id",
    )?;
    let space_ids = stmt
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(Share {
        visibility,
        space_ids,
    })
}

/// Replace the share of `owner`'s connection `id` with `share`. `false`
/// when `owner` has no such connection.
pub fn set_share(
    conn: &mut Connection,
    owner: &str,
    id: &str,
    share: &Share,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    let Some(row) = get(&tx, owner, id)? else {
        return Ok(false);
    };
    let before = share_of(&tx, id)?;
    tx.execute("DELETE FROM provider_shares WHERE connection_id = ?1", [id])?;
    if share.visibility != Visibility::Personal {
        tx.execute(
            "INSERT INTO provider_shares (connection_id, visibility, updated_at)
             VALUES (?1, ?2, ?3)",
            params![id, share.visibility.as_str(), now()],
        )?;
        for sid in &share.space_ids {
            tx.execute(
                "INSERT OR IGNORE INTO provider_share_spaces (connection_id, space_id)
                 VALUES (?1, ?2)",
                params![id, sid],
            )?;
        }
    }
    // Revoked or narrowed: other people's library vectors built with this
    // media connection go now, in the same transaction; the owner's stay.
    if narrowed(&before, share) && media(&row.provider_id) {
        crate::library_search::hooks::connection_access_changed(&tx, id, owner)?;
    }
    tx.commit()?;
    Ok(true)
}

/// Whether `after` lets fewer people use a connection than `before`: an
/// organization share made smaller, or a Space share that lost a Space.
pub fn narrowed(before: &Share, after: &Share) -> bool {
    match (before.visibility, after.visibility) {
        (Visibility::Personal, _) => false,
        (Visibility::Organization, v) => v != Visibility::Organization,
        (Visibility::Spaces, Visibility::Organization) => false,
        (Visibility::Spaces, Visibility::Personal) => true,
        (Visibility::Spaces, Visibility::Spaces) => before
            .space_ids
            .iter()
            .any(|s| !after.space_ids.contains(s)),
    }
}

/// Whether `user_id` is a member (or the owner) of Space `space_id` now.
pub fn is_member(conn: &Connection, space_id: &str, user_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM space_members WHERE space_id = ?1 AND user_id = ?2)",
        params![space_id, user_id],
        |r| r.get(0),
    )
}

/// Other people's connections shared with the organization, or with a
/// Space both `caller` and the owner belong to now. `space`: only shares
/// with that Space (plus organization shares); `None` with `any_space`
/// false: organization shares only; `None` with `any_space`: any Space.
pub fn shared_with(
    conn: &Connection,
    caller: &str,
    space: Option<&str>,
    any_space: bool,
) -> rusqlite::Result<Vec<(Row, Visibility)>> {
    let columns = COLUMNS
        .split(", ")
        .map(|c| format!("c.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {columns}, s.visibility FROM provider_connections c
         JOIN provider_shares s ON s.connection_id = c.id
         WHERE c.user_id <> ?1 AND c.model <> '' AND (
             s.visibility = 'organization'
             OR (s.visibility = 'spaces' AND (?2 IS NOT NULL OR ?3) AND EXISTS (
                 SELECT 1 FROM provider_share_spaces ss
                 JOIN space_members me ON me.space_id = ss.space_id AND me.user_id = ?1
                 JOIN space_members owner ON owner.space_id = ss.space_id
                      AND owner.user_id = c.user_id
                 WHERE ss.connection_id = c.id AND (?2 IS NULL OR ss.space_id = ?2)))
         )
         ORDER BY c.created_at, c.rowid"
    ))?;
    stmt.query_map(params![caller, space, any_space], |r| {
        let row = read(r)?;
        let v: String = r.get(10)?;
        Ok((row, Visibility::parse(&v).unwrap_or(Visibility::Personal)))
    })?
    .collect()
}
