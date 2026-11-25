//! A page's data, resolved on the server **as the viewing person**, and
//! what each of its buttons would do for them.
//!
//! Every source goes through the same membership checks as the endpoints a
//! person could call by hand, so a page never shows anyone more than they
//! could already read:
//!
//! - A **Space page** reads only its own Space, and only while the viewer
//!   is a member of it (rechecked here, not just when the page was opened).
//! - A **personal page** reads its owner's personal Chat, personal library
//!   and own schedules, and any Space it names only while they're a member
//!   of it now.
//!
//! A failed check answers that one source with
//! `{"ok": false, "error": "no_access"}` and a fixed message; other sources
//! still resolve, and nothing about the hidden data leaks into the error.
//! The HTTP endpoint doesn't send these values to a model or index them.
//! Space agents resolve a room-safe subset as their run's initiator.

use std::collections::HashMap;

use serde_json::{Value as Json, json};

use super::spec::{DataDecl, PageSpec, Target, action};
use super::store;
use crate::AppState;
use crate::db::{Db, Event, kind, rfc3339_utc};
use crate::library::{self, AssetKind};
use crate::orchestration::{RunInfo, RunStatus};

/// Longest text of one message or answer returned to a page.
const MAX_ITEM_TEXT: usize = 4_000;
/// Rows a source returns when the page sets no `limit`.
const DEFAULT_LIMIT: usize = 20;

/// Where a page's data is read from: the page (or draft) and its Space.
#[derive(Clone, Debug)]
pub struct Context {
    /// `None` for a draft of a page that doesn't exist yet.
    pub page_id: Option<String>,
    /// The Space of a Space page; `None` for a personal page.
    pub space_id: Option<String>,
    pub version: Option<i64>,
    /// A draft: its buttons don't run.
    pub draft: bool,
}

