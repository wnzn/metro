//! Which Space agents a new room message starts, and on which models.
//!
//! - **People only** (`@bob`): nothing, and System One is never asked.
//! - **To agents**: each `@handle` (or `{"kind":"agent","agent_id":…}`, or
//!   Ask agent with an agent id) starts that agent, with no classification.
//!   The generic `@agent` (or Ask agent without an id) starts the default
//!   agent; when the sender opted in to classification in this Space
//!   ([`crate::space_prefs`]) and the Space has several agents, one System
//!   One Choice over the agents (no "No agent" option) picks it instead. A
//!   low-confidence answer keeps the default agent; a failed decision runs
//!   no agent for `@agent` and says why. Several agents start one run each.
//! - **Unaddressed**: nothing, unless the sender opted in here. Then one
//!   System One Choice over every agent plus "No agent": if the probability
//!   of any agent clears the sender's floor, run the chosen agent (or the
//!   strongest agent option if it chose "No agent"). Low directedness and
//!   malformed or failed answers start nothing.
//! - **Replies to an agent** (`reply_to` an agent's reply in a thread, see
//!   [`crate::replies`]): that agent is an explicit recipient, with any of
//!   the above, and nothing is classified. If it was deleted, it isn't
//!   asked and the sender alone is told.
//!
//! Each run records how its agent was chosen on its `started` marker
//! (`activity.pick`), except when the classifier picked it: the run then
//! simply shows its agent. A chat model's free text is never used as a System
//! One answer.

use serde_json::json;

use crate::AppState;
use crate::db::{Addressed, Author, Event, Room, actor, kind};
use crate::mentions::Mention;
use crate::model_routing::{
    self as routing, AddressedBy, Addressing, AddressingNote, Decision, DecisionFailure, Route,
    RoutingSettings,
};
use crate::space_agents::SpaceAgent;
use crate::space_prefs::SpacePrefs;

/// One agent run to start.
#[derive(Clone, Debug)]
pub struct Target {
    pub agent: SpaceAgent,
    /// How it was chosen, for the run's `started` marker; none when the
    /// classifier picked it (the run simply shows its agent).
    pub pick: Option<String>,
    pub addressed_by: AddressedBy,
}

/// What a message starts.
#[derive(Debug, Default)]
pub struct Plan {
    pub targets: Vec<Target>,
    /// Shown in the room after the message, e.g. a failed agent decision.
    pub errors: Vec<String>,
    /// Only returned to the sender; this never becomes a room event.
    pub addressing_note: Option<AddressingNote>,
    /// Set when the classifier addressed an unaddressed message: the
    /// mentions to store instead of the empty ones.
    pub classified: Option<Addressed>,
}

fn push(targets: &mut Vec<Target>, t: Target) {
    if !targets.iter().any(|x| x.agent.id == t.agent.id) {
        targets.push(t);
    }
}

fn options(agents: &[SpaceAgent]) -> Vec<(String, String)> {
    agents
        .iter()
        .map(|a| (a.name.clone(), a.description.clone()))
        .collect()
}

/// Whether the sender opted in to classification here and has a System
/// One connection set.
fn classifies(prefs: &SpacePrefs, settings: &RoutingSettings) -> bool {
    prefs.classify_unaddressed && settings.classifier_id.is_some()
}

