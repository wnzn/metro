//! Reading tools: Spaces and rooms, a room's messages, catch-up, search
//! and what's waiting on the person. Each checks the grant on the same
//! connection hold that reads, and returns only `message`, `agent` and
//! `thread` events (never activity, tool, compaction or other kinds).

use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Ctx;
use super::grant::Grant;
use super::tools::{Out, R, args, denied, internal, on_db};
use crate::auth::Human;
use crate::db::{Db, Event, Room, SpaceView, rfc3339_utc, row_event};
use crate::search::clip;

pub(super) const READ_MAX: usize = 40;
const READ_DEFAULT: usize = 20;
const READ_TEXT: usize = 2_000;
const LIST_SPACES: usize = 50;
const LIST_ROOMS: usize = 100;
const DESCRIPTION: usize = 300;
const CATCH_UP_SPACES: usize = 10;
const CATCH_UP_ROOMS: usize = 8;
const CATCH_UP_LATEST: usize = 5;
const CATCH_UP_TEXT: usize = 400;
const CATCH_UP_MAX_DAYS: i64 = 7;
pub(super) const QUERY_MAX: usize = 500;
pub(super) const SEARCH_MAX: usize = 10;
const SEARCH_DEFAULT: usize = 5;
const EXCERPT: usize = 400;
pub(super) const WAITING_DAYS_MAX: i64 = 14;
const WAITING_DAYS_DEFAULT: i64 = 7;
const WAITING_MAX: usize = 20;
const WAITING_TEXT: usize = 300;
/// Newest events of a Space scanned for catch-up and waiting items.
const SCAN: i64 = 2_000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The columns [`row_event`] reads.
const EVENT_COLUMNS: &str =
    "id, kind, actor, actor_id, text, mentions, audience, activity, created_at";

fn author(e: &Event) -> Value {
    match (
        &e.actor_id,
        e.activity.as_ref().and_then(|a| a.agent.as_ref()),
    ) {
        (Some(id), _) => json!({"id": id, "name": e.actor}),
        (None, Some(agent)) => json!({"name": agent.name, "agent_handle": agent.handle}),
        (None, None) => json!({"name": e.activity.as_ref().map_or("Metro", |a| a.name.as_str())}),
    }
}

fn author_name(e: &Event) -> String {
    author(e)["name"].as_str().unwrap_or_default().to_owned()
}

fn room_label(room: &Room) -> String {
    if room.is_reply_thread() {
        "a thread".to_owned()
    } else {
        format!("#{}", room.name)
    }
}

fn room_view(room: &Room) -> Value {
    let mut v =
        json!({"id": room.id, "name": if room.is_reply_thread() { "Thread" } else { &room.name }});
    if let Some(parent) = &room.parent_room_id {
        v["thread_of"] = json!(parent);
    }
    v
}

// ---- spaces_list ----

pub(super) async fn spaces_list(ctx: &Ctx) -> R {
    on_db(ctx, |db, grant| {
        let conn = db.lock();
        let spaces = match grant.spaces(&conn)? {
            Ok(s) => s,
            Err(d) => return Ok(Err(denied(d))),
        };
        let mut items = Vec::new();
        for s in spaces.iter().take(LIST_SPACES) {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {} FROM space_rooms WHERE space_id = ?1 AND anchor_event_id IS NULL
                 ORDER BY parent_room_id IS NOT NULL, created_at, rowid LIMIT ?2",
                crate::db::ROOM_COLUMNS
            ))?;
            let rooms: Vec<Value> = stmt
                .query_map(params![s.id, LIST_ROOMS as i64], crate::db::row_room)?
                .map(|r| r.map(|r| room_view(&r)))
                .collect::<Result<_, _>>()?;
            let agents: Vec<Value> = crate::space_agents::list_in(&conn, &s.id)?
                .iter()
                .map(|a| {
                    json!({"id": a.id, "handle": a.handle, "name": a.name,
                           "description": a.description, "default": a.is_default})
                })
                .collect();
            items.push(json!({"id": s.id, "name": s.name,
                              "description": clip(&s.description, DESCRIPTION, "…"),
                              "role": s.role, "rooms": rooms, "agents": agents}));
        }
        let names = spaces.iter().map(|s| s.name.clone()).collect();
        Ok(Ok(
            Out::list(items, json!({}), "Listed your Spaces").spaces(names)
        ))
    })
    .await
}