#[derive(Debug)]
enum Fail {
    NoAccess,
    Gone(&'static str),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for Fail {
    fn from(e: anyhow::Error) -> Self {
        Fail::Internal(e)
    }
}

impl Fail {
    fn json(self) -> Json {
        match self {
            Fail::NoAccess => json!({
                "ok": false, "error": "no_access",
                "message": "You don't have access to this."
            }),
            Fail::Gone(what) => json!({"ok": false, "error": "not_found", "message": what}),
            Fail::Internal(e) => {
                tracing::error!(error = %format!("{e:#}"), "resolving page data");
                json!({"ok": false, "error": "unavailable",
                       "message": "This couldn't be loaded; try again."})
            }
        }
    }
}

/// The Space a source of this page reads, checked for `viewer` now:
/// `Ok(Some(space))`, `Ok(None)` for the viewer's own personal data (or
/// all their Spaces, for sources that allow it), or no access.
fn space_of(db: &Db, ctx: &Context, t: &Target, viewer: &str) -> Result<Option<String>, Fail> {
    match (&ctx.space_id, &t.space_id) {
        (Some(own), None) if !t.chat => {
            if db.membership(own, viewer)?.is_none() {
                return Err(Fail::NoAccess);
            }
            Ok(Some(own.clone()))
        }
        // A Space page never reads another Space or anyone's Chat, even
        // when a stored spec says so.
        (Some(_), _) => Err(Fail::NoAccess),
        (None, Some(sid)) => {
            if db.membership(sid, viewer)?.is_none() {
                return Err(Fail::NoAccess);
            }
            Ok(Some(sid.clone()))
        }
        (None, None) => Ok(None),
    }
}

fn room_ok(db: &Db, viewer: &str, space: &str, room: Option<&str>) -> Result<(), Fail> {
    // Membership is in the same statement as the room.
    if let Some(r) = room
        && store::room_for(db, viewer, space, r)?.is_none()
    {
        return Err(Fail::Gone("The room this reads no longer exists."));
    }
    Ok(())
}

fn text(t: &str) -> String {
    crate::tools::truncate(t, MAX_ITEM_TEXT)
}

fn at(e: &Event) -> Json {
    json!(e.created_at.and_then(rfc3339_utc))
}

fn display(db: &Db, id: Option<&str>, fallback: &str) -> String {
    match id {
        Some(id) => db
            .profile(id)
            .map(|p| p.display_name)
            .unwrap_or_else(|_| fallback.to_owned()),
        None => fallback.to_owned(),
    }
}

fn message_item(db: &Db, e: &Event) -> Json {
    let agent = e.activity.as_ref().and_then(|a| a.agent.as_ref());
    json!({
        "id": e.id,
        "kind": if e.kind == kind::AGENT { "answer" } else { "message" },
        "author": match agent {
            Some(a) => a.name.clone(),
            None if e.kind == kind::AGENT => crate::db::Activity::AGENT_NAME.to_owned(),
            None => display(db, e.actor_id.as_deref(), &e.actor),
        },
        "author_id": e.actor_id,
        "agent": agent.map(|a| a.handle.clone()),
        "text": text(&e.text),
        "at": at(e),
    })
}

fn limit_of(t: &Target) -> usize {
    t.limit.map(|l| l as usize).unwrap_or(DEFAULT_LIMIT)
}

fn messages(db: &Db, ctx: &Context, t: &Target, viewer: &str) -> Result<Json, Fail> {
    let kinds: &[&str] = match t.kind.as_deref() {
        Some("messages") => &[kind::MESSAGE],
        Some("answers") => &[kind::AGENT],
        _ => &[kind::MESSAGE, kind::AGENT],
    };
    messages_of(db, ctx, t, viewer, kinds)
}

fn messages_of(
    db: &Db,
    ctx: &Context,
    t: &Target,
    viewer: &str,
    kinds: &[&str],
) -> Result<Json, Fail> {
    let mut events = match space_of(db, ctx, t, viewer)? {
        Some(sid) => {
            let room = t.room_id.as_deref().ok_or(Fail::NoAccess)?;
            room_ok(db, viewer, &sid, Some(room))?;
            store::room_events(
                db,
                viewer,
                &sid,
                room,
                kinds,
                None,
                t.agent_id.as_deref(),
                limit_of(t),
            )?
        }
        None if t.chat => store::chat_events(db, viewer, kinds, limit_of(t))?,
        None => return Err(Fail::NoAccess),
    };
    events.reverse();
    let items: Vec<Json> = events.iter().map(|e| message_item(db, e)).collect();
    Ok(json!({"room": t.room, "items": items}))
}

fn answer_json(db: &Db, e: &Event, room: Option<&str>) -> Json {
    let agent = e.activity.as_ref().and_then(|a| a.agent.as_ref());
    match e.kind.as_str() {
        kind::AGENT => json!({
            "status": "done",
            "text": text(&e.text),
            "at": at(e),
            "agent": agent.map(|a| a.name.clone()).unwrap_or_else(|| crate::db::Activity::AGENT_NAME.to_owned()),
            "agent_handle": agent.map(|a| a.handle.clone()),
            "room": room,
        }),
        kind::STATUS => {
            json!({"status": "cancelled", "error": text(&e.text), "at": at(e), "room": room})
        }
        _ => {
            let _ = db;
            json!({"status": "failed", "error": text(&e.text), "at": at(e), "room": room})
        }
    }
}

fn agent_answer(db: &Db, ctx: &Context, t: &Target, viewer: &str) -> Result<Json, Fail> {
    let space = space_of(db, ctx, t, viewer)?;
    if let Some(sid) = &space {
        room_ok(db, viewer, sid, t.room_id.as_deref())?;
    }
    if let Some(block) = &t.block {
        let Some(page) = &ctx.page_id else {
            return Ok(Json::Null);
        };
        // A personal page shows only its owner's runs; a Space page shows
        // the latest run any member started from the block, since its
        // answer is in a room every member reads.
        let only = if ctx.space_id.is_none() {
            Some(viewer)
        } else {
            None
        };
        let Some(run) = store::latest_run(db, viewer, page, block, only)? else {
            return Ok(Json::Null);
        };
        let latest = |kinds: &[&str]| match (&run.space_id, &run.room_id) {
            (Some(sid), Some(rid)) => {
                // The run's room must still be readable by this viewer.
                if Some(sid) != space.as_ref() && db.membership(sid, viewer)?.is_none() {
                    return Err(Fail::NoAccess);
                }
                Ok(
                    store::room_events(db, viewer, sid, rid, kinds, Some(&run.run_id), None, 1)?
                        .into_iter()
                        .next(),
                )
            }
            _ => Ok(store::last_page_event(
                db,
                page,
                viewer,
                kinds,
                Some(&run.run_id),
            )?),
        };
        // The worker publishes a reply before its terminal marker. Read the
        // marker first so a reply committed between the reads cannot look
        // like a completed run that ended without one.
        let marker = latest(&[kind::ACTIVITY])?;
        let room = t.room.as_deref();
        // The store excludes tool errors: those do not end the run.
        if let Some(e) = latest(&[kind::AGENT, kind::ERROR, kind::STATUS])? {
            return Ok(answer_json(db, &e, room));
        }
        // No answer: the run couldn't start, hasn't started yet, or ended
        // without a reply (its last lifecycle marker says how).
        if let Some(error) = &run.error {
            return Ok(json!({"status": "failed", "error": text(error),
                             "at": rfc3339_utc(run.created_at), "room": room}));
        }
        if run.run_id.is_empty() {
            return Ok(
                json!({"status": "starting", "at": rfc3339_utc(run.created_at), "room": room}),
            );
        }
        let phase = marker
            .as_ref()
            .and_then(|e| e.activity.as_ref())
            .and_then(|a| a.phase.clone());
        let at = marker
            .as_ref()
            .map(at)
            .unwrap_or(json!(rfc3339_utc(run.created_at)));
        return Ok(match phase.as_deref() {
            Some(crate::db::activity::COMPLETED) => {
                json!({"status": "done", "text": null, "at": at, "room": room})
            }
            Some(crate::db::activity::FAILED) => {
                json!({"status": "failed", "at": at, "room": room})
            }
            Some(crate::db::activity::CANCELLED) => {
                json!({"status": "cancelled", "at": at, "room": room})
            }
            Some(crate::db::activity::INTERRUPTED) => {
                json!({"status": "interrupted", "at": at, "room": room})
            }
            _ => json!({"status": "running", "at": at, "room": room}),
        });
    }
    let latest = match &space {
        Some(sid) => {
            let room = t.room_id.as_deref().ok_or(Fail::NoAccess)?;
            store::room_events(
                db,
                viewer,
                sid,
                room,
                &[kind::AGENT],
                None,
                t.agent_id.as_deref(),
                1,
            )?
        }
        None if t.chat => store::chat_events(db, viewer, &[kind::AGENT], 1)?,
        None => return Err(Fail::NoAccess),
    };
    Ok(latest
        .first()
        .map(|e| answer_json(db, e, t.room.as_deref()))
        .unwrap_or(Json::Null))
}

fn schedules(db: &Db, ctx: &Context, t: &Target, viewer: &str) -> Result<Json, Fail> {
    let space = space_of(db, ctx, t, viewer)?;
    // Only the viewer's own tasks, ever: on a Space page another member
    // sees theirs for this Space, never the page creator's.
    let tasks = crate::schedules::list_tasks(db, viewer)?;
    let items: Vec<Json> = tasks
        .iter()
        .filter(|task| match (&space, &task.target) {
            (Some(sid), Some((s, r))) => {
                s == sid && t.room_id.as_ref().is_none_or(|room| room == r)
            }
            (Some(_), None) => false,
            (None, _) => true,
        })
        .map(|task| {
            let v = task.view();
            json!({
                "id": task.id,
                "name": task.name,
                "enabled": task.enabled,
                "next_run": v["next_run_at"],
                "last_status": task.last_status,
                "last_run": v["last_run_at"],
                "room": task.target.as_ref().map(|(_, r)| r.clone()),
                "owned": true,
            })
        })
        .collect();
    Ok(json!({"items": items}))
}

fn kinds_of(group: Option<&str>) -> Option<Vec<AssetKind>> {
    Some(match group? {
        "document" => vec![
            AssetKind::Text,
            AssetKind::Markdown,
            AssetKind::Pdf,
            AssetKind::Docx,
        ],
        "image" => vec![AssetKind::Png, AssetKind::Jpeg],
        "audio" => vec![AssetKind::Wav, AssetKind::Mp3],
        "video" => vec![AssetKind::Mp4, AssetKind::Webm],
        _ => return None,
    })
}

fn like_pattern(q: Option<&str>) -> String {
    match q {
        None => "%".to_owned(),
        Some(q) => {
            let mut p = String::from("%");
            for c in q.chars() {
                if matches!(c, '%' | '_' | '\\') {
                    p.push('\\');
                }
                p.push(c);
            }
            p.push('%');
            p
        }
    }
}

fn library_files(db: &Db, ctx: &Context, t: &Target, viewer: &str) -> Result<Json, Fail> {
    let scope = match space_of(db, ctx, t, viewer)? {
        Some(sid) => library::Scope::Space(sid),
        None => library::Scope::Personal,
    };
    let kinds = kinds_of(t.kind.as_deref());
    let limit = t.limit.map(|l| l as usize).unwrap_or(DEFAULT_LIMIT).min(20);
    let assets = {
        let conn = db.lock();
        library::find_by_name(
            &conn,
            viewer,
            &scope,
            &like_pattern(t.query.as_deref()),
            kinds.as_deref(),
            &[],
            limit,
        )?
    };
    let items: Vec<Json> = assets
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "name": a.name,
                "kind": a.kind,
                "mime_type": a.mime_type,
                "size": a.size_bytes,
                "uploaded": rfc3339_utc(a.created_at),
                "space_id": a.space_id,
            })
        })
        .collect();
    Ok(json!({"items": items}))
}

