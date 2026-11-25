//! Who may do what with a page, decided from the database on every request.
//!
//! - **Personal page**: only its owner, for everything. Nobody else, not
//!   even an org admin, can tell it exists (404).
//! - **Space page**: every current member views it. Its creator (while a
//!   member) and the Space owner edit it, manage "Members can edit" and
//!   delete it; with "Members can edit" on, every member edits it too.
//!   Outsiders get 404, members without the right 403.
//! - **Soft-deleted page**: visible only to those who may restore it, and
//!   only within the 30-day restore window.
//!
//! [`rights_in`] works on a connection, so every write can decide rights
//! in the same transaction as the write itself.

use rusqlite::{Connection, OptionalExtension, params};

use super::store::{DELETED_RETENTION_MS, PageRow, Scope};
use crate::db::{Db, now_ms};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Rights {
    pub can_edit: bool,
    pub can_delete: bool,
    /// Turn "Members can edit" on or off.
    pub can_manage: bool,
}

/// `user`'s role in `space` now, if they're a member.
pub fn member_role(conn: &Connection, space: &str, user: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT role FROM space_members WHERE space_id = ?1 AND user_id = ?2",
        params![space, user],
        |r| r.get(0),
    )
    .optional()
}

/// `user`'s rights on `page` now, read on `conn`; `None` when they may not
/// see it at all.
pub fn rights_in(
    conn: &Connection,
    page: &PageRow,
    user: &str,
) -> rusqlite::Result<Option<Rights>> {
    let rights = match page.scope {
        Scope::Personal => (page.owner_id.as_deref() == Some(user)).then_some(Rights {
            can_edit: true,
            can_delete: true,
            can_manage: true,
        }),
        Scope::Space => {
            let Some(sid) = page.space_id.as_deref() else {
                return Ok(None);
            };
            match member_role(conn, sid, user)? {
                None => None,
                Some(role) => {
                    let lead = role == "owner" || page.creator_id == user;
                    Some(Rights {
                        can_edit: lead || page.members_edit,
                        can_delete: lead,
                        can_manage: lead,
                    })
                }
            }
        }
    };
    Ok(match (rights, page.deleted_at) {
        (Some(r), Some(at)) if !r.can_delete || at < now_ms() - DELETED_RETENTION_MS => None,
        (other, _) => other,
    })
}

/// [`rights_in`] on the shared connection.
pub fn rights(db: &Db, page: &PageRow, user: &str) -> anyhow::Result<Option<Rights>> {
    Ok(rights_in(&db.lock(), page, user)?)
}

/// Resolves names for a page being saved, as the person saving it: only
/// Spaces they're a member of now, and only rooms and agents in them. Each
/// lookup joins their membership in the same statement, so nothing about a
/// Space they aren't in (not even its room names) reaches a diagnostic.
pub struct DbResolver<'a> {
    pub db: &'a Db,
    pub user: &'a str,
    pub space: Option<String>,
}

fn not_member(id: &str) -> String {
    format!("no Space \"{id}\" that you're a member of")
}

impl super::compile::Resolver for DbResolver<'_> {
    fn page_space(&self) -> Option<&str> {
        self.space.as_deref()
    }

    fn space(&mut self, id: &str) -> Result<String, String> {
        if crate::config::validate_id(id).is_err() {
            return Err(not_member(id));
        }
        match self.db.membership(id, self.user) {
            Ok(Some(s)) => Ok(s.name),
            _ => Err(not_member(id)),
        }
    }

    fn room(&mut self, space: &str, name: &str) -> Result<(String, String, bool), String> {
        let rooms: Vec<(String, String, bool)> = {
            let conn = self.db.lock();
            let stmt = conn.prepare_cached(
                "SELECT r.id, r.name, r.parent_room_id IS NOT NULL FROM space_rooms r
                 WHERE r.space_id = ?1 AND EXISTS (SELECT 1 FROM space_members m
                     WHERE m.space_id = r.space_id AND m.user_id = ?2)
                 ORDER BY r.created_at, r.rowid",
            );
            let Ok(mut stmt) = stmt else {
                return Err("rooms couldn't be read".to_owned());
            };
            let rows = stmt.query_map(params![space, self.user], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            });
            match rows.and_then(|r| r.collect::<Result<Vec<_>, _>>()) {
                Ok(v) => v,
                Err(_) => return Err("rooms couldn't be read".to_owned()),
            }
        };
        // Not a member now: no names at all.
        if rooms.is_empty() {
            return Err(not_member(space));
        }
        rooms
            .iter()
            .find(|r| r.1.eq_ignore_ascii_case(name))
            .or_else(|| rooms.iter().find(|r| r.0 == name))
            .cloned()
            .ok_or_else(|| {
                let names: Vec<&str> = rooms
                    .iter()
                    .filter(|r| !r.2)
                    .take(20)
                    .map(|r| r.1.as_str())
                    .collect();
                format!(
                    "no room named \"{name}\" in this Space; its rooms are: {}",
                    names.join(", ")
                )
            })
    }

    fn agent(&mut self, space: &str, handle: &str) -> Result<(String, String), String> {
        let agents: Vec<(String, String)> = {
            let conn = self.db.lock();
            let stmt = conn.prepare_cached(
                "SELECT a.id, a.handle FROM space_agents a
                 WHERE a.space_id = ?1 AND EXISTS (SELECT 1 FROM space_members m
                     WHERE m.space_id = a.space_id AND m.user_id = ?2)
                 ORDER BY a.created_at, a.rowid",
            );
            let Ok(mut stmt) = stmt else {
                return Err("agents couldn't be read".to_owned());
            };
            let rows = stmt.query_map(params![space, self.user], |r| Ok((r.get(0)?, r.get(1)?)));
            match rows.and_then(|r| r.collect::<Result<Vec<_>, _>>()) {
                Ok(v) => v,
                Err(_) => return Err("agents couldn't be read".to_owned()),
            }
        };
        if agents.is_empty() {
            return Err(not_member(space));
        }
        agents
            .iter()
            .find(|a| a.1.eq_ignore_ascii_case(handle))
            .cloned()
            .ok_or_else(|| {
                let handles: Vec<String> = agents.iter().map(|a| format!("@{}", a.1)).collect();
                format!(
                    "no agent @{handle} in this Space; its agents are: {}",
                    handles.join(", ")
                )
            })
    }
}