// ---- space_read ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    space_id: String,
    room_id: String,
    before_event_id: Option<i64>,
    limit: Option<usize>,
}

/// The Space and room, both checked now.
fn place(
    conn: &Connection,
    grant: &Grant,
    sid: &str,
    rid: &str,
) -> anyhow::Result<Result<(SpaceView, Room), String>> {
    let space = match grant.check(conn, sid)? {
        Ok(s) => s,
        Err(d) => return Ok(Err(denied(d))),
    };
    if crate::config::validate_id(rid).is_err() {
        return Ok(Err("no such room".to_owned()));
    }
    Ok(match Db::room_in(conn, sid, rid)? {
        Some(room) => Ok((space, room)),
        None => Err("no such room".to_owned()),
    })
}

pub(super) async fn space_read(ctx: &Ctx, a: Value) -> R {
    let a: ReadArgs = args(a)?;
    let limit = a.limit.unwrap_or(READ_DEFAULT);
    if !(1..=READ_MAX).contains(&limit) {
        return Err(format!("limit must be 1..={READ_MAX}"));
    }
    let before = a.before_event_id.unwrap_or(i64::MAX);
    on_db(ctx, move |db, grant| {
        let (space, room, mut events) = {
            let conn = db.lock();
            let (space, room) = match place(&conn, grant, &a.space_id, &a.room_id)? {
                Ok(p) => p,
                Err(e) => return Ok(Err(e)),
            };
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {EVENT_COLUMNS} FROM space_events
                 WHERE space_id = ?1 AND room_id = ?2 AND id < ?3
                   AND kind IN ('message', 'agent', 'thread')
                 ORDER BY id DESC LIMIT ?4"
            ))?;
            let mut events: Vec<Event> = stmt
                .query_map(params![space.id, room.id, before, limit as i64], row_event)?
                .collect::<Result<_, _>>()?;
            events.reverse();
            (space, room, events)
        };
        if room.parent_room_id.is_none() {
            crate::replies::hydrate(db, &space.id, &room.id, &mut events)?;
        }
        let items: Vec<Value> = events
            .iter()
            .map(|e| {
                let mut v = json!({"id": e.id, "kind": e.kind, "author": author(e),
                                   "text": clip(&e.text, READ_TEXT, "…"),
                                   "sent_at": e.created_at.and_then(rfc3339_utc)});
                if let Some(r) = e.meta.reply_to {
                    v["reply_to"] = json!(r);
                }
                if let Some(r) = &e.replies {
                    v["replies"] = json!({"room_id": r.room_id, "count": r.count});
                }
                v
            })
            .collect();
        let summary = format!("Read {} in {}", room_label(&room), space.name);
        Ok(Ok(Out::list(
            items,
            json!({"space_id": space.id, "room": room_view(&room)}),
            summary,
        )
        .spaces(vec![space.name])))
    })
    .await
}

// ---- spaces_catch_up ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatchUpArgs {
    since: Option<String>,
    space_ids: Option<Vec<String>>,
}

/// What counts as news for the person: others' messages and agent replies.
const NEWS: &str = "kind IN ('message', 'agent') AND (actor_id IS NULL OR actor_id <> :me)";
const MENTIONS_ME: &str = "CASE WHEN json_valid(mentions) THEN EXISTS (SELECT 1 FROM json_each(mentions) j
     WHERE json_extract(j.value, '$.kind') = 'user' AND json_extract(j.value, '$.id') = :me) ELSE 0 END";

