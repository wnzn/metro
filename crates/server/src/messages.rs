//! The one path that posts a person's message into a Space room or thread:
//! `POST /v1/spaces/{sid}/rooms/{rid}/messages`, and Metro posting from the
//! person's Chat (`via: "metro"`). Both get the same checks, the same
//! transactional append and the same agent dispatch.
//!
//! Every check acts as `who` and is made again inside the write
//! transaction: membership, the room, each concrete recipient, the files
//! and the `reply_to` target. Nothing is stored unless all pass, and
//! nothing ever falls back to another room, recipient or connection.

use serde::Serialize;

use crate::AppState;
use crate::api::{
    ApiError, ApiResult, agent_needs_text, check_choice, check_message, db, member_of,
    no_attachment, room_of,
};
use crate::attachments::{self, Append, Place};
use crate::auth::Human;
use crate::db::{Addressed, Author, Event, MessageMeta, Via, actor, kind};
use crate::mentions::{self, AddressedBy as StoredBy, Mention, MentionSpan, ValidatedSpans};
use crate::model_routing::{self as routing, Addressing, AddressingNote, RoutingSettings};
use crate::orchestration::RunInfo;
use crate::replies::{ReplyAgent, ReplyError};
use crate::space_agents::SpaceAgent;

/// A message to post, as `POST .../messages` takes it. Build it with
/// [`Outgoing::new`] and set what applies.
#[derive(Clone, Debug)]
pub struct Outgoing {
    pub text: String,
    /// Ask the Space's agent: the generic `@agent`, or `agent_id`.
    pub invoke_agent: bool,
    pub agent_id: Option<String>,
    /// Explicit recipients; spans index into this list.
    pub mentions: Vec<Mention>,
    /// `false` when the caller resolved addressing itself: `@` text in
    /// `text` then addresses nobody.
    pub infer_text_mentions: bool,
    pub spans: Vec<MentionSpan>,
    /// The model chosen for this turn; never falls back.
    pub provider_connection_id: Option<String>,
    pub model_id: Option<String>,
    pub reasoning_effort: Option<String>,
    /// The sender's own library files of this Space.
    pub asset_ids: Vec<String>,
    /// An event of this thread, or a reply thread's anchor.
    pub reply_to: Option<i64>,
    /// With `reply_to` an agent's reply: ask that agent (the default).
    pub reply_to_agent: bool,
    /// `Some(Via::Metro)` when the person's Chat posts for them.
    pub via: Option<Via>,
    /// Store the message first and make any System One decision (opt-in
    /// classification, the generic agent's pick, use-case routing) and the
    /// runs it starts afterwards, in the background. The sender collects
    /// the sender-only notes with [`crate::decisions`].
    pub defer_decisions: bool,
}

impl Outgoing {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            invoke_agent: false,
            agent_id: None,
            mentions: Vec::new(),
            infer_text_mentions: true,
            spans: Vec::new(),
            provider_connection_id: None,
            model_id: None,
            reasoning_effort: None,
            asset_ids: Vec::new(),
            reply_to: None,
            reply_to_agent: true,
            via: None,
            defer_decisions: false,
        }
    }
}

/// A sender-only note on the POST response; never stored, never an event.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SenderNote {
    Addressing(AddressingNote),
    Reply(ReplyNote),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyNote {
    /// The agent whose reply this answers is no longer in the Space, so it
    /// wasn't asked.
    ReplyAgentUnavailable { message: String },
}

/// What [`post_room_message`] did.
#[derive(Debug)]
pub struct Posted {
    pub event: Event,
    pub addressing_note: Option<SenderNote>,
    /// Why use-case routing or the turn's model failed, for the sender.
    pub routing_note: Option<AddressingNote>,
    /// The agent runs the message started.
    pub runs: Vec<RunInfo>,
    /// The decisions and runs were deferred ([`Outgoing::defer_decisions`]):
    /// the notes and runs above are empty, and the sender collects the notes
    /// from `POST .../messages/{eid}/decisions`.
    pub decisions_pending: bool,
}

