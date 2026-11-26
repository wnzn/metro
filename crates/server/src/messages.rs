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
    /// The classifier chose an agent for this unaddressed message (it is
    /// never added to the stored recipients).
    pub classified: bool,
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
    // An explicit model for this turn, as a pending decision stores it.
    let choice = msg.provider_connection_id.as_ref().map(|id| {
        serde_json::json!({
            "provider_connection_id": id,
            "model_id": msg.model_id,
            "reasoning_effort": msg.reasoning_effort,
        })
    });
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
            None,
        )
        .await
    };
    // The classifier's output is never stored in a message's recipients, in
    // any path: its pick is run activity only. Explicit spans keep their
    // canonical indices.
    let mut addressed = stored(&resolved, reply_agent.as_ref(), msg.via);
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
    // Durable with the message: a restart makes the decision again.
    let pending = deferred.then(|| crate::decisions::NewPending {
        scope: crate::decisions::Scope::Space,
        user_id: who.id.clone(),
        place: Some((space.id.clone(), room.id.clone())),
        payload: serde_json::json!({
            "resolved": resolved,
            "reply_agent": reply_agent.as_ref().map(|a| a.id.clone()),
            "choice": choice,
        }),
        claimed_by: state.decisions.epoch().to_owned(),
        turn_id: None,
    });
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
            pending.as_ref(),
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
        let finisher = state.decisions.register(decided, event.clone());
        let job = Deferred {
            state: state.clone(),
            user: who.id.clone(),
            space: space.id.clone(),
            room,
            author,
            event: event.clone(),
            resolved,
            addressing,
            settings,
            prefs,
            reply_agent: reply_agent.map(|a| a.id),
            chosen: chosen.map(Box::new),
            choice,
        };
        let watch = crate::decisions::Watch::space(state, &space.id, &job.room.id, event.id);
        tokio::spawn(async move {
            let outcome = job.run(watch).await;
            // Nobody may be waiting; the registry keeps it briefly.
            finisher.finish(outcome.into());
        });
        return Ok(Posted {
            event,
            addressing_note: reply_note,
            routing_note: None,
            runs: Vec::new(),
            decisions_pending: true,
            classified: false,
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
        classified: started.classified,
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
    resolved: Vec<Mention>,
    addressing: Addressing,
    settings: RoutingSettings,
    prefs: crate::space_prefs::SpacePrefs,
    /// The id of the agent whose reply this message answers.
    reply_agent: Option<String>,
    /// The model chosen for the turn, checked (`None` after a restart:
    /// rechecked from `choice`).
    chosen: Option<Box<crate::model_routing::Route>>,
    choice: Option<serde_json::Value>,
}

/// The live trigger: the message as stored now, `None` once deleted.
async fn live_message(state: &AppState, space: &str, room: &str, id: i64) -> Option<Event> {
    let (s, r) = (space.to_owned(), room.to_owned());
    match state
        .db
        .call(move |db| Ok(crate::replies::event_in(&db.lock(), &s, &r, id)?))
        .await
    {
        Ok(event) => event.filter(|e| e.kind == kind::MESSAGE),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "reading a pending decision's message");
            None
        }
    }
}

/// The model chosen for a turn, checked again (after a restart).
async fn recheck_choice(
    state: &AppState,
    user: &str,
    name: &str,
    space: &str,
    choice: &serde_json::Value,
) -> Result<Option<crate::model_routing::Route>, String> {
    let who = Human {
        id: user.to_owned(),
        name: name.to_owned(),
    };
    let text = |k: &str| choice.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    check_choice(
        state,
        &who,
        Some(space),
        text("provider_connection_id"),
        text("model_id"),
        text("reasoning_effort"),
        Addressing::Agent,
    )
    .await
    .map_err(|e| e.message().to_owned())
}