/// Decide what a message of `user` in `space` starts. `agents` are the
/// Space's agents, the default first. `reply` is the agent whose reply the
/// message answers in a thread ([`crate::replies::reply_agent`]): it is an
/// explicit recipient, so `addressing` is then [`Addressing::Agent`] and
/// the unaddressed classifier never runs.
#[allow(clippy::too_many_arguments)]
pub async fn plan(
    state: &AppState,
    user: &str,
    space: &str,
    room: &str,
    text: &str,
    resolved: &[Mention],
    addressing: Addressing,
    settings: &RoutingSettings,
    prefs: &SpacePrefs,
    agents: &[SpaceAgent],
    reply: Option<&SpaceAgent>,
) -> Plan {
    let mut plan = Plan::default();
    let Some(default) = agents.iter().find(|a| a.is_default).or(agents.first()) else {
        return plan;
    };
    match addressing {
        Addressing::PeopleOnly => {}
        // Attachments alone are ordinary conversation: never classified.
        Addressing::Unaddressed if text.is_empty() => {}
        Addressing::Unaddressed => {
            if !prefs.classify_unaddressed {
                return plan;
            }
            let answer = routing::pick_agent(
                state,
                user,
                space,
                settings,
                text,
                &options(agents),
                Some(room),
            )
            .await;
            if let Decision::Failed(why) = answer {
                plan.addressing_note = Some(why.into());
            }
            if let Decision::Answered(a) = answer
                && let Some(i) = a.index
                && a.meets_directedness(settings.min_directedness)
            {
                let agent = agents[i].clone();
                plan.classified = Some(Addressed {
                    mentions: json!([{"kind": "agent", "addressed_by": "classifier",
                                      "agent_id": agent.id}]),
                    audience: "agent".to_owned(),
                    spans: None,
                });
                plan.targets.push(Target {
                    pick: None,
                    agent,
                    addressed_by: AddressedBy::Classifier,
                });
            }
        }
        Addressing::Agent => {
            let mut generic_failed = None;
            for m in resolved {
                match m {
                    Mention::AgentId { agent_id } => {
                        if let Some(agent) = agents.iter().find(|a| a.id == *agent_id) {
                            push(
                                &mut plan.targets,
                                Target {
                                    pick: Some(format!("Addressed: @{}", agent.handle)),
                                    agent: agent.clone(),
                                    addressed_by: AddressedBy::Explicit,
                                },
                            );
                        }
                    }
                    Mention::Agent => {
                        match generic(state, user, space, text, settings, prefs, agents, default)
                            .await
                        {
                            Ok(t) => push(&mut plan.targets, t),
                            Err(why) => {
                                // The room reads one category; the sender
                                // alone gets the reason (below).
                                generic_failed.get_or_insert(why);
                                let why = why.shared();
                                plan.errors.push(format!(
                                    "Your classifier could not choose an agent for this message: \
                                     {why}. No agent ran for @agent; mention an agent by its \
                                     handle, or check your System One connection in Settings."
                                ));
                            }
                        }
                    }
                    Mention::User { .. } => {}
                }
            }
            if let Some(why) = generic_failed {
                plan.addressing_note = Some(AddressingNote::generic_agent_unavailable(
                    why,
                    resolved
                        .iter()
                        .any(|m| matches!(m, Mention::AgentId { .. })),
                ));
            }
        }
    }
    // After named agents, so naming the replied agent too starts one run.
    if let Some(agent) = reply {
        push(
            &mut plan.targets,
            Target {
                pick: Some(format!("Reply to @{}", agent.handle)),
                agent: agent.clone(),
                addressed_by: AddressedBy::Explicit,
            },
        );
    }
    plan
}

/// The agent the generic `@agent` means for this sender.
#[allow(clippy::too_many_arguments)]
async fn generic(
    state: &AppState,
    user: &str,
    space: &str,
    text: &str,
    settings: &RoutingSettings,
    prefs: &SpacePrefs,
    agents: &[SpaceAgent],
    default: &SpaceAgent,
) -> Result<Target, DecisionFailure> {
    let default_target = |pick: String| Target {
        agent: default.clone(),
        pick: Some(pick),
        addressed_by: AddressedBy::Explicit,
    };
    if !classifies(prefs, settings) || agents.len() < 2 {
        return Ok(default_target("Default agent".to_owned()));
    }
    match routing::pick_agent(state, user, space, settings, text, &options(agents), None).await {
        Decision::Answered(a) => match a.index {
            Some(i) if a.probability >= settings.min_route_probability => Ok(Target {
                agent: agents[i].clone(),
                pick: None,
                addressed_by: AddressedBy::Explicit,
            }),
            Some(i) => Ok(default_target(format!(
                "Default agent (the classifier was unsure: best {} at {:.2})",
                agents[i].name, a.probability
            ))),
            None => Ok(default_target("Default agent".to_owned())),
        },
        Decision::Failed(why) => Err(why),
    }
}

/// What [`start_runs`] did.
#[derive(Debug, Default)]
pub struct Started {
    /// Why runs could not start, in full, for the person who asked (a
    /// scheduled prompt's creator); the room got the shown text.
    pub failures: Vec<String>,
    /// The first sender-only note, for their POST response only.
    pub note: Option<AddressingNote>,
    /// The runs that started, in target order.
    pub runs: Vec<crate::orchestration::RunInfo>,
}

/// Start one run per target answering `event` (already stored) in `room`.
/// `chosen` is the explicit model for the turn, if any. A run that can't
/// start posts why in the room; the reasons are returned.
#[allow(clippy::too_many_arguments)]
pub async fn start_runs(
    state: &AppState,
    space: &str,
    room: &Room,
    author: &Author,
    event: &Event,
    targets: Vec<Target>,
    chosen: Option<Route>,
    settings: &RoutingSettings,
    prefs: &SpacePrefs,
) -> Started {
    let user = author.id.clone().unwrap_or_default();
    let mut started = Started::default();
    for t in targets {
        let route = match &chosen {
            Some(route) => route.clone(),
            None => {
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
        match crate::agent::start_room_run(
            state, space, room, author, event, route, &t.agent, t.pick,
        )
        .await
        {
            Ok(run) => started.runs.push(run),
            // The message was deleted meanwhile: silently nothing.
            Err(refusal) if refusal.shown.is_empty() => {}
            Err(refusal) => {
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
