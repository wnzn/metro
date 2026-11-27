//! Steering: a person's message delivered into an agent turn that is
//! already running, at its next model call, instead of starting another.
//!
//! A running turn opens an inbox for its [`Place`]: a personal Chat turn
//! that waited its place in its sender's line, or a Space run answering a
//! person's message (its room or thread, its agent and the person who
//! started it). A message offered to an open inbox waits there. Before each
//! model call the turn takes what waits and checks each message again, in
//! one transaction that also writes a `steered` marker naming it: still a
//! message, by the same person, who may still read the room, and (in Chat)
//! not stopped. It then joins the model's history as a user turn, after
//! everything the model has seen so far, and stays there for the rest of
//! the run. An offer the turn never takes (it ended first, or a check
//! failed) is handed back, and its sender's work goes on as it would have
//! without a running turn. Inboxes are memory only.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use rig_agent::agent::{DispatchAction, DispatchEvent};
use rig_agent::prelude::*;
use rig_core::effect::EffectKind;
use rig_core::message::Message;
use tokio::sync::{oneshot, watch};

use crate::AppState;
use crate::db::{Activity, Addressed, Author, Event, actor, kind};

/// Where a running turn takes steering.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    /// `user`'s personal Chat.
    Chat { user: String },
    /// Runs of `agent` that `user` started in room (or thread) `room`.
    Room {
        space: String,
        room: String,
        agent: String,
        user: String,
    },
}

/// A message waiting in an inbox.
pub struct Offer {
    event_id: i64,
    /// Space: the message's planned run (deferred work), claimed when taken.
    planned: bool,
    /// Chat: the message's own turn, which its sender may stop.
    cancel: Option<watch::Receiver<bool>>,
    taken: oneshot::Sender<()>,
}

type Shared = Arc<Mutex<VecDeque<Offer>>>;

/// The open inboxes, one per place.
#[derive(Default)]
pub struct Steering(Mutex<HashMap<Place, Shared>>);

impl Steering {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Place, Shared>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Open `place`'s inbox for a running turn. `None` when another turn
    /// there holds it (steering goes to that one).
    pub fn open(self: &Arc<Self>, place: Place) -> Option<Inbox> {
        let mut open = self.lock();
        if open.contains_key(&place) {
            return None;
        }
        let shared = Shared::default();
        open.insert(place.clone(), shared.clone());
        Some(Inbox {
            steering: self.clone(),
            place,
            shared,
        })
    }

    /// Offer message `event_id` to the turn running at `place`. `None` when
    /// nothing runs there; otherwise resolves `Ok` once the turn took it,
    /// `Err` when it was handed back.
    pub fn offer(
        &self,
        place: &Place,
        event_id: i64,
        planned: bool,
        cancel: Option<watch::Receiver<bool>>,
    ) -> Option<oneshot::Receiver<()>> {
        let open = self.lock();
        let shared = open.get(place)?;
        let (taken, rx) = oneshot::channel();
        shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(Offer {
                event_id,
                planned,
                cancel,
                taken,
            });
        Some(rx)
    }

    #[cfg(test)]
    pub fn is_open(&self, place: &Place) -> bool {
        self.lock().contains_key(place)
    }
}

/// A running turn's inbox. Dropped when the turn ends: whatever still
/// waits is handed back.
pub struct Inbox {
    steering: Arc<Steering>,
    place: Place,
    shared: Shared,
}

impl Inbox {
    fn take(&self) -> Vec<Offer> {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }
}

impl Drop for Inbox {
    fn drop(&mut self) {
        // Under the registry's lock: no offer lands after this.
        let mut open = self.steering.lock();
        if open
            .get(&self.place)
            .is_some_and(|s| Arc::ptr_eq(s, &self.shared))
        {
            open.remove(&self.place);
        }
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

/// Renders a delivered message as the run's user turn.
pub type Render = Arc<dyn Fn(&Event) -> Message + Send + Sync>;

/// The hook that delivers a turn's steering.
pub struct Steer {
    state: AppState,
    inbox: Arc<Inbox>,
    /// The running turn, as its `steered` markers name it.
    turn: Activity,
    render: Render,
    /// Delivered so far: where in the model's history each goes, and what.
    delivered: tokio::sync::Mutex<Vec<(usize, Arc<Message>)>>,
}

impl Steer {
    pub fn new(state: &AppState, inbox: Arc<Inbox>, turn: &Activity, render: Render) -> Self {
        Self {
            state: state.clone(),
            inbox,
            turn: turn.clone(),
            render,
            delivered: Default::default(),
        }
    }

    /// Check `offer` again and write its marker; the message once taken.
    async fn accept(&self, offer: Offer) -> Option<Event> {
        if offer.cancel.as_ref().is_some_and(|c| *c.borrow()) {
            return None;
        }
        let mut marker = self.turn.at("");
        marker.phase = None;
        marker.steered = Some(offer.event_id);
        let (place, epoch, id, planned) = (
            self.inbox.place.clone(),
            self.state.decisions.epoch().to_owned(),
            offer.event_id,
            offer.planned,
        );
        let accepted = self
            .state
            .db
            .call(move |db| deliver(db, &place, &epoch, id, planned, &marker))
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %format!("{e:#}"), "delivering a steered message");
                None
            });
        let event = accepted?;
        let _ = offer.taken.send(());
        Some(event)
    }
}