impl Deferred {
    /// Make the decision and start its runs. `watch` notes and drops the
    /// work if this ends on an error (or panics) rather than settling it.
    async fn run(self, watch: crate::decisions::Watch) -> Finished {
        let nothing = || Finished {
            addressing_note: None,
            routing_note: None,
            runs: Vec::new(),
            classified: false,
        };
        // A System One call waits for a slot (none is dropped), held only
        // for the decision itself: never while runs are started.
        let slot = match plan_calls_system_one(
            self.addressing,
            &self.event.text,
            &self.resolved,
            &self.settings,
            &self.prefs,
        ) {
            true => Some(self.state.decisions.slot(&self.user).await),
            false => None,
        };
        // The message as it is now, after the wait: one deleted meanwhile
        // goes to no model. And the Space's agents as they are now.
        let Some(event) =
            live_message(&self.state, &self.space, &self.room.id, self.event.id).await
        else {
            crate::decisions::forget(&self.state, crate::decisions::Scope::Space, self.event.id)
                .await;
            watch.done();
            return nothing();
        };
        let space = self.space.clone();
        let agents = match self.state.db.call(move |db| db.space_agents(&space)).await {
            Ok(agents) => agents,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "loading agents for a deferred decision");
                return nothing();
            }
        };
        let reply_agent = self
            .reply_agent
            .as_ref()
            .and_then(|r| agents.iter().find(|a| a.id == *r).cloned());
        let plan = crate::agent_dispatch::plan(
            &self.state,
            &self.user,
            &self.space,
            &self.room.id,
            &event.text,
            &self.resolved,
            self.addressing,
            &self.settings,
            &self.prefs,
            &agents,
            reply_agent.as_ref(),
            Some(event.id),
        )
        .await;
        let classified = plan.classified.is_some();
        let addressing_note = plan.addressing_note.clone().map(SenderNote::Addressing);
        // The decision is made: its planned runs replace it, durably, with
        // the room errors it posts. Lost claim or deleted message: stop.
        let planned: Vec<_> = plan
            .targets
            .iter()
            .map(|t| crate::decisions::PlannedTarget {
                agent_id: t.agent.id.clone(),
                pick: t.pick.clone(),
                addressed_by: t.addressed_by,
            })
            .collect();
        let (epoch, sid, rid, user, eid, choice, errors) = (
            self.state.decisions.epoch().to_owned(),
            self.space.clone(),
            self.room.id.clone(),
            self.user.clone(),
            event.id,
            self.choice.clone(),
            plan.errors.clone(),
        );
        let persisted = self
            .state
            .db
            .call(move |db| {
                crate::decisions::plan_in(
                    db,
                    &epoch,
                    &sid,
                    &rid,
                    &user,
                    eid,
                    &planned,
                    choice.as_ref(),
                    &errors,
                )
            })
            .await;
        drop(slot);
        match persisted {
            Ok(true) => {}
            // Another process holds it, or the message is gone.
            Ok(false) => {
                watch.done();
                return nothing();
            }
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "storing a decision's runs");
                return nothing();
            }
        }
        let chosen = self.chosen.map(|r| *r);
        let started = dispatch_planned(
            &self.state,
            &self.space,
            &self.room,
            &self.author,
            event.id,
            plan.targets,
            chosen,
            &self.settings,
            &self.prefs,
        )
        .await;
        watch.done();
        Finished {
            addressing_note,
            routing_note: started.note,
            runs: started.runs,
            classified,
        }
    }
}