/// Post `msg` as `who` in room or thread `rid` of Space `sid` and start
/// the agent runs it addresses (see [`crate::agent_dispatch`]). 404 for a
/// Space or room `who` can't see; 400 for a refused message.
pub async fn post_room_message(
    state: &AppState,
    who: &Human,
    sid: &str,
    rid: &str,
    msg: Outgoing,
) -> ApiResult<Posted> {
    let space = member_of(state, sid, who).await?;
    let room = room_of(state, &space, rid).await?;
    let spans = ValidatedSpans::new(&msg.text, msg.mentions.len(), msg.spans)
        .map_err(ApiError::bad_request)?;
    let text = check_message(state, &who.id, Some(&space.id), &msg.text, &msg.asset_ids).await?;
    let (space_id, room_id, reply_to) = (space.id.clone(), room.id.clone(), msg.reply_to);
    let (member_ids, agents, target) = db(state, move |db| {
        let members: Vec<String> = db
            .list_members(&space_id)?
            .into_iter()
            .map(|m| m.user_id)
            .collect();
        let target = match reply_to {
            Some(id) => Some(crate::replies::reply_target_in(
                &db.lock(),
                &space_id,
                &room_id,
                id,
            )?),
            None => None,
        };
        Ok((members, db.space_agents(&space_id)?, target))
    })
    .await?;
    let target = match target.transpose() {
        Ok(target) => target,
        Err(ReplyError::TopLevel) => return Err(ApiError::bad_request("reply in a thread")),
        Err(ReplyError::NotFound) => {
            return Err(ApiError::bad_request(
                "reply_to must be a message in this thread or the message it replies to",
            ));
        }
    };
    let agent_handles: Vec<(String, String)> = agents
        .iter()
        .map(|a| (a.id.clone(), a.handle.clone()))
        .collect();
    let (resolved, spans) = mentions::resolve_with_spans(
        msg.infer_text_mentions.then_some(text.as_str()),
        &msg.mentions,
        msg.invoke_agent,
        msg.agent_id.as_deref(),
        &member_ids,
        &agent_handles,
        spans,
    )
    .map_err(ApiError::bad_request)?;
    // A reply to an agent's reply asks that agent, unless the sender opted
    // out. The agent doesn't read attachments, so a reply needs words.
    let mut reply_note = None;
    let reply_agent = match target.as_ref() {
        Some(t) if msg.reply_to_agent && !text.is_empty() => {
            match crate::replies::reply_agent(t, &agents) {
                ReplyAgent::Agent(a) => Some(a.clone()),
                ReplyAgent::Unavailable => {
                    reply_note = Some(SenderNote::Reply(ReplyNote::ReplyAgentUnavailable {
                        message:
                            "The agent you replied to is no longer in this Space, so no agent \
                              was asked to reply."
                                .to_owned(),
                    }));
                    None
                }
                ReplyAgent::None => None,
            }
        }
        _ => None,
    };
    let addressing = match Addressing::of(&resolved) {
        _ if reply_agent.is_some() => Addressing::Agent,
        // The reply was meant for an agent that's gone: nothing else is
        // asked to stand in for it.
        Addressing::Unaddressed if reply_note.is_some() => Addressing::PeopleOnly,
        a => a,
    };
    agent_needs_text(&text, addressing)?;
    let chosen = check_choice(
        state,
        who,
        Some(&space.id),
        msg.provider_connection_id,
        msg.model_id,
        msg.reasoning_effort,
        addressing,
    )
    .await?;
    // People-only messages never reach System One or the preferences.
    let (settings, prefs) = if addressing == Addressing::PeopleOnly {
        (
            RoutingSettings::default(),
            crate::space_prefs::SpacePrefs::default(),
        )
    } else {
        (
            routing::preferences(state, &who.id)
                .await
                .map_err(ApiError::internal)?,
            crate::space_prefs::load(state, &space.id, &who.id)
                .await
                .map_err(ApiError::internal)?,
        )
    };
    let deferred = msg.defer_decisions
        && needs_system_one(
            addressing, &text, &resolved, &settings, &prefs, &agents, &chosen,
        );
    let plan = if deferred {
        crate::agent_dispatch::Plan::default()
    } else {
        crate::agent_dispatch::plan(
            state,
            &who.id,
            &space.id,
            &room.id,
            &text,
            &resolved,
            addressing,
            &settings,
            &prefs,
            &agents,
            reply_agent.as_ref(),
        )
        .await
    };
    // Only an unaddressed message can acquire a recipient here. Explicit
    // spans keep their canonical indices even if @agent's target is picked
    // by the classifier (that choice is run activity, not an edit to To:).
    // A deferred decision never edits the stored message.
    let mut addressed = plan
        .classified
        .clone()
        .unwrap_or_else(|| stored(&resolved, reply_agent.as_ref(), msg.via));
    if !msg.infer_text_mentions || !spans.is_empty() {
        addressed.spans = Some(spans);
    }
    let author = who.author();
    let place = Place::Space {
        space_id: space.id.clone(),
        room_id: room.id.clone(),
    };
    let meta = MessageMeta {
        reply_to: msg.reply_to,
        via: msg.via,
        ..MessageMeta::default()
    };
    let (writer, ids, stored_text) = (author.clone(), msg.asset_ids, text.clone());
    // Include targets derived from classification, @agent or a reply as
    // well as stored explicit recipients. A refusal reveals no classifier
    // result.
    let mut recipients = resolved.clone();
    recipients.extend(plan.targets.iter().map(|target| Mention::AgentId {
        agent_id: target.agent.id.clone(),
    }));
    let event = match db(state, move |db| {
        attachments::append_message(
            db,
            &place,
            &writer,
            &stored_text,
            &addressed,
            &ids,
            &recipients,
            &meta,
        )
    })
    .await?
    {
        Append::Posted(event) => *event,
        Append::NoRoom => return Err(ApiError::not_found("no such room")),
        Append::NoRecipient => return Err(ApiError::bad_request("unknown recipient")),
        Append::NoAsset => return Err(no_attachment()),
        Append::NoReplyTarget => {
            return Err(ApiError::bad_request(
                "reply_to must be a message in this thread or the message it replies to",
            ));
        }
    };
    if deferred {
        let decided = crate::decisions::Key::new(&who.id, &space.id, &room.id, event.id);
        let done = state.decisions.register(decided, event.clone());
        let job = Deferred {
            state: state.clone(),
            user: who.id.clone(),
            space: space.id.clone(),
            room,
            author,
            event: event.clone(),
            text,
            resolved,
            addressing,
            settings,
            prefs,
            reply_agent,
            chosen,
        };
        tokio::spawn(async move {
            let outcome = job.run().await;
            // Nobody may be waiting; the registry keeps it briefly.
            let _ = done.send(Some(outcome.into()));
        });
        return Ok(Posted {
            event,
            addressing_note: reply_note,
            routing_note: None,
            runs: Vec::new(),
            decisions_pending: true,
        });
    }
    let started = finish(
        state, &space.id, &room, &author, &event, plan, chosen, &settings, &prefs,
    )
    .await;
    Ok(Posted {
        event,
        addressing_note: reply_note.or(started.addressing_note),
        routing_note: started.routing_note,
        runs: started.runs,
        decisions_pending: false,
    })
}

