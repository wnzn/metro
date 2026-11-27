//! `space_send`: post as the person, via Metro, through the one posting
//! path ([`crate::messages::post_room_message`]), optionally waiting for
//! the agent run it starts.
//!
//! - **Destination provenance.** The Space must be named (whole words,
//!   case-insensitive) or given by id in what the person wrote: this
//!   turn's message or their previous few Chat messages (a scheduled
//!   prompt's text is such a message). Space content read in this turn
//!   can't choose where to post.
//! - **Explicit recipients only.** `@` text adds nobody. The agent is
//!   `agent_id` or the Space's default agent, addressed by id, so
//!   classification never runs; people only by `mention_user_ids`.
//! - **Recorded in the person's Chat only** as a `space_post` event.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::tools::{Out, R, args, denied, internal, on_db};
use super::{Ctx, PostedAgent, SpacePost};
use crate::auth::Human;
use crate::db::{Activity, Addressed, Author, SpaceView, Via, actor, kind};
use crate::mentions::Mention;
use crate::messages::{Outgoing, post_room_message};
use crate::orchestration::RunStatus;
use crate::tools::truncate;

pub(super) const TEXT_MAX: usize = 4_000;
pub(super) const MENTIONS_MAX: usize = 8;
/// Longest wait for a delegated run (less when the Chat turn has less
/// time left).
pub const WAIT_MAX: Duration = Duration::from_secs(120);
/// Longest agent reply handed back, in bytes.
const REPLY_MAX: usize = 8 * 1024;

pub(super) const NOT_NAMED: &str = "the user didn't name this Space in their own messages; ask the \
user to confirm the Space by name";

/// Whether `text` holds `needle` as whole words (case-insensitive).
fn has_words(text: &str, needle: &str) -> bool {
    let (text, needle) = (text.to_lowercase(), needle.trim().to_lowercase());
    if needle.is_empty() {
        return false;
    }
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    text.match_indices(&needle).any(|(at, m)| {
        edge(text[..at].chars().next_back()) && edge(text[at + m.len()..].chars().next())
    })
}

/// Whether the person named `space` (by name or id) in `own` text.
pub(super) fn named_by_user(own: &[String], space: &SpaceView) -> bool {
    own.iter()
        .any(|t| has_words(t, &space.name) || has_words(t, &space.id))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    space_id: String,
    room_id: String,
    text: String,
    agent_id: Option<String>,
    #[serde(default)]
    mention_user_ids: Vec<String>,
    #[serde(default)]
    wait: bool,
    reply_to: Option<i64>,
}