/// The message `id` at `place`, checked as [`Steer::accept`] says, with its
/// `steered` marker written; `None` (nothing written) when a check fails.
fn deliver(
    db: &crate::db::Db,
    place: &Place,
    epoch: &str,
    id: i64,
    planned: bool,
    marker: &Activity,
) -> anyhow::Result<Option<Event>> {
    let author = Author::server(actor::AGENT);
    match place {
        Place::Chat { user } => {
            let message = {
                let conn = db.lock();
                let mut stmt = conn.prepare(
                    "SELECT id, kind, actor, actor_id, text, mentions, audience, activity, created_at
                     FROM personal_events WHERE id = ?1 AND user_id = ?2 AND kind = 'message'",
                )?;
                use rusqlite::OptionalExtension;
                stmt.query_row(rusqlite::params![id, user], crate::db::row_event)
                    .optional()?
            };
            let Some(message) = message else {
                return Ok(None);
            };
            // Rechecks the message, and settles its pending turn unless it
            // was stopped or another process holds it.
            let written = db.append_personal_for_message(
                epoch,
                user,
                id,
                kind::STEERED,
                &author,
                "",
                Some(marker),
            )?;
            Ok(written.map(|_| message))
        }
        Place::Room {
            space,
            room,
            agent,
            user,
        } => {
            let mut conn = db.lock();
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            if !crate::db::Db::room_member_in(&tx, space, room, user)? {
                return Ok(None);
            }
            let Some(message) = crate::replies::event_in(&tx, space, room, id)?
                .filter(|e| e.kind == kind::MESSAGE && e.actor_id.as_deref() == Some(user))
            else {
                return Ok(None);
            };
            if planned && !crate::decisions::claim_target_in(&tx, epoch, id, agent)? {
                return Ok(None);
            }
            crate::db::Db::append_event_in(
                &tx,
                space,
                room,
                kind::STEERED,
                &author,
                "",
                &Addressed::room(),
                Some(marker),
            )?;
            tx.commit()?;
            Ok(Some(message))
        }
    }
}

/// `history` with each delivered message at its place: `delivered` in the
/// order delivered, each position counted in the history without them.
fn with_delivered(
    history: &[Arc<Message>],
    delivered: &[(usize, Arc<Message>)],
) -> Vec<Arc<Message>> {
    let mut out = history.to_vec();
    for (i, (at, message)) in delivered.iter().enumerate() {
        out.insert((at + i).min(out.len()), message.clone());
    }
    out
}

impl AgentHook for Steer {
    fn on_dispatch(
        &self,
        _ctx: &HookContext,
        event: DispatchEvent<'_>,
    ) -> impl std::future::Future<Output = DispatchAction> + Send {
        let request = match event.kind {
            EffectKind::Completion { request, stream } => Some((request.clone(), *stream)),
            _ => None,
        };
        async move {
            let Some((request, stream)) = request else {
                return DispatchAction::Proceed;
            };
            let mut delivered = self.delivered.lock().await;
            let at = request.chat_history.len();
            for offer in self.inbox.take() {
                if let Some(message) = self.accept(offer).await {
                    delivered.push((at, Arc::new((self.render)(&message))));
                }
            }
            if delivered.is_empty() {
                return DispatchAction::Proceed;
            }
            let mut patched = (*request).clone();
            patched.chat_history = with_delivered(&request.chat_history, &delivered);
            DispatchAction::Patch(EffectKind::Completion {
                request: Arc::new(patched),
                stream,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place() -> Place {
        Place::Chat {
            user: "alice".into(),
        }
    }

    #[tokio::test]
    async fn an_offer_waits_for_an_open_inbox_and_is_handed_back_when_it_closes() {
        let steering = Arc::new(Steering::default());
        assert!(steering.offer(&place(), 1, false, None).is_none());
        let inbox = steering.open(place()).unwrap();
        // One inbox per place.
        assert!(steering.open(place()).is_none());
        let first = steering.offer(&place(), 1, false, None).unwrap();
        let second = steering.offer(&place(), 2, false, None).unwrap();
        let taken = inbox.take();
        assert_eq!(taken.iter().map(|o| o.event_id).collect::<Vec<_>>(), [1, 2]);
        let mut taken = taken.into_iter();
        taken.next().unwrap().taken.send(()).unwrap();
        drop(taken);
        assert!(first.await.is_ok());
        assert!(second.await.is_err(), "taken but not accepted: handed back");
        let late = steering.offer(&place(), 3, false, None).unwrap();
        drop(inbox);
        assert!(late.await.is_err());
        assert!(!steering.is_open(&place()));
        assert!(steering.offer(&place(), 4, false, None).is_none());
    }

    #[test]
    fn delivered_messages_keep_their_place_as_history_grows() {
        let m = |t: &str| Arc::new(Message::user(t));
        let text = |h: &[Arc<Message>]| -> Vec<String> {
            h.iter()
                .map(|m| match &**m {
                    Message::User { content } => match content.iter().next() {
                        Some(rig_core::message::UserContent::Text(t)) => t.text.clone(),
                        _ => "?".into(),
                    },
                    _ => "?".into(),
                })
                .collect()
        };
        // Delivered at the second model call (history of 2), then at the
        // third (history of 4): each stays after what preceded it.
        let delivered = vec![(2, m("s1")), (4, m("s2"))];
        let history = vec![m("a"), m("b"), m("c"), m("d")];
        assert_eq!(
            text(&with_delivered(&history, &delivered)),
            ["a", "b", "s1", "c", "d", "s2"]
        );
        let history = vec![m("a"), m("b"), m("c"), m("d"), m("e")];
        assert_eq!(
            text(&with_delivered(&history, &delivered)),
            ["a", "b", "s1", "c", "d", "s2", "e"]
        );
    }
}