pub(super) async fn catch_up(ctx: &Ctx, a: Value) -> R {
    let a: CatchUpArgs = args(a)?;
    let now = crate::db::now_ms();
    let floor = now - CATCH_UP_MAX_DAYS * DAY_MS;
    let since = match &a.since {
        Some(t) => chrono::DateTime::parse_from_rfc3339(t.trim())
            .map_err(|_| "since must be an RFC 3339 time".to_owned())?
            .timestamp_millis(),
        None => ctx.previous_message_at.unwrap_or(floor),
    }
    .max(floor);
    on_db(ctx, move |db, grant| {
        let conn = db.lock();
        let spaces = match grant.spaces_among(&conn, a.space_ids.as_deref())? {
            Ok(s) => s,
            Err(d) => return Ok(Err(denied(d))),
        };
        let mut items = Vec::new();
        let mut quiet = 0;
        for s in spaces.iter().take(CATCH_UP_SPACES) {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT room_id, COUNT(*), SUM({MENTIONS_ME}), MAX(id) FROM
                   (SELECT * FROM space_events WHERE space_id = :space ORDER BY id DESC LIMIT :scan)
                 WHERE created_at >= :since AND {NEWS}
                 GROUP BY room_id ORDER BY MAX(id) DESC LIMIT :rooms"
            ))?;
            let counts: Vec<(String, i64, i64)> = stmt
                .query_map(
                    rusqlite::named_params! {":space": s.id, ":scan": SCAN, ":since": since,
                    ":me": grant.user, ":rooms": CATCH_UP_ROOMS as i64},
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?
                .collect::<Result<_, _>>()?;
            if counts.is_empty() {
                quiet += 1;
                continue;
            }
            let mut rooms = Vec::new();
            for (rid, new_count, mentions) in counts {
                let Some(room) = Db::room_in(&conn, &s.id, &rid)? else {
                    continue;
                };
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT {EVENT_COLUMNS} FROM space_events
                     WHERE space_id = :space AND room_id = :room AND created_at >= :since AND {NEWS}
                     ORDER BY id DESC LIMIT :n"
                ))?;
                let mut latest: Vec<Value> = stmt
                    .query_map(
                        rusqlite::named_params! {":space": s.id, ":room": rid, ":since": since,
                        ":me": grant.user, ":n": CATCH_UP_LATEST as i64},
                        row_event,
                    )?
                    .map(|e| {
                        e.map(|e| {
                            json!({"event_id": e.id, "author": author_name(&e),
                                   "text": clip(&e.text, CATCH_UP_TEXT, "…"),
                                   "sent_at": e.created_at.and_then(rfc3339_utc)})
                        })
                    })
                    .collect::<Result<_, _>>()?;
                latest.reverse();
                let mut v = room_view(&room);
                v["new_count"] = json!(new_count);
                v["mentions_me"] = json!(mentions);
                v["latest"] = json!(latest);
                rooms.push(v);
            }
            items.push(json!({"space_id": s.id, "space_name": s.name, "rooms": rooms}));
        }
        let names = items
            .iter()
            .filter_map(|i| i["space_name"].as_str().map(str::to_owned))
            .collect();
        Ok(Ok(Out::list(
            items,
            json!({"since": rfc3339_utc(since), "quiet_spaces": quiet}),
            "Caught up on your Spaces",
        )
        .spaces(names)))
    })
    .await
}

// ---- spaces_search ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    space_ids: Option<Vec<String>>,
    limit: Option<usize>,
}

/// One hit before the final check: `(space, room, event, author, text, score)`.
type Hit = (String, String, i64, String, String, Option<f64>);