/// Start each planned run of message `event_id` in order. Before each one
/// (its use-case routing is a System One call) the message is read again:
/// once it's deleted, the rest are dropped. Each run is claimed, registered
/// and marked `started` in one transaction ([`crate::agent::start_room_run_claimed`]);
/// a run that can't start says why in the room and is dropped.
#[allow(clippy::too_many_arguments)]
async fn dispatch_planned(
    state: &AppState,
    space: &str,
    room: &crate::db::Room,
    author: &Author,
    event_id: i64,
    targets: Vec<crate::agent_dispatch::Target>,
    chosen: Option<crate::model_routing::Route>,
    settings: &RoutingSettings,
    prefs: &crate::space_prefs::SpacePrefs,
) -> crate::agent_dispatch::Started {
    let user = author.id.clone().unwrap_or_default();
    let mut started = crate::agent_dispatch::Started::default();
    for t in targets {
        let Some(event) = live_message(state, space, &room.id, event_id).await else {
            crate::decisions::forget(state, crate::decisions::Scope::Space, event_id).await;
            break;
        };
        let route = match &chosen {
            Some(route) => route.clone(),
            None => {
                // Only use-case routing calls System One; only it waits.
                let _slot = match routing_calls_system_one(settings, prefs, &t.agent.id) {
                    true => Some(state.decisions.slot(&user).await),
                    false => None,
                };
                crate::space_prefs::route_for(
                    state,
                    &user,
                    space,
                    prefs,
                    settings,
                    &t.agent.id,
                    &event.text,
                    t.addressed_by,
                )
                .await
            }
        };
        match crate::agent::start_room_run_claimed(
            state, space, room, author, &event, route, &t.agent, t.pick, true,
        )
        .await
        {
            Ok(run) => started.runs.push(run),
            Err(refusal) => {
                crate::decisions::forget_target(state, event_id, &t.agent.id).await;
                if refusal.shown.is_empty() {
                    continue;
                }
                state
                    .append(
                        space,
                        &room.id,
                        kind::ERROR,
                        &Author::server(actor::SYSTEM),
                        &refusal.shown,
                    )
                    .await;
                started.failures.push(refusal.private().to_owned());
                if started.note.is_none() {
                    started.note = refusal.note;
                }
            }
        }
    }
    started
}

/// Whether [`crate::agent_dispatch::plan`] asks System One for this message:
/// opt-in classification of unaddressed text, or the classifier's pick for
/// the generic `@agent`.
fn plan_calls_system_one(
    addressing: Addressing,
    text: &str,
    resolved: &[Mention],
    settings: &RoutingSettings,
    prefs: &crate::space_prefs::SpacePrefs,
) -> bool {
    let classifies = prefs.classify_unaddressed && settings.classifier_id.is_some();
    match addressing {
        Addressing::Unaddressed => !text.is_empty() && prefs.classify_unaddressed,
        Addressing::Agent => classifies && resolved.contains(&Mention::Agent),
        Addressing::PeopleOnly => false,
    }
}

/// Whether [`crate::space_prefs::route_for`] asks System One for agent
/// `agent_id`: use-case routing on, with no model set for the agent or the
/// Space.
fn routing_calls_system_one(
    settings: &RoutingSettings,
    prefs: &crate::space_prefs::SpacePrefs,
    agent_id: &str,
) -> bool {
    !prefs.agent_models.contains_key(agent_id)
        && prefs.default_model.is_none()
        && prefs.routing
        && settings.wants_case_routing()
}

/// The sender's current settings for a Space, for work resumed at startup.
async fn current_settings(
    state: &AppState,
    user: &str,
    space: &str,
) -> Option<(RoutingSettings, crate::space_prefs::SpacePrefs)> {
    match (
        routing::preferences(state, user).await,
        crate::space_prefs::load(state, space, user).await,
    ) {
        (Ok(s), Ok(p)) => Some((s, p)),
        _ => {
            tracing::error!("loading settings for pending work");
            None
        }
    }
}

/// The room of a pending Space message and the message, if both remain.
async fn pending_place(
    state: &AppState,
    space: &str,
    room: &str,
    event_id: i64,
) -> Option<(crate::db::Room, Event)> {
    let (sid, rid) = (space.to_owned(), room.to_owned());
    let found = state
        .db
        .call(move |db| {
            let conn = db.lock();
            let room = crate::db::Db::room_in(&conn, &sid, &rid)?;
            let event = crate::replies::event_in(&conn, &sid, &rid, event_id)?;
            Ok(room.zip(event))
        })
        .await;
    match found {
        Ok(Some((room, event))) if event.kind == kind::MESSAGE => Some((room, event)),
        Ok(_) => None,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "resuming pending work");
            None
        }
    }
}

