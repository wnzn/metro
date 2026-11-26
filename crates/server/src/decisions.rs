//! Decisions made after a message is stored.
//!
//! With `"defer_decisions": true`, `POST /v1/spaces/{sid}/rooms/{rid}/messages`
//! stores and returns the message at once (`"decisions_pending": true`) when
//! it would otherwise wait on System One: the sender's opt-in
//! classification, the classifier's pick for the generic `@agent`, or
//! use-case routing. The server then makes those decisions, exactly once,
//! and starts the runs they choose in the background, whether or not anyone
//! asks for the outcome.
//!
//! `POST /v1/spaces/{sid}/rooms/{rid}/messages/{eid}/decisions` (body `{}`)
//! waits for them and returns the sender-only notes as POST metadata, shaped
//! like the message POST's response. Only the authenticated sender of that
//! message in that room gets them, and only while still a member and while
//! the message is still in the transcript; they are handed out once and
//! kept in memory only, so a restart, a second call, a session of someone
//! else or a lost membership gets 404. Nothing here is ever an event.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tokio::sync::watch;

use crate::AppState;
use crate::api::{ApiError, ApiResult, db, empty_object, member_of, room_of};
use crate::auth::Human;
use crate::db::Event;
use crate::messages::{Finished, SenderNote};
use crate::model_routing::AddressingNote;

/// How long a finished outcome waits for its sender.
const KEEP: Duration = Duration::from_secs(10 * 60);
/// Outcomes held at once; the oldest go first.
const MAX_PENDING: usize = 4096;
/// How long one call waits before answering "still pending".
const WAIT: Duration = Duration::from_secs(90);

/// The sender, Space, room and message an outcome belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    user: String,
    space: String,
    room: String,
    event: i64,
}

impl Key {
    pub fn new(user: &str, space: &str, room: &str, event: i64) -> Self {
        Self {
            user: user.to_owned(),
            space: space.to_owned(),
            room: room.to_owned(),
            event,
        }
    }
}

/// What the sender gets back.
#[derive(Clone, Debug)]
pub struct Outcome {
    addressing_note: Option<SenderNote>,
    routing_note: Option<AddressingNote>,
    classified: bool,
}

impl From<Finished> for Outcome {
    fn from(f: Finished) -> Self {
        Self {
            addressing_note: f.addressing_note,
            routing_note: f.routing_note,
            classified: f.classified,
        }
    }
}

struct Entry {
    event: Event,
    done: watch::Receiver<Option<Outcome>>,
    at: Instant,
}

/// Deferred outcomes by [`Key`], in memory only.
#[derive(Default)]
pub struct Pending {
    entries: Mutex<HashMap<Key, Entry>>,
}

impl Pending {
    /// Hold the outcome of `event`'s decisions for its sender; send it on
    /// the returned channel once they are made.
    pub fn register(&self, key: Key, event: Event) -> watch::Sender<Option<Outcome>> {
        let (tx, rx) = watch::channel(None);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        entries.retain(|_, e| now.duration_since(e.at) < KEEP || e.done.borrow().is_none());
        while entries.len() >= MAX_PENDING {
            let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.at)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            entries.remove(&oldest);
        }
        entries.insert(
            key,
            Entry {
                event,
                done: rx,
                at: now,
            },
        );
        tx
    }

    fn watch(&self, key: &Key) -> Option<(Event, watch::Receiver<Option<Outcome>>)> {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.get(key).map(|e| (e.event.clone(), e.done.clone()))
    }

    /// The finished outcome, handed out once.
    fn take(&self, key: &Key) -> Option<Outcome> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.get(key)?.done.borrow().is_none() {
            return None;
        }
        entries.remove(key).and_then(|e| e.done.borrow().clone())
    }

    fn forget(&self, key: &Key) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.remove(key);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

#[derive(Serialize)]
struct Decided {
    #[serde(flatten)]
    event: Event,
    addressing_note: Option<SenderNote>,
    routing_note: Option<AddressingNote>,
    /// The classifier chose an agent for this unaddressed message.
    classified: bool,
}

fn none_pending() -> ApiError {
    ApiError::not_found("no pending decisions for this message")
}

/// `POST .../messages/{eid}/decisions`: see the module docs. 200 with the
/// outcome; 202 `{"decisions_pending": true}` while still deciding.
pub async fn post_decisions(
    State(state): State<AppState>,
    who: Human,
    Path((sid, rid, eid)): Path<(String, String, i64)>,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    empty_object(&body)?;
    let space = member_of(&state, &sid, &who).await?;
    let room = room_of(&state, &space, &rid).await?;
    let key = Key::new(&who.id, &space.id, &room.id, eid);
    let Some((event, mut done)) = state.decisions.watch(&key) else {
        return Err(none_pending());
    };
    if tokio::time::timeout(WAIT, done.wait_for(Option::is_some))
        .await
        .is_err()
    {
        return Ok((
            StatusCode::ACCEPTED,
            Json(serde_json::json!({"decisions_pending": true})),
        )
            .into_response());
    }
    // Access and the row are checked again now: a note is never handed out
    // after its sender left or its message was deleted.
    let still = async {
        member_of(&state, &sid, &who).await?;
        let (space_id, room_id, user) = (space.id.clone(), room.id.clone(), who.id.clone());
        db(&state, move |db| {
            Ok(
                crate::replies::event_in(&db.lock(), &space_id, &room_id, eid)?
                    .is_some_and(|e| e.actor_id.as_deref() == Some(user.as_str())),
            )
        })
        .await
    }
    .await;
    match still {
        Ok(true) => {}
        Ok(false) => {
            state.decisions.forget(&key);
            return Err(none_pending());
        }
        Err(e) => {
            state.decisions.forget(&key);
            return Err(e);
        }
    }
    let Some(outcome) = state.decisions.take(&key) else {
        return Err(none_pending());
    };
    Ok((
        StatusCode::OK,
        Json(Decided {
            event,
            addressing_note: outcome.addressing_note,
            routing_note: outcome.routing_note,
            classified: outcome.classified,
        }),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Addressed, Author, kind, new_event};

    fn event(id: i64) -> Event {
        new_event(
            id,
            kind::MESSAGE,
            &Author {
                id: Some("alice".into()),
                name: "Alice".into(),
            },
            "hi",
            &Addressed::room(),
            None,
            1_800_000_000_000,
        )
    }

    #[test]
    fn an_outcome_is_handed_out_once_and_only_to_its_key() {
        let pending = Pending::default();
        let key = Key::new("alice", "s", "lobby", 7);
        let tx = pending.register(key.clone(), event(7));
        assert!(pending.take(&key).is_none(), "nothing before it is decided");
        tx.send(Some(Outcome {
            addressing_note: None,
            routing_note: None,
            classified: true,
        }))
        .unwrap();
        for other in [
            Key::new("bob", "s", "lobby", 7),
            Key::new("alice", "t", "lobby", 7),
            Key::new("alice", "s", "other", 7),
            Key::new("alice", "s", "lobby", 8),
        ] {
            assert!(pending.take(&other).is_none());
        }
        assert!(pending.take(&key).unwrap().classified);
        assert!(pending.take(&key).is_none(), "never replayed");
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn the_registry_is_bounded() {
        let pending = Pending::default();
        for i in 0..(MAX_PENDING as i64 + 10) {
            let _ = pending.register(Key::new("alice", "s", "lobby", i), event(i));
        }
        assert_eq!(pending.len(), MAX_PENDING);
    }
}
