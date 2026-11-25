//! The live grant behind every Metro-in-Chat tool call: the switch is on,
//! the notice was shown, the person is a member of the Space now, and the
//! Space isn't excluded. Checked on the connection (or transaction) that
//! does the read or write, so a Space the person left is refused on the
//! very next call, even within one turn.

use rusqlite::Connection;

use super::prefs;
use crate::db::{Db, SpaceView};

/// Why a call was refused. The texts go to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// The switch is off, or the notice wasn't shown yet.
    Off,
    /// Not a member, excluded, or no such Space: the same answer for all.
    NoSpace,
}

impl Denied {
    pub fn text(self) -> &'static str {
        match self {
            Denied::Off => {
                "Metro no longer has access to the user's Spaces from Chat; they can turn it on \
                 in Settings → Metro in Chat"
            }
            Denied::NoSpace => "no such Space",
        }
    }
}

/// The person a turn acts as.
#[derive(Clone, Debug)]
pub struct Grant {
    pub user: String,
}

impl Grant {
    pub fn new(user: impl Into<String>) -> Self {
        Self { user: user.into() }
    }

    /// The switch and notice gate alone.
    pub fn on(&self, conn: &Connection) -> anyhow::Result<Result<(), Denied>> {
        Ok(if prefs::switch_in(conn, &self.user)?.active() {
            Ok(())
        } else {
            Err(Denied::Off)
        })
    }

    /// Space `space_id` as the person sees it now, if every condition holds.
    pub fn check(
        &self,
        conn: &Connection,
        space_id: &str,
    ) -> anyhow::Result<Result<SpaceView, Denied>> {
        if let Err(d) = self.on(conn)? {
            return Ok(Err(d));
        }
        if crate::config::validate_id(space_id).is_err()
            || prefs::excluded_in(conn, &self.user, space_id)?
        {
            return Ok(Err(Denied::NoSpace));
        }
        Ok(Db::membership_in(conn, space_id, &self.user)?.ok_or(Denied::NoSpace))
    }

    /// Every Space the tools may use now, in the person's usual order:
    /// member Spaces minus excluded ones. Empty when the switch is off.
    pub fn spaces(&self, conn: &Connection) -> anyhow::Result<Result<Vec<SpaceView>, Denied>> {
        if let Err(d) = self.on(conn)? {
            return Ok(Err(d));
        }
        let mut out = Vec::new();
        for s in Db::spaces_in(conn, &self.user)? {
            if !prefs::excluded_in(conn, &self.user, &s.id)? {
                out.push(s);
            }
        }
        Ok(Ok(out))
    }

    /// [`Grant::spaces`], narrowed to `only` when given. An id outside the
    /// grant is refused as [`Denied::NoSpace`].
    pub fn spaces_among(
        &self,
        conn: &Connection,
        only: Option<&[String]>,
    ) -> anyhow::Result<Result<Vec<SpaceView>, Denied>> {
        let all = match self.spaces(conn)? {
            Ok(all) => all,
            Err(d) => return Ok(Err(d)),
        };
        let Some(only) = only else {
            return Ok(Ok(all));
        };
        let mut out = Vec::new();
        for id in only {
            match all.iter().find(|s| &s.id == id) {
                Some(s) if !out.iter().any(|o: &SpaceView| o.id == s.id) => out.push(s.clone()),
                Some(_) => {}
                None => return Ok(Err(Denied::NoSpace)),
            }
        }
        Ok(Ok(out))
    }
}