/// The model chosen for resumed work, checked again.
async fn resumed_choice(
    state: &AppState,
    user: &str,
    space: &str,
    event: &Event,
    choice: Option<&serde_json::Value>,
) -> Result<Option<crate::model_routing::Route>, ()> {
    let Some(choice) = choice.filter(|c| !c.is_null()) else {
        return Ok(None);
    };
    // Why it can't be used is for its sender alone, and nothing private
    // reaches them after a restart: the room gets a generic note.
    recheck_choice(state, user, &event.actor, space, choice)
        .await
        .map_err(|why| tracing::info!(%why, "a resumed turn's chosen model can't be used"))
}

/// Make a Space message's decision left pending at shutdown
/// ([`crate::decisions::resume`]) with the sender's current settings.
pub(crate) async fn resume_space(state: AppState, row: crate::decisions::Stored) {
    use crate::decisions::{Scope, forget};
    let (Some(space), Some(room)) = (row.space_id.clone(), row.room_id.clone()) else {
        return forget(&state, Scope::Space, row.event_id).await;
    };
    let Some((room, event)) = pending_place(&state, &space, &room, row.event_id).await else {
        return forget(&state, Scope::Space, row.event_id).await;
    };
    // From here every early return drops the work and says so in the room.
    let mut watch = crate::decisions::Watch::space(&state, &space, &room.id, row.event_id);
    let resolved: Vec<Mention> =
        serde_json::from_value(row.payload["resolved"].clone()).unwrap_or_default();
    let reply_agent = row.payload["reply_agent"].as_str().map(str::to_owned);
    let addressing = if reply_agent.is_some() {
        Addressing::Agent
    } else {
        Addressing::of(&resolved)
    };
    let Some((settings, prefs)) = current_settings(&state, &row.user_id, &space).await else {
        return;
    };
    let choice = row.payload.get("choice").filter(|c| !c.is_null()).cloned();
    let Ok(chosen) = resumed_choice(&state, &row.user_id, &space, &event, choice.as_ref()).await
    else {
        watch.note = crate::decisions::RESTART_NOTE;
        return;
    };
    let author = Author {
        id: Some(row.user_id.clone()),
        name: event.actor.clone(),
    };
    let job = Deferred {
        state: state.clone(),
        user: row.user_id,
        space,
        room,
        author,
        event,
        resolved,
        addressing,
        settings,
        prefs,
        reply_agent,
        chosen: chosen.map(Box::new),
        choice,
    };
    // Its sender's notes went with the old process: nobody collects them.
    let _ = job.run(watch).await;
}

/// Start the runs a decision planned before a restart, in their order.
pub(crate) async fn resume_targets(state: AppState, planned: crate::decisions::StoredTargets) {
    use crate::decisions::{Scope, forget};
    let eid = planned.event_id;
    let Some((room, event)) = pending_place(&state, &planned.space_id, &planned.room_id, eid).await
    else {
        return forget(&state, Scope::Space, eid).await;
    };
    let mut watch = crate::decisions::Watch::space(&state, &planned.space_id, &room.id, eid);
    let Some((settings, prefs)) =
        current_settings(&state, &planned.user_id, &planned.space_id).await
    else {
        return;
    };
    let Ok(chosen) = resumed_choice(
        &state,
        &planned.user_id,
        &planned.space_id,
        &event,
        planned.choice.as_ref(),
    )
    .await
    else {
        watch.note = crate::decisions::RESTART_NOTE;
        return;
    };
    let space = planned.space_id.clone();
    let agents = match state.db.call(move |db| db.space_agents(&space)).await {
        Ok(agents) => agents,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "loading agents for planned runs");
            return;
        }
    };
    let mut targets = Vec::new();
    for t in planned.targets {
        match agents.iter().find(|a| a.id == t.agent_id) {
            Some(agent) => targets.push(crate::agent_dispatch::Target {
                agent: agent.clone(),
                pick: t.pick,
                addressed_by: t.addressed_by,
            }),
            // Deleted since: it isn't replaced.
            None => crate::decisions::forget_target(&state, eid, &t.agent_id).await,
        }
    }
    let author = Author {
        id: Some(planned.user_id.clone()),
        name: event.actor.clone(),
    };
    dispatch_planned(
        &state,
        &planned.space_id,
        &room,
        &author,
        eid,
        targets,
        chosen,
        &settings,
        &prefs,
    )
    .await;
    watch.done();
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