/// Whether a message would wait on a System One call before its runs
/// start: opt-in classification of unaddressed text, the classifier's pick
/// for the generic `@agent`, or use-case routing. People-only messages never.
fn needs_system_one(
    addressing: Addressing,
    text: &str,
    resolved: &[Mention],
    settings: &RoutingSettings,
    prefs: &crate::space_prefs::SpacePrefs,
    agents: &[SpaceAgent],
    chosen: &Option<crate::model_routing::Route>,
) -> bool {
    match addressing {
        Addressing::PeopleOnly => false,
        Addressing::Unaddressed => !text.is_empty() && prefs.classify_unaddressed,
        Addressing::Agent => {
            let picks = resolved.contains(&Mention::Agent)
                && prefs.classify_unaddressed
                && settings.classifier_id.is_some()
                && agents.len() >= 2;
            let routes = chosen.is_none()
                && settings.enabled
                && prefs.routing
                && prefs.default_model.is_none();
            picks || routes
        }
    }
}

/// The sender-only outcome of a message's decisions and runs.
pub(crate) struct Finished {
    pub addressing_note: Option<SenderNote>,
    pub routing_note: Option<AddressingNote>,
    pub runs: Vec<RunInfo>,
    /// The classifier picked an agent for an unaddressed message.
    pub classified: bool,
}