pub(super) async fn search(ctx: &Ctx, a: Value) -> R {
    let a: SearchArgs = args(a)?;
    let query = a.query.trim().to_owned();
    if query.is_empty() || query.chars().count() > QUERY_MAX {
        return Err(format!("query must be 1..={QUERY_MAX} characters"));
    }
    let limit = a.limit.unwrap_or(SEARCH_DEFAULT);
    if !(1..=SEARCH_MAX).contains(&limit) {
        return Err(format!("limit must be 1..={SEARCH_MAX}"));
    }
    let only = a.space_ids.clone();
    let spaces = on_db(ctx, move |db, grant| {
        Ok(grant
            .spaces_among(&db.lock(), only.as_deref())?
            .map_err(denied))
    })
    .await?;
    let sources: Vec<crate::search::Source> = spaces
        .iter()
        .take(crate::search::MAX_SPACES_PER_REQUEST)
        .map(|s| crate::search::Source::Space(s.id.clone()))
        .collect();
    let prefs = crate::model_routing::preferences(&ctx.state, &ctx.user)
        .await
        .map_err(internal)?;
    let who = Human {
        id: ctx.user.clone(),
        name: ctx.name.clone(),
    };
    let mut notice = None;
    let mut semantic = None;
    if prefs.embedding_id.is_some() && !sources.is_empty() {
        match crate::search::search_as(&ctx.state, &who, &query, &sources, limit).await {
            Ok(r) => semantic = Some(r),
            Err(crate::search::SearchError::Busy) => {
                return Err("search is busy; try again shortly".to_owned());
            }
            Err(_) => {
                notice = Some("Semantic search failed, so these are literal text matches.");
            }
        }
    }
    let (mode, reranked, hits): (&str, bool, Vec<Hit>) = match &semantic {
        Some(r) => (
            "semantic",
            // Only when System One ranked every result.
            r.ranking == crate::search::Ranking::Classifier,
            r.results
                .iter()
                .filter_map(|h| {
                    Some((
                        h.space_id.clone()?,
                        h.room_id.clone()?,
                        h.event_id,
                        h.actor.clone(),
                        h.text.clone(),
                        Some(h.score),
                    ))
                })
                .collect(),
        ),
        None => {
            let (ids, q, user) = (
                sources.clone(),
                clip(&query, crate::quick_search::MAX_QUERY_CHARS, ""),
                ctx.user.clone(),
            );
            let hits = on_db(ctx, move |db, _| {
                let conn = db.lock();
                let mut hits = Vec::new();
                for src in &ids {
                    let crate::search::Source::Space(sid) = src else {
                        continue;
                    };
                    let found = crate::quick_search::lookup(
                        &conn,
                        &user,
                        &q,
                        crate::quick_search::Scope::Space,
                        Some(sid),
                        crate::quick_search::Mode::Conversations,
                        None,
                    )?;
                    for m in found.into_iter().flat_map(|f| f.messages) {
                        hits.push((m.created_at, m));
                    }
                }
                hits.sort_by_key(|h| std::cmp::Reverse(h.0));
                Ok(Ok(hits
                    .into_iter()
                    .take(limit)
                    .filter_map(|(_, m)| {
                        Some((m.space_id?, m.room_id?, m.event_id, m.actor, m.text, None))
                    })
                    .collect::<Vec<Hit>>()))
            })
            .await?;
            ("text", false, hits)
        }
    };
    // The final check, on exactly what is returned.
    on_db(ctx, move |db, grant| {
        let conn = db.lock();
        let allowed = match grant.spaces(&conn)? {
            Ok(s) => s,
            Err(d) => return Ok(Err(denied(d))),
        };
        let mut items = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for (sid, rid, eid, actor, text, score) in hits {
            let Some(space) = allowed.iter().find(|s| s.id == sid) else {
                continue;
            };
            let sent: Option<i64> = conn
                .query_row(
                    "SELECT created_at FROM space_events WHERE space_id = ?1 AND room_id = ?2 AND id = ?3",
                    params![sid, rid, eid],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(sent) = sent else { continue };
            if !names.contains(&space.name) {
                names.push(space.name.clone());
            }
            let mut v = json!({"space_id": sid, "space_name": space.name, "room_id": rid,
                               "event_id": eid, "author": actor, "excerpt": clip(&text, EXCERPT, "…"),
                               "sent_at": crate::db::event_time(sent).and_then(rfc3339_utc)});
            if let Some(score) = score {
                v["score"] = json!(score);
            }
            items.push(v);
        }
        let mut extra = json!({"mode": mode, "reranked": reranked});
        if let Some(n) = notice {
            extra["notice"] = json!(n);
        }
        Ok(Ok(Out::list(items, extra, "Searched your Spaces").spaces(names)))
    })
    .await
}

// ---- waiting_on_me ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitingArgs {
    days: Option<i64>,
}

