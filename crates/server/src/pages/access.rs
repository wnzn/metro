//! Who may do what with a page, decided from the database on every request.
//!
//! - **Personal page**: only its owner, for everything. Nobody else, not
//!   even an org admin, can tell it exists (404).
//! - **Space page**: every current member views it. Its creator (while a
//!   member) and the Space owner edit it, manage "Members can edit" and
//!   delete it; with "Members can edit" on, every member edits it too.
//!   Outsiders get 404, members without the right 403.
//! - **Soft-deleted page**: visible only to those who may restore it.

use super::store::{PageRow, Scope};
use crate::db::{Db, Role};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Rights {
    pub can_edit: bool,
    pub can_delete: bool,
    /// Turn "Members can edit" on or off.
    pub can_manage: bool,
}

/// `user`'s rights on `page` now; `None` when they may not see it at all.
pub fn rights(db: &Db, page: &PageRow, user: &str) -> anyhow::Result<Option<Rights>> {
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
            match db.membership(sid, user)? {
                None => None,
                Some(space) => {
                    let lead = space.role == Role::Owner || page.creator_id == user;
                    Some(Rights {
                        can_edit: lead || page.members_edit,
                        can_delete: lead,
                        can_manage: lead,
                    })
                }
            }
        }
    };
    Ok(match rights {
        Some(r) if page.deleted_at.is_some() && !r.can_delete => None,
        other => other,
    })
}

/// Resolves names for a page being saved, as the person saving it: only
/// Spaces they're a member of now, and only rooms and agents in them.
pub struct DbResolver<'a> {
    pub db: &'a Db,
    pub user: &'a str,
    pub space: Option<String>,
}

impl super::compile::Resolver for DbResolver<'_> {
    fn page_space(&self) -> Option<&str> {
        self.space.as_deref()
    }

    fn space(&mut self, id: &str) -> Result<String, String> {
        let not = || format!("no Space \"{id}\" that you're a member of");
        if crate::config::validate_id(id).is_err() {
            return Err(not());
        }
        match self.db.membership(id, self.user) {
            Ok(Some(s)) => Ok(s.name),
            _ => Err(not()),
        }
    }

    fn room(&mut self, space: &str, name: &str) -> Result<(String, String), String> {
        let rooms = self
            .db
            .list_rooms(space)
            .map_err(|_| "rooms couldn't be read".to_owned())?;
        rooms
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(name))
            .or_else(|| rooms.iter().find(|r| r.id == name))
            .map(|r| (r.id.clone(), r.name.clone()))
            .ok_or_else(|| {
                let names: Vec<&str> = rooms
                    .iter()
                    .filter(|r| r.parent_room_id.is_none())
                    .take(20)
                    .map(|r| r.name.as_str())
                    .collect();
                format!(
                    "no room named \"{name}\" in this Space; its rooms are: {}",
                    names.join(", ")
                )
            })
    }

    fn agent(&mut self, space: &str, handle: &str) -> Result<(String, String), String> {
        let agents = self
            .db
            .space_agents(space)
            .map_err(|_| "agents couldn't be read".to_owned())?;
        agents
            .iter()
            .find(|a| a.handle.eq_ignore_ascii_case(handle))
            .map(|a| (a.id.clone(), a.handle.clone()))
            .ok_or_else(|| {
                let handles: Vec<String> =
                    agents.iter().map(|a| format!("@{}", a.handle)).collect();
                format!(
                    "no agent @{handle} in this Space; its agents are: {}",
                    handles.join(", ")
                )
            })
    }
}