fn agents(db: &Db, ctx: &Context, t: &Target, viewer: &str) -> Result<Json, Fail> {
    let Some(sid) = space_of(db, ctx, t, viewer)? else {
        return Err(Fail::NoAccess);
    };
    // Membership and the read under one lock: nothing changes in between.
    let list = {
        let conn = db.lock();
        if super::access::member_role(&conn, &sid, viewer)
            .map_err(anyhow::Error::from)?
            .is_none()
        {
            return Err(Fail::NoAccess);
        }
        crate::space_agents::list_in(&conn, &sid).map_err(anyhow::Error::from)?
    };
    let items: Vec<Json> = list
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "name": a.name,
                "handle": a.handle,
                "description": a.description,
                "is_default": a.is_default,
                "color": a.color,
                "initial": a.initial,
                "avatar_version": a.avatar_version,
                "space_id": sid,
            })
        })
        .collect();
    Ok(json!({"items": items}))
}

/// Runs of the Spaces this source may read, with the agent of each from
/// its `started` marker.
fn runs(db: &Db, ctx: &Context, t: &Target, viewer: &str, all: &[RunInfo]) -> Result<Json, Fail> {
    let allowed: Vec<String> = match space_of(db, ctx, t, viewer)? {
        Some(sid) => vec![sid],
        None if t.chat => return Err(Fail::NoAccess),
        None => db.list_spaces(viewer)?.into_iter().map(|s| s.id).collect(),
    };
    if let (Some(sid), Some(room)) = (allowed.first(), t.room_id.as_deref()) {
        room_ok(db, viewer, sid, Some(room))?;
    }
    // Filter in registry order (`RunRegistry::list` is newest first); limit
    // only afterwards.
    let mut matching = Vec::new();
    for r in all.iter() {
        if !allowed.contains(&r.space_id) {
            continue;
        }
        if t.room_id.as_ref().is_some_and(|room| *room != r.room_id) {
            continue;
        }
        // Newest first: the last of a run's (at most a few) lifecycle
        // markers is its `started` one; any marker carries the agent.
        let markers = store::room_events(
            db,
            viewer,
            &r.space_id,
            &r.room_id,
            &[kind::ACTIVITY],
            Some(&r.id),
            None,
            8,
        )?;
        let agent = markers
            .last()
            .and_then(|e| e.activity.as_ref())
            .and_then(|a| a.agent.clone());
        if let Some(want) = &t.agent_id
            && agent.as_ref().is_none_or(|a| a.id != *want)
        {
            continue;
        }
        matching.push((r, agent, markers.last().map(at)));
    }
    // Membership again after the read: a Space the viewer left meanwhile
    // contributes nothing, to the list or the count.
    let still: std::collections::HashSet<String> =
        db.list_spaces(viewer)?.into_iter().map(|s| s.id).collect();
    matching.retain(|(r, ..)| still.contains(&r.space_id));
    // Active runs are counted over every match, not just the listed ones.
    let active = matching
        .iter()
        .filter(|(r, ..)| r.status == RunStatus::Running)
        .count();
    let mut rooms: HashMap<(String, String), String> = HashMap::new();
    let mut items = Vec::new();
    for (r, agent, started) in matching.into_iter().take(limit_of(t)) {
        let key = (r.space_id.clone(), r.room_id.clone());
        if !rooms.contains_key(&key) {
            let name = store::room_for(db, viewer, &r.space_id, &r.room_id)?
                .map(|room| room.name)
                .unwrap_or_default();
            rooms.insert(key.clone(), name);
        }
        items.push(json!({
            "id": r.id,
            "status": r.status.as_str(),
            "agent": agent.as_ref().map(|a| a.name.clone()),
            "agent_handle": agent.as_ref().map(|a| a.handle.clone()),
            "room": rooms.get(&key),
            "room_id": r.room_id,
            "space_id": r.space_id,
            "started": started,
            "sender": display(db, Some(&r.actor), &r.actor),
            "sender_id": r.actor,
            "task": r.task,
        }));
    }
    Ok(json!({"active": active, "items": items}))
}