pub(super) async fn space_send(ctx: &Ctx, a: Value) -> R {
    let a: SendArgs = args(a)?;
    let text = a.text.trim().to_owned();
    if text.is_empty() || text.chars().count() > TEXT_MAX {
        return Err(format!("text must be 1..={TEXT_MAX} characters"));
    }
    if a.mention_user_ids.len() > MENTIONS_MAX {
        return Err(format!("at most {MENTIONS_MAX} people can be mentioned"));
    }
    // The grant, provenance and the addressed agent, checked together.
    let (own, sid, rid, wanted, reply_to) = (
        ctx.own_text.clone(),
        a.space_id.clone(),
        a.room_id.clone(),
        a.agent_id.clone(),
        a.reply_to,
    );
    let (space, room, agent) = on_db(ctx, move |db, grant| {
        let conn = db.lock();
        let space = match grant.check(&conn, &sid)? {
            Ok(s) => s,
            Err(d) => return Ok(Err(denied(d))),
        };
        if !named_by_user(&own, &space) {
            return Ok(Err(NOT_NAMED.to_owned()));
        }
        let room = match crate::config::validate_id(&rid)
            .ok()
            .map(|_| crate::db::Db::room_in(&conn, &space.id, &rid))
            .transpose()?
            .flatten()
        {
            Some(r) => r,
            None => return Ok(Err("no such room".to_owned())),
        };
        let agents = crate::space_agents::list_in(&conn, &space.id)?;
        // A reply to an agent's message asks that agent, like a person's
        // reply; otherwise the default agent. Exactly one agent is asked.
        let replied = match (&wanted, reply_to) {
            (None, Some(id)) => {
                match crate::replies::reply_target_in(&conn, &space.id, &room.id, id)? {
                    Ok(target) => match crate::replies::reply_agent(&target, &agents) {
                        crate::replies::ReplyAgent::Agent(a) => Some(a.id.clone()),
                        _ => None,
                    },
                    Err(_) => None,
                }
            }
            _ => None,
        };
        let agent = match wanted.or(replied) {
            Some(id) => agents.into_iter().find(|a| a.id == id),
            None => agents.into_iter().find(|a| a.is_default),
        };
        match agent {
            Some(agent) => Ok(Ok((space, room, agent))),
            None => Ok(Err("no such agent in this Space".to_owned())),
        }
    })
    .await?;
    let mut mentions = vec![Mention::AgentId {
        agent_id: agent.id.clone(),
    }];
    mentions.extend(
        a.mention_user_ids
            .iter()
            .map(|id| Mention::User { id: id.clone() }),
    );
    let who = Human {
        id: ctx.user.clone(),
        name: ctx.name.clone(),
    };
    let posted = post_room_message(
        &ctx.state,
        &who,
        &space.id,
        &room.id,
        Outgoing {
            mentions,
            infer_text_mentions: false,
            reply_to: a.reply_to,
            // The agent above is the only one asked: a reply to another
            // agent's message doesn't add that agent too.
            reply_to_agent: false,
            via: Some(Via::Metro),
            ..Outgoing::new(text)
        },
    )
    .await
    .map_err(|e| e.message().to_owned())?;
    let run = posted.runs.first().cloned();
    let thread_name = room.parent_room_id.as_ref().map(|_| {
        if room.is_reply_thread() {
            "Thread".to_owned()
        } else {
            room.name.clone()
        }
    });
    let record = SpacePost {
        space_id: space.id.clone(),
        space_name: space.name.clone(),
        room_id: room.id.clone(),
        room_name: match &room.parent_room_id {
            Some(parent) => parent_name(ctx, &space.id, parent).await,
            None => room.name.clone(),
        },
        thread_name,
        parent_room_id: room.parent_room_id.clone(),
        event_id: posted.event.id,
        agent: PostedAgent {
            id: agent.id.clone(),
            handle: agent.handle.clone(),
            name: agent.name.clone(),
        },
        run_id: run.as_ref().map(|r| r.id.clone()),
    };
    record_post(ctx, &posted.event.text, record).await;
    let mut out = json!({"event_id": posted.event.id, "space_id": space.id, "room_id": room.id,
                         "agent": {"id": agent.id, "handle": agent.handle},
                         "run_id": run.as_ref().map(|r| &r.id)});
    out["status"] = json!(if run.is_some() { "sent" } else { "sent_no_run" });
    if run.is_none()
        && let Some(note) = &posted.routing_note
    {
        out["agent_not_started"] = json!(note);
    }
    let summary = format!("Sent a message to {}", space.name);
    let names = vec![space.name.clone()];
    let Some(run) = run.filter(|_| a.wait) else {
        return Ok(Out::new(out, summary).spaces(names));
    };
    // Delegate and wait, within this turn's and the server's wait slots.
    let slot = ctx.budget.wait_slot();
    let permit = ctx.state.meta_waits.clone().try_acquire_owned().ok();
    let (Some(_slot), Some(_permit)) = (slot, permit) else {
        out["run_status"] = json!("running");
        out["note"] = json!(
            "Metro is waiting on too many runs to wait for this one; check it later with runs_list"
        );
        return Ok(Out::new(out, summary).spaces(names));
    };
    // At most two minutes, and never so long that this turn can't answer.
    let left = ctx
        .deadline
        .saturating_duration_since(tokio::time::Instant::now());
    let timeout = WAIT_MAX.min(left.saturating_sub(left / 4));
    let done = ctx.state.runs.done(&run.id);
    if let Some(mut done) = done {
        let _ = tokio::time::timeout(timeout, done.wait_for(|d| *d)).await;
    }
    let status = ctx
        .state
        .runs
        .get(&run.id)
        .map_or(RunStatus::Failed, |r| r.status);
    let settled = ctx.state.runs.done(&run.id).is_none_or(|d| *d.borrow());
    if !settled {
        out["run_status"] = json!("running");
        out["note"] = json!("the agent is still working; check it later with runs_list");
        return Ok(Out::new(out, summary).spaces(names));
    }
    // Read the run's answer as the person, now.
    let (sid, rid, run_id) = (space.id.clone(), room.id.clone(), run.id.clone());
    let reply: Option<String> = on_db(ctx, move |db, grant| {
        let conn = db.lock();
        if let Err(d) = grant.check(&conn, &sid)? {
            return Ok(Err(denied(d)));
        }
        let mut stmt = conn.prepare_cached(
            "SELECT text FROM space_events
             WHERE space_id = ?1 AND room_id = ?2 AND kind IN ('agent', 'error')
               AND json_valid(activity) AND json_extract(activity, '$.id') = ?3
             ORDER BY id",
        )?;
        let texts: Vec<String> = stmt
            .query_map(rusqlite::params![sid, rid, run_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(Ok((!texts.is_empty()).then(|| texts.join("\n\n"))))
    })
    .await?;
    out["run_status"] = json!(status);
    if let Some(reply) = reply {
        out["truncated"] = json!(reply.len() > REPLY_MAX);
        out["reply"] = json!(truncate(&reply, REPLY_MAX));
    }
    Ok(Out::new(out, summary).spaces(names))
}

async fn parent_name(ctx: &Ctx, space: &str, room: &str) -> String {
    let (s, r) = (space.to_owned(), room.to_owned());
    ctx.state
        .db
        .call(move |db| db.get_room(&s, &r))
        .await
        .ok()
        .flatten()
        .map(|r| r.name)
        .unwrap_or_default()
}

/// The `space_post` event in the person's own Chat.
async fn record_post(ctx: &Ctx, text: &str, post: SpacePost) {
    let mut activity: Activity = ctx.turn.clone();
    activity.space_post = Some(post);
    let (user, thread, text) = (ctx.user.clone(), ctx.thread.clone(), text.to_owned());
    if let Err(e) = ctx
        .state
        .db
        .call(move |db| {
            db.append_personal_with(
                &user,
                thread.as_deref(),
                kind::SPACE_POST,
                &Author::server(actor::AGENT),
                &text,
                &Addressed::room(),
                Some(&activity),
            )
        })
        .await
    {
        internal(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Role;

    fn space(name: &str) -> SpaceView {
        SpaceView {
            id: "s-abc123".into(),
            name: name.into(),
            description: String::new(),
            role: Role::Member,
            runner_paired: false,
        }
    }

    #[test]
    fn a_destination_must_be_named_in_whole_words() {
        let own = |t: &str| vec![t.to_owned()];
        assert!(named_by_user(
            &own("post this in design please"),
            &space("Design")
        ));
        assert!(named_by_user(
            &own("Ask DESIGN TEAM."),
            &space("Design team")
        ));
        assert!(named_by_user(&own("use s-abc123"), &space("Other")));
        assert!(!named_by_user(&own("redesign it"), &space("Design")));
        assert!(!named_by_user(&own("designers"), &space("Design")));
        assert!(!named_by_user(&own("post it there"), &space("Design")));
        assert!(!named_by_user(&[], &space("Design")));
    }
}