/// Post the plan's room errors, then start its runs answering `event`.
#[allow(clippy::too_many_arguments)]
async fn finish(
    state: &AppState,
    space: &str,
    room: &crate::db::Room,
    author: &Author,
    event: &Event,
    plan: crate::agent_dispatch::Plan,
    chosen: Option<crate::model_routing::Route>,
    settings: &RoutingSettings,
    prefs: &crate::space_prefs::SpacePrefs,
) -> Finished {
    for reason in &plan.errors {
        state
            .append(
                space,
                &room.id,
                kind::ERROR,
                &Author::server(actor::SYSTEM),
                reason,
            )
            .await;
    }
    let classified = plan.classified.is_some()
        || plan
            .targets
            .iter()
            .any(|t| t.addressed_by == crate::model_routing::AddressedBy::Classifier);
    // Mentioning only people never starts agent work.
    let started = crate::agent_dispatch::start_runs(
        state,
        space,
        room,
        author,
        event,
        plan.targets,
        chosen,
        settings,
        prefs,
    )
    .await;
    Finished {
        addressing_note: plan.addressing_note.map(SenderNote::Addressing),
        routing_note: started.note,
        runs: started.runs,
        classified,
    }
}

/// A message's decisions, made after it was stored.
struct Deferred {
    state: AppState,
    user: String,
    space: String,
    room: crate::db::Room,
    author: Author,
    event: Event,
    text: String,
    resolved: Vec<Mention>,
    addressing: Addressing,
    settings: RoutingSettings,
    prefs: crate::space_prefs::SpacePrefs,
    reply_agent: Option<SpaceAgent>,
    chosen: Option<crate::model_routing::Route>,
}

impl Deferred {
    async fn run(self) -> Finished {
        let nothing = || Finished {
            addressing_note: None,
            routing_note: None,
            runs: Vec::new(),
            classified: false,
        };
        // The Space's agents as they are now: one deleted since the post is
        // never picked or started. A message deleted meanwhile starts nothing.
        let (space, room, eid) = (self.space.clone(), self.room.id.clone(), self.event.id);
        let agents = match self
            .state
            .db
            .call(move |db| {
                let present = crate::replies::event_in(&db.lock(), &space, &room, eid)?
                    .is_some_and(|e| e.kind == kind::MESSAGE);
                present.then(|| db.space_agents(&space)).transpose()
            })
            .await
        {
            Ok(Some(agents)) => agents,
            Ok(None) => return nothing(),
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "loading agents for a deferred decision");
                return nothing();
            }
        };
        let reply_agent = self
            .reply_agent
            .as_ref()
            .and_then(|r| agents.iter().find(|a| a.id == r.id).cloned());
        let plan = crate::agent_dispatch::plan(
            &self.state,
            &self.user,
            &self.space,
            &self.room.id,
            &self.text,
            &self.resolved,
            self.addressing,
            &self.settings,
            &self.prefs,
            &agents,
            reply_agent.as_ref(),
        )
        .await;
        finish(
            &self.state,
            &self.space,
            &self.room,
            &self.author,
            &self.event,
            plan,
            self.chosen,
            &self.settings,
            &self.prefs,
        )
        .await
    }
}

/// The stored recipients: `resolved`, then the replied agent unless it was
/// named too. Agents Metro addressed and the replied agent say so.
fn stored(resolved: &[Mention], reply: Option<&SpaceAgent>, via: Option<Via>) -> Addressed {
    let mut all = resolved.to_vec();
    let mut tagged: Vec<(&str, StoredBy)> = Vec::new();
    if via == Some(Via::Metro) {
        for m in resolved {
            if let Mention::AgentId { agent_id } = m {
                tagged.push((agent_id, StoredBy::Metro));
            }
        }
    }
    if let Some(agent) = reply {
        let named = resolved
            .iter()
            .any(|m| matches!(m, Mention::AgentId { agent_id } if *agent_id == agent.id));
        if !named {
            all.push(Mention::AgentId {
                agent_id: agent.id.clone(),
            });
            tagged.push((&agent.id, StoredBy::Reply));
        }
    }
    Addressed {
        mentions: mentions::stored(&all, &tagged),
        audience: mentions::audience(&all).to_owned(),
        spans: None,
    }
}