pub(super) async fn waiting_on_me(ctx: &Ctx, a: Value) -> R {
    let a: WaitingArgs = args(a)?;
    let days = a.days.unwrap_or(WAITING_DAYS_DEFAULT);
    if !(1..=WAITING_DAYS_MAX).contains(&days) {
        return Err(format!("days must be 1..={WAITING_DAYS_MAX}"));
    }
    let since = crate::db::now_ms() - days * DAY_MS;
    on_db(ctx, move |db, grant| {
        let conn = db.lock();
        let spaces = match grant.spaces(&conn)? {
            Ok(s) => s,
            Err(d) => return Ok(Err(denied(d))),
        };
        let mut found: Vec<(i64, Value, String)> = Vec::new();
        for s in &spaces {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT e.id, e.room_id, e.actor, e.text, e.created_at, ({MENTIONS_ME}) FROM
                   (SELECT * FROM space_events WHERE space_id = :space AND kind = 'message'
                    ORDER BY id DESC LIMIT :scan) e
                 WHERE e.created_at >= :since AND (e.actor_id IS NULL OR e.actor_id <> :me)
                   AND (({MENTIONS_ME})
                        OR (json_valid(e.activity) AND EXISTS (SELECT 1 FROM space_events r
                            WHERE r.space_id = e.space_id
                              AND r.id = json_extract(e.activity, '$.reply_to')
                              AND r.actor_id = :me)))
                   AND NOT EXISTS (SELECT 1 FROM space_events l
                       WHERE l.space_id = e.space_id AND l.room_id = e.room_id AND l.id > e.id
                         AND l.kind = 'message' AND l.actor_id = :me)
                 ORDER BY e.id DESC LIMIT :n"
            ))?;
            let rows = stmt.query_map(
                rusqlite::named_params! {":space": s.id, ":scan": SCAN, ":since": since,
                                          ":me": grant.user, ":n": WAITING_MAX as i64},
                |r| {
                    let mention: bool = r.get(5)?;
                    let at: i64 = r.get(4)?;
                    Ok((
                        at,
                        json!({"space_id": s.id, "space_name": s.name, "room_id": r.get::<_, String>(1)?,
                               "event_id": r.get::<_, i64>(0)?, "author": r.get::<_, String>(2)?,
                               "excerpt": clip(&r.get::<_, String>(3)?, WAITING_TEXT, "…"),
                               "sent_at": crate::db::event_time(at).and_then(rfc3339_utc),
                               "kind": if mention { "mention" } else { "reply" }}),
                        s.name.clone(),
                    ))
                },
            )?;
            for row in rows {
                found.push(row?);
            }
        }
        found.sort_by_key(|f| std::cmp::Reverse(f.0));
        found.truncate(WAITING_MAX);
        let mut names: Vec<String> = Vec::new();
        for (_, _, n) in &found {
            if !names.contains(n) {
                names.push(n.clone());
            }
        }
        let items = found.into_iter().map(|(_, v, _)| v).collect();
        Ok(Ok(Out::list(items, json!({"days": days}), "Checked what's waiting on you").spaces(names)))
    })
    .await
}