fn source(db: &Db, ctx: &Context, d: &DataDecl, viewer: &str, all_runs: &[RunInfo]) -> Json {
    let t = &d.params;
    let value = match d.source.as_str() {
        "runs" => runs(db, ctx, t, viewer, all_runs),
        "messages" => messages(db, ctx, t, viewer),
        "agentAnswer" => agent_answer(db, ctx, t, viewer),
        "schedules" => schedules(db, ctx, t, viewer),
        "library" => library_files(db, ctx, t, viewer),
        "agents" => agents(db, ctx, t, viewer),
        _ => Err(Fail::Gone("Unknown data source.")),
    };
    match value {
        Ok(v) => json!({"ok": true, "value": v}),
        Err(f) => f.json(),
    }
}

/// What a button would do for `viewer` now.
fn capability(
    db: &Db,
    ctx: &Context,
    block: &super::spec::Block,
    viewer: &str,
    vouched: Option<&store::Vouched>,
) -> Json {
    let Some(a) = &block.action else {
        return json!({"enabled": false});
    };
    let no = |reason: &str| json!({"enabled": false, "reason": reason});
    let runs_server = a.kind == action::RUN_AGENT || a.kind == action::RUN_SCHEDULE;
    if ctx.draft && runs_server {
        return no("Apply the draft to use its buttons.");
    }
    // No page version (a draft): nothing runs, nothing to confirm. A
    // failed lookup asks.
    let confirm = || -> bool {
        match (&ctx.page_id, ctx.version) {
            (Some(_), Some(_)) => vouched.is_none_or(|v| v.needs_confirmation(a)),
            _ => false,
        }
    };
    let result: Result<Json, Fail> = (|| match a.kind.as_str() {
        // The same destination policy the click claim applies.
        action::RUN_AGENT | action::RUN_SCHEDULE => {
            // The viewer's grant and the destination under one lock.
            let dest = {
                let conn = db.lock();
                if let Some(page) = &ctx.page_id
                    && store::load_in(&conn, page, viewer, store::Need::View)
                        .map_err(anyhow::Error::from)?
                        .is_err()
                {
                    return Err(Fail::NoAccess);
                }
                store::destination_in(&conn, ctx.space_id.as_deref(), a, viewer)
                    .map_err(anyhow::Error::from)?
            };
            Ok(match dest {
                Err(e) => no(e.reason()),
                Ok(store::Destination::Schedule { task, .. }) => {
                    json!({"enabled": true, "confirm": confirm(), "schedule_revision": task.revision})
                }
                Ok(_) => json!({"enabled": true, "confirm": confirm()}),
            })
        }
        action::OPEN_ROOM => match space_of(db, ctx, &a.target, viewer)? {
            Some(sid) => {
                let room = a.target.room_id.clone().unwrap_or_default();
                Ok(if store::room_for(db, viewer, &sid, &room)?.is_some() {
                    json!({"enabled": true, "space_id": sid, "room_id": room})
                } else {
                    no("This room no longer exists.")
                })
            }
            None if a.target.chat => Ok(json!({"enabled": true, "chat": true})),
            None => Err(Fail::NoAccess),
        },
        action::OPEN_PAGE => {
            let id = a.page_id.clone().unwrap_or_default();
            let visible = match store::get(db, &id)? {
                Some(p) if p.deleted_at.is_none() => {
                    super::access::rights(db, &p, viewer)?.is_some()
                }
                _ => false,
            };
            Ok(if visible {
                json!({"enabled": true, "page_id": id})
            } else {
                no("You can't open this page.")
            })
        }
        _ => Ok(json!({"enabled": true})),
    })();
    match result {
        Ok(v) => v,
        Err(Fail::NoAccess) => no("You don't have access to this."),
        Err(Fail::Gone(why)) => no(why),
        Err(Fail::Internal(e)) => {
            tracing::error!(error = %format!("{e:#}"), "checking a page action");
            no("This couldn't be checked; try again.")
        }
    }
}

