//! Copy a readable message's text without copying recipients or dispatching.

use axum::{Json, extract::State, http::StatusCode};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::api::{self, ApiError, ApiResult, JsonBody};
use crate::auth::Human;
use crate::db::{Addressed, Db, Event, Forwarded, ForwardedAuthor, MessageMeta, SameSpace, kind};
use crate::db::{rfc3339_utc, row_event};
use crate::dms::{self, DmError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    source: Source,
    destination: Destination,
    comment: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Source {
    Space {
        space_id: String,
        room_id: String,
        event_id: i64,
    },
    Dm {
        dm_id: String,
        event_id: i64,
    },
    Chat {
        event_id: i64,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Destination {
    Space { space_id: String, room_id: String },
    Dm { user_id: String },
}

#[derive(Serialize)]
pub(crate) struct Response {
    events: Vec<Event>,
    omitted_attachments: i64,
}

fn no_source() -> ApiError {
    ApiError::not_found("no such source message").with_code("forward_source_not_found")
}

fn no_destination() -> ApiError {
    ApiError::not_found("no such destination").with_code("forward_destination_not_found")
}

impl Source {
    fn read(&self, tx: &Transaction<'_>, user: &str) -> Result<(Event, i64), DmError> {
        const COLUMNS: &str =
            "id, kind, actor, actor_id, text, mentions, audience, activity, created_at";
        let (event, scope) = match self {
            Self::Space {
                space_id,
                room_id,
                event_id,
            } => {
                if !Db::room_member_in(tx, space_id, room_id, user)? {
                    return Err(no_source().into());
                }
                let event = tx
                    .query_row(
                        &format!(
                            "SELECT {COLUMNS} FROM space_events
                                  WHERE space_id = ?1 AND room_id = ?2 AND id = ?3"
                        ),
                        params![space_id, room_id, event_id],
                        row_event,
                    )
                    .optional()?;
                (event, Some("space"))
            }
            Self::Dm { dm_id, event_id } => (dms::visible_event(tx, dm_id, user, *event_id)?, None),
            Self::Chat { event_id } => {
                let event = tx
                    .query_row(
                        &format!(
                            "SELECT {COLUMNS} FROM personal_events WHERE user_id = ?1 AND id = ?2"
                        ),
                        params![user, event_id],
                        row_event,
                    )
                    .optional()?;
                (event, Some("personal"))
            }
        };
        let event = event
            .filter(|event| matches!(event.kind.as_str(), kind::MESSAGE | kind::AGENT))
            .ok_or_else(no_source)?;
        let omitted = match scope {
            Some(scope) => tx.query_row(
                "SELECT count(*) FROM message_attachments WHERE scope = ?1 AND event_id = ?2",
                params![scope, event.id],
                |row| row.get(0),
            )?,
            None => 0,
        };
        Ok((event, omitted))
    }
}

fn write(
    tx: &Transaction<'_>,
    who: &Human,
    Request {
        source,
        destination,
        comment,
    }: Request,
) -> Result<Response, DmError> {
    let (event, omitted_attachments) = source.read(tx, &who.id)?;
    let same_space = match (&source, &destination) {
        (
            Source::Space {
                space_id,
                room_id,
                event_id,
            },
            Destination::Space {
                space_id: target_space,
                room_id: target_room,
            },
        ) if space_id == target_space => {
            if room_id == target_room {
                return Err(ApiError::bad_request("forward to a different room").into());
            }
            Some(SameSpace {
                room_id: room_id.clone(),
                event_id: *event_id,
            })
        }
        _ => None,
    };
    let meta = MessageMeta {
        forwarded: Some(Forwarded {
            // An agent's reply is stored under the server actor: its name
            // is the agent's (or the turn's, `Metro` in Chat).
            author_name: match (&event.kind[..], &event.activity) {
                (kind::AGENT, Some(a)) => a.agent.as_ref().map_or(&a.name, |b| &b.name).clone(),
                _ => event.actor,
            },
            author_kind: if event.kind == kind::AGENT {
                ForwardedAuthor::Agent
            } else {
                ForwardedAuthor::Person
            },
            sent_at: event.created_at.and_then(rfc3339_utc),
            same_space,
        }),
        ..MessageMeta::default()
    };
    let messages = comment
        .as_deref()
        .into_iter()
        .map(|text| (text, MessageMeta::default()))
        .chain(std::iter::once((event.text.as_str(), meta)));
    let events = match destination {
        Destination::Space { space_id, room_id } => {
            if !Db::room_member_in(tx, &space_id, &room_id, &who.id)? {
                return Err(no_destination().into());
            }
            let author = who.author();
            let addressed = Addressed {
                spans: Some(Vec::new()),
                ..Addressed::room()
            };
            messages
                .map(|(text, meta)| {
                    Db::append_message_in(tx, &space_id, &room_id, &author, text, &addressed, &meta)
                        .map_err(DmError::Store)
                })
                .collect::<Result<_, _>>()?
        }
        Destination::Dm { user_id } => {
            let (id, _) = dms::get_or_create(tx, &who.id, &user_id).map_err(|err| match err {
                DmError::Request(ref e) if e.status() == StatusCode::NOT_FOUND => {
                    no_destination().into()
                }
                _ => err,
            })?;
            messages
                .map(|(text, meta)| dms::append_message(tx, &id, who, text, &meta))
                .collect::<Result<_, _>>()?
        }
    };
    Ok(Response {
        events,
        omitted_attachments,
    })
}

pub(crate) async fn post(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<Request>,
) -> ApiResult<(StatusCode, Json<Response>)> {
    let Json(mut request) = body?;
    request.comment = request
        .comment
        .as_deref()
        .map(|text| api::check_text(&state, text))
        .transpose()?;
    let response = api::db(&state, move |db| {
        let mut conn = db.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = write(&tx, &who, request);
        if result.is_ok() {
            tx.commit()?;
        }
        Ok(result)
    })
    .await??;
    Ok((StatusCode::CREATED, Json(response)))
}