/// Resolve `spec`'s data (all of it, or only `only`) and its buttons for
/// `viewer`. The spec always comes from storage, never from the client.
pub async fn resolve(
    state: &AppState,
    ctx: Context,
    spec: PageSpec,
    viewer: String,
    only: Option<Vec<String>>,
) -> anyhow::Result<Json> {
    resolve_with_actions(state, ctx, spec, viewer, only).await
}

/// Shared-room model data is a separate, positive allowlist. Never resolve
/// a viewer-only source here, even to count or redact its rows. Rights to
/// view something in the page UI aren't consent to share it with the room.
pub(crate) async fn resolve_room_sources(
    state: &AppState,
    ctx: Context,
    spec: PageSpec,
    viewer: String,
    only: Option<Vec<String>>,
) -> anyhow::Result<Json> {
    anyhow::ensure!(
        ctx.space_id.is_some() && !ctx.draft,
        "expected a live Space page"
    );
    state
        .db
        .call(move |db| {
            let mut data = serde_json::Map::new();
            for d in &spec.data {
                if only.as_ref().is_some_and(|o| !o.contains(&d.name)) {
                    continue;
                }
                data.insert(d.name.clone(), room_source(db, &ctx, d, &viewer));
            }
            Ok(json!({"data": data, "resolved_at": rfc3339_utc(crate::db::now_ms())}))
        })
        .await
}

/// Only transcript messages and shared Space inventory. The field lists
/// protect the model boundary even if a human-facing resolver gains fields.
fn room_source(db: &Db, ctx: &Context, d: &DataDecl, viewer: &str) -> Json {
    let t = &d.params;
    let (result, fields): (_, &[&str]) = match d.source.as_str() {
        "messages" if matches!(t.kind.as_deref(), None | Some("all" | "messages")) => (
            messages_of(db, ctx, t, viewer, &[kind::MESSAGE]),
            &["id", "kind", "author", "author_id", "text", "at"],
        ),
        "library" => (
            library_files(db, ctx, t, viewer),
            &[
                "id",
                "name",
                "kind",
                "mime_type",
                "size",
                "uploaded",
                "space_id",
            ],
        ),
        "agents" => (
            agents(db, ctx, t, viewer),
            &[
                "id",
                "name",
                "handle",
                "description",
                "is_default",
                "color",
                "initial",
                "avatar_version",
                "space_id",
            ],
        ),
        // Schedules are per-viewer. Run registry tasks and page-run origins
        // can include user/provider diagnostics and model/account metadata.
        // Answers are intentionally withheld too, including block runs and
        // failure text. Capabilities are never queried here.
        _ => {
            return json!({"ok": false, "error": "not_shared", "message": "This source is not available to shared room agents."});
        }
    };
    match result {
        Ok(value) => {
            let items: Vec<Json> = value["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|row| row.as_object())
                .map(|row| {
                    Json::Object(
                        fields
                            .iter()
                            .filter_map(|&k| {
                                row.get(k)
                                    .filter(|v| !v.is_array() && !v.is_object())
                                    .map(|v| (k.to_owned(), v.clone()))
                            })
                            .collect(),
                    )
                })
                .collect();
            let mut value = json!({"items": items});
            if d.source == "messages" {
                value["room"] = json!(t.room);
            }
            json!({"ok": true, "value": value})
        }
        Err(f) => f.json(),
    }
}

async fn resolve_with_actions(
    state: &AppState,
    ctx: Context,
    spec: PageSpec,
    viewer: String,
    only: Option<Vec<String>>,
) -> anyhow::Result<Json> {
    let all_runs = state.runs.list();
    state
        .db
        .call(move |db| {
            let mut data = serde_json::Map::new();
            for d in &spec.data {
                if only.as_ref().is_some_and(|o| !o.contains(&d.name)) {
                    continue;
                }
                data.insert(d.name.clone(), source(db, &ctx, d, &viewer, &all_runs));
            }
            // What the viewer vouched for, once per request.
            let vouched = match (&ctx.page_id, ctx.version) {
                (Some(page), Some(version)) if !ctx.draft && !spec.server_actions().is_empty() => {
                    match store::vouched(db, page, version, &viewer) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            tracing::error!(error = %format!("{e:#}"), "reading confirmations");
                            None
                        }
                    }
                }
                _ => None,
            };
            let mut actions = serde_json::Map::new();
            for b in spec.all_blocks() {
                if b.action.is_some()
                    && let Some(k) = &b.key
                {
                    actions.insert(
                        k.clone(),
                        capability(db, &ctx, b, &viewer, vouched.as_ref()),
                    );
                }
            }
            Ok(json!({
                "data": data,
                "actions": actions,
                "resolved_at": rfc3339_utc(crate::db::now_ms()),
            }))
        })
        .await
}
