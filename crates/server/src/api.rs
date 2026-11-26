//! HTTP handlers.
//!
//! Everything about a Space is visible to its members only: a Space that
//! does not exist and one the caller is not a member of both answer 404.
//! Owner-only actions answer 403 to members. The caller's role is read from
//! the database on every request.

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::agent::stop_runs_of;
use crate::attachments::{self, Append, IdsError, Place};
use crate::auth::{Human, Runner, new_runner_token, runner_token_hash, valid_id};
use crate::db::reactions::{ReactionScope, Reactor, valid_emoji};
use crate::db::{
    Activity, AddMember, Addressed, Author, Event, LimitError, MessageMeta, RemoveMember, Role,
    Room, SpaceView, activity, actor, kind,
};
use crate::jobs::{CompleteError, JobOutcome};
use crate::mentions::{self, Mention, MentionSpan};
use crate::model_routing::{self as routing, AddressedBy, Addressing, AddressingNote};
use crate::tools::RunnerTool;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
    code: Option<&'static str>,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: None,
        }
    }
    /// Add a stable code without replacing a more specific one.
    pub(crate) fn with_code(mut self, code: &'static str) -> Self {
        self.code.get_or_insert(code);
        self
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "missing or invalid bearer token")
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn status(&self) -> StatusCode {
        self.status
    }
    pub fn internal(e: anyhow::Error) -> Self {
        tracing::error!(error = %format!("{e:#}"), "internal error");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"error": self.message});
        if let Some(code) = self.code {
            body["code"] = json!(code);
        }
        let mut response = (self.status, Json(body)).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        Self::new(e.status(), e.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(e: QueryRejection) -> Self {
        Self::bad_request(e.body_text())
    }
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;
pub(crate) type JsonBody<T> = Result<Json<T>, JsonRejection>;

pub(crate) async fn db<T, F>(state: &AppState, f: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce(&crate::db::Db) -> anyhow::Result<T> + Send + 'static,
{
    state.db.call(f).await.map_err(ApiError::internal)
}

const NO_SPACE: &str = "no such Space";

/// The Space `sid` as `who` sees it; 404 unless they are a member.
pub(crate) async fn member_of(state: &AppState, sid: &str, who: &Human) -> ApiResult<SpaceView> {
    if crate::config::validate_id(sid).is_err() {
        return Err(ApiError::not_found(NO_SPACE));
    }
    let (sid, uid) = (sid.to_owned(), who.id.clone());
    db(state, move |db| db.membership(&sid, &uid))
        .await?
        .ok_or_else(|| ApiError::not_found(NO_SPACE))
}

/// Like [`member_of`], and 403 unless they own it.
pub(crate) async fn owner_of(state: &AppState, sid: &str, who: &Human) -> ApiResult<SpaceView> {
    let space = member_of(state, sid, who).await?;
    if space.role != Role::Owner {
        return Err(ApiError::forbidden("only the Space owner can do this"));
    }
    Ok(space)
}

pub(crate) async fn room_of(state: &AppState, space: &SpaceView, rid: &str) -> ApiResult<Room> {
    if crate::config::validate_id(rid).is_err() {
        return Err(ApiError::not_found("no such room"));
    }
    let (sid, rid) = (space.id.clone(), rid.to_owned());
    db(state, move |db| db.get_room(&sid, &rid))
        .await?
        .ok_or_else(|| ApiError::not_found("no such room"))
}

pub(crate) fn check_name(state: &AppState, name: &str) -> ApiResult<String> {
    let name = name.trim().to_owned();
    let max = state.config.limits.max_room_name_chars;
    if name.is_empty() || name.chars().count() > max || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "name must be 1..={max} printable characters"
        )));
    }
    Ok(name)
}

pub(crate) fn check_text(state: &AppState, text: &str) -> ApiResult<String> {
    let text = text.trim().to_owned();
    let max = state.config.limits.max_message_chars;
    if text.is_empty() || text.chars().count() > max {
        return Err(ApiError::bad_request(format!(
            "text must be 1..={max} characters"
        )));
    }
    Ok(text)
}

/// A message's text and attachments checked together: the text may be
/// empty only when files are attached, and the ids must be distinct, at
/// most [`attachments::MAX_ATTACHMENTS`], and shaped like library ids.
pub(crate) async fn check_message(
    state: &AppState,
    sender: &str,
    space: Option<&str>,
    text: &str,
    asset_ids: &[String],
) -> ApiResult<String> {
    match attachments::check_ids(asset_ids) {
        Ok(()) => {}
        Err(IdsError::TooMany) => {
            return Err(ApiError::bad_request(format!(
                "a message can attach at most {} files",
                attachments::MAX_ATTACHMENTS
            )));
        }
        Err(IdsError::Duplicate) => {
            return Err(ApiError::bad_request("asset_ids must not repeat a file"));
        }
        Err(IdsError::Unknown) => return Err(no_attachment()),
    }
    if !asset_ids.is_empty() && text.trim().is_empty() {
        return Ok(String::new());
    }
    let text = check_text(state, text)?;
    // A rejected file must not cause even the message text to be sent to
    // the opt-in classifier. append_message rechecks inside the write transaction.
    if !asset_ids.is_empty() {
        let (ids, user, scope) = (
            asset_ids.to_vec(),
            sender.to_owned(),
            space.map(str::to_owned),
        );
        let allowed = db(state, move |db| {
            for id in &ids {
                if !crate::library::visible(db, &user, id)?
                    .is_some_and(|asset| asset.uploader_id == user && asset.space_id == scope)
                {
                    return Ok(false);
                }
            }
            Ok(true)
        })
        .await?;
        if !allowed {
            return Err(no_attachment());
        }
    }
    Ok(text)
}

pub(crate) fn no_attachment() -> ApiError {
    ApiError::not_found("no such attachment")
}

/// Attachments are for people: a message for the agent needs words.
pub(crate) fn agent_needs_text(text: &str, addressing: Addressing) -> ApiResult<()> {
    if text.is_empty() && addressing == Addressing::Agent {
        return Err(ApiError::bad_request(
            "a message for the agent needs a question; the agent does not read attachments",
        ));
    }
    Ok(())
}

fn addressed(resolved: &[Mention]) -> Addressed {
    Addressed {
        mentions: serde_json::to_value(resolved).unwrap_or_else(|_| json!([])),
        audience: mentions::audience(resolved).to_owned(),
        spans: None,
    }
}

#[derive(Deserialize)]
pub struct AfterQuery {
    after: Option<i64>,
}

fn after_cursor(query: Result<Query<AfterQuery>, QueryRejection>) -> ApiResult<i64> {
    let Query(query) = query?;
    let after = query.after.unwrap_or(0);
    if after < 0 {
        return Err(ApiError::bad_request(
            "after must be a non-negative integer",
        ));
    }
    Ok(after)
}

// ---- Public and identity ----

pub async fn healthz() -> Json<serde_json::Value> {
    Json(json!({"ok": true}))
}

pub async fn me(State(state): State<AppState>, who: Human) -> Json<serde_json::Value> {
    let is_org_admin = state.config.is_org_admin(&who.id);
    Json(json!({"id": who.id, "actor": who.name, "is_org_admin": is_org_admin}))
}

/// A JSON response that must not be cached (it carries a token).
pub(crate) fn no_store(status: StatusCode, body: serde_json::Value) -> Response {
    let mut response = (status, Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

// ---- Spaces ----

pub async fn list_spaces(
    State(state): State<AppState>,
    who: Human,
) -> ApiResult<Json<serde_json::Value>> {
    let spaces = db(&state, move |db| db.list_spaces(&who.id)).await?;
    Ok(Json(json!({"spaces": spaces})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NameBody {
    name: String,
}

pub async fn create_space(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<NameBody>,
) -> ApiResult<(StatusCode, Json<SpaceView>)> {
    let Json(body) = body?;
    let name = check_name(&state, &body.name)?;
    let max = state.config.limits.max_spaces_per_owner;
    let owner = who.id.clone();
    match db(&state, move |db| Ok(db.create_space(&owner, &name, max))).await? {
        Ok(space) => {
            tracing::info!(space = %space.id, "Space created");
            Ok((StatusCode::CREATED, Json(space)))
        }
        Err(LimitError::TooMany) => {
            Err(ApiError::conflict(format!("you already own {max} Spaces")))
        }
        Err(LimitError::Duplicate | LimitError::Nested) => {
            Err(ApiError::conflict("the Space could not be created"))
        }
        Err(LimitError::Other(e)) => Err(ApiError::internal(e)),
    }
}

// ---- Members ----

pub async fn list_members(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let space = member_of(&state, &sid, &who).await?;
    let members = db(&state, move |db| db.list_members(&space.id)).await?;
    Ok(Json(json!({"members": members})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserBody {
    user_id: String,
}

pub async fn add_member(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<UserBody>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let space = owner_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    if !valid_id(&body.user_id) {
        return Err(ApiError::bad_request(
            "user_id must be 1..=256 printable characters without surrounding spaces",
        ));
    }
    // A managed id can be added only once its account exists, so no Space
    // can be prepared for an account that doesn't yet exist.
    if state.managed.enabled()
        && body
            .user_id
            .starts_with(crate::admin_users::MANAGED_ID_PREFIX)
    {
        let user = body.user_id.clone();
        let exists = crate::admin_users::is_managed_id(&user)
            && db(&state, move |db| db.managed_user_exists(&user)).await?;
        if !exists {
            return Err(ApiError::not_found("no such managed user"));
        }
    }
    let max = state.config.limits.max_members;
    let user = body.user_id.clone();
    match db(&state, move |db| Ok(db.add_member(&space.id, &user, max))).await? {
        Ok(AddMember::Added) => Ok((
            StatusCode::CREATED,
            Json(json!({"user_id": body.user_id, "role": Role::Member})),
        )),
        Ok(AddMember::Already(role)) => Ok((
            StatusCode::OK,
            Json(json!({"user_id": body.user_id, "role": role})),
        )),
        Err(LimitError::TooMany) => Err(ApiError::conflict(format!(
            "a Space can have at most {max} members"
        ))),
        Err(LimitError::Duplicate | LimitError::Nested) => {
            Err(ApiError::conflict("the member could not be added"))
        }
        Err(LimitError::Other(e)) => Err(ApiError::internal(e)),
    }
}

pub async fn remove_member(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<UserBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let space = owner_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    let (space_id, user) = (space.id.clone(), body.user_id.clone());
    match db(&state, move |db| db.remove_member(&space_id, &user)).await? {
        RemoveMember::Removed => {
            // Their runs stop now; their delegated actions are also refused
            // at call time by the membership check.
            let stopped = stop_runs_of(&state, &space.id, &body.user_id);
            // Queued borrowed device jobs whose caller just lost this Space's
            // share fail now instead of at claim time. The removal is already
            // committed and no database lock is held here; the relay still
            // rechecks access at claim and before returning a result.
            if let Err(e) = state.device.shares_changed(&state.db, None).await {
                tracing::warn!(error = %format!("{e:#}"), "rechecking borrowed device jobs");
            }
            Ok(Json(json!({"removed": true, "stopped_runs": stopped})))
        }
        RemoveMember::NotMember => Err(ApiError::not_found("no such member")),
        RemoveMember::IsOwner => Err(ApiError::conflict("the owner cannot be removed")),
    }
}

// ---- Runner token ----

pub async fn issue_runner_token(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    // `If-None-Match: *`: issue only while the Space has no runner at all.
    if let Some(r) =
        crate::runner_pairing::issue_if_requested(&state, &who, &sid, &headers, &body).await
    {
        return r;
    }
    empty_object(&body)?;
    let space = owner_of(&state, &sid, &who).await?;
    let token = new_runner_token().map_err(ApiError::internal)?;
    let hash = runner_token_hash(&token);
    let space_id = space.id.clone();
    let Some(generation) = db(&state, move |db| db.set_runner_token(&space_id, &hash)).await?
    else {
        return Err(ApiError::conflict(
            "this Space uses a registered runner; its token is managed from the runner \
             (POST /v1/runners/{id}/token). Detach it first to pair a dedicated runner",
        ));
    };
    // Jobs claimed with the previous token fail now; queued ones wait for
    // the runner that holds the new token.
    state.jobs.rotate(&space.id, generation);
    tracing::info!(space = %space.id, generation, "runner token issued");
    Ok(no_store(StatusCode::OK, json!({"token": token})))
}

pub async fn revoke_runner_token(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    // `If-Match: "rtg-<generation>"`: revoke only that exact pairing.
    if let Some(r) = crate::runner_pairing::revoke_if_requested(&state, &who, &sid, &headers).await
    {
        return r;
    }
    let space = owner_of(&state, &sid, &who).await?;
    let space_id = space.id.clone();
    let Some(generation) = db(&state, move |db| db.clear_runner_token(&space_id)).await? else {
        return Err(ApiError::conflict(
            "this Space uses a registered runner; detach it with DELETE /v1/spaces/{sid}/runner",
        ));
    };
    state.jobs.revoke(&space.id, generation);
    tracing::info!(space = %space.id, generation, "runner token revoked");
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// An empty body or `{}`.
pub(crate) fn empty_object(body: &[u8]) -> ApiResult<()> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Empty {}
    if !body.iter().all(u8::is_ascii_whitespace) {
        serde_json::from_slice::<Empty>(body)
            .map_err(|_| ApiError::bad_request("body must be {}"))?;
    }
    Ok(())
}

// ---- Rooms and events ----

#[derive(Deserialize)]
pub struct RoomsQuery {
    #[serde(default)]
    top_level_only: bool,
}

pub async fn list_rooms(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    query: Result<Query<RoomsQuery>, QueryRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let Query(query) = query?;
    let space = member_of(&state, &sid, &who).await?;
    let rooms = db(&state, move |db| {
        db.list_rooms_filtered(&space.id, query.top_level_only)
    })
    .await?;
    Ok(Json(json!({"rooms": rooms})))
}

pub async fn create_room(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
    body: JsonBody<NameBody>,
) -> ApiResult<(StatusCode, Json<Room>)> {
    let space = member_of(&state, &sid, &who).await?;
    let Json(body) = body?;
    let name = check_name(&state, &body.name)?;
    if crate::replies::is_reply_thread_name(&name) {
        return Err(ApiError::bad_request(
            "names like reply-1 are reserved for reply threads",
        ));
    }
    let limits = state.config.limits.clone();
    match db(&state, move |db| {
        Ok(db.create_room(
            &space.id,
            &name,
            None,
            limits.max_rooms,
            limits.max_threads_per_room,
        ))
    })
    .await?
    {
        Ok(room) => Ok((StatusCode::CREATED, Json(room))),
        Err(LimitError::Duplicate) => {
            Err(ApiError::conflict("a room with this name already exists"))
        }
        Err(LimitError::TooMany) => Err(ApiError::conflict("room limit reached")),
        Err(LimitError::Nested) => Err(ApiError::conflict("the room could not be created")),
        Err(LimitError::Other(e)) => Err(ApiError::internal(e)),
    }
}

pub async fn list_events(
    State(state): State<AppState>,
    who: Human,
    Path((sid, rid)): Path<(String, String)>,
    query: Result<Query<AfterQuery>, QueryRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let after = after_cursor(query)?;
    let space = member_of(&state, &sid, &who).await?;
    let room = room_of(&state, &space, &rid).await?;
    let page = state.config.limits.events_page;
    let events = db(&state, move |db| {
        let mut events = db.events_after(&space.id, &room.id, after, page)?;
        if room.parent_room_id.is_none() {
            crate::replies::hydrate(db, &space.id, &room.id, &mut events)?;
        }
        let place = Place::Space {
            space_id: space.id,
            room_id: room.id,
        };
        attachments::hydrate(db, &who.id, &place, &mut events)?;
        Ok(events)
    })
    .await?;
    Ok(Json(json!({"events": events})))
}

/// The reply thread of event `eid` in top-level room `rid`: 200 when it
/// exists, 201 when this call created it.
pub async fn reply_thread(
    State(state): State<AppState>,
    who: Human,
    Path((sid, rid, eid)): Path<(String, String, i64)>,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<Room>)> {
    use crate::replies::ThreadError;
    empty_object(&body)?;
    let space = member_of(&state, &sid, &who).await?;
    let room = room_of(&state, &space, &rid).await?;
    let max = state.config.limits.max_reply_threads;
    match db(&state, move |db| {
        crate::replies::get_or_create(db, &space.id, &room.id, eid, &who.id, max)
    })
    .await?
    {
        Ok((thread, true)) => Ok((StatusCode::CREATED, Json(thread))),
        Ok((thread, false)) => Ok((StatusCode::OK, Json(thread))),
        Err(ThreadError::NotFound) => Err(ApiError::not_found("no such event")),
        Err(ThreadError::Nested) => Err(ApiError::conflict("threads can't have threads")),
        Err(ThreadError::Kind) => Err(ApiError::conflict("this event can't have a thread")),
        Err(ThreadError::TooMany) => Err(ApiError::conflict(format!(
            "a Space can have at most {max} reply threads"
        ))),
        Err(ThreadError::NameTaken) => Err(ApiError::conflict(
            "this thread can't be created: an older room has its name",
        )),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostMessage {
    text: String,
    #[serde(default)]
    invoke_agent: bool,
    /// With `invoke_agent`: ask this agent of the Space instead of the
    /// generic one.
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    mentions: Vec<Mention>,
    /// Set to false when the client has resolved addressing itself. Only
    /// structured mentions and Ask agent can explicitly address recipients.
    #[serde(default = "default_true")]
    infer_text_mentions: bool,
    /// Optional display ranges in the decoded, untrimmed `text`; each
    /// index refers to the explicit `mentions` list as submitted.
    #[serde(default)]
    spans: Vec<MentionSpan>,
    /// Answer with this model connection instead of the sender's routing or
    /// default: their own, or one shared with this Space or the
    /// organization. Never falls back.
    #[serde(default)]
    provider_connection_id: Option<String>,
    model_id: Option<String>,
    reasoning_effort: Option<String>,
    /// Library files of this Space to attach (see [`crate::attachments`]).
    #[serde(default)]
    asset_ids: Vec<String>,
    /// In a thread: the event of this thread, or the reply thread's
    /// anchor, this message replies to (see [`crate::replies`]).
    #[serde(default)]
    reply_to: Option<i64>,
    /// `false`: replying to an agent's message doesn't ask that agent.
    #[serde(default = "default_true")]
    reply_to_agent: bool,
    /// Store the message first; make any System One decision afterwards
    /// (see [`crate::decisions`]).
    #[serde(default)]
    defer_decisions: bool,
}

fn default_true() -> bool {
    true
}

/// A model chosen for one turn, checked before anything is stored so a bad
/// choice never leaves a message the agent then answers some other way.
pub(crate) async fn check_choice(
    state: &AppState,
    who: &Human,
    space: Option<&str>,
    choice: Option<String>,
    model_id: Option<String>,
    reasoning_effort: Option<String>,
    addressing: Addressing,
) -> ApiResult<Option<routing::Route>> {
    let selecting = model_id.is_some() || reasoning_effort.is_some();
    let Some(id) = choice else {
        if selecting {
            return Err(ApiError::bad_request(
                "model_id and reasoning_effort require provider_connection_id",
            ));
        }
        return Ok(None);
    };
    if !crate::providers::api::valid_connection_id(&id) {
        return Err(ApiError::bad_request(
            "provider_connection_id must be a model connection id (pc_…)",
        ));
    }
    if addressing != Addressing::Agent {
        return Err(ApiError::bad_request(
            "provider_connection_id only applies to a message for the agent \
             (@agent or invoke_agent)",
        ));
    }
    let mut route = routing::Route::chosen(AddressedBy::Explicit, &id);
    if selecting {
        route.turn_selection = Some(
            crate::providers::turn::check(
                state,
                &who.id,
                space,
                &id,
                model_id.as_deref(),
                reasoning_effort.as_deref(),
            )
            .await
            .map_err(crate::providers::api::use_error)?,
        );
        return Ok(Some(route));
    }
    // Another person's unshared connection and a missing one look the same.
    // Other problems with a usable connection are reported by the turn.
    match state
        .routing
        .backend
        .chat_model(state, &who.id, space, &id)
        .await
    {
        Err(crate::providers::UseError::NotFound) => {
            Err(ApiError::not_found("no such model connection"))
        }
        Err(crate::providers::UseError::Internal) => Err(ApiError::internal(anyhow::anyhow!(
            "checking a model connection"
        ))),
        _ => Ok(Some(route)),
    }
}

#[derive(Serialize)]
pub struct PostedMessage {
    #[serde(flatten)]
    event: Event,
    addressing_note: Option<crate::messages::SenderNote>,
    /// Sender-only: why their use-case routing could not choose a model.
    routing_note: Option<AddressingNote>,
    /// The System One decisions for this message come afterwards; collect
    /// their notes from `POST .../messages/{eid}/decisions`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    decisions_pending: bool,
}

pub async fn post_message(
    State(state): State<AppState>,
    who: Human,
    Path((sid, rid)): Path<(String, String)>,
    body: JsonBody<PostMessage>,
) -> ApiResult<(StatusCode, Json<PostedMessage>)> {
    let Json(body) = body?;
    let posted = crate::messages::post_room_message(
        &state,
        &who,
        &sid,
        &rid,
        crate::messages::Outgoing {
            invoke_agent: body.invoke_agent,
            agent_id: body.agent_id,
            mentions: body.mentions,
            infer_text_mentions: body.infer_text_mentions,
            spans: body.spans,
            provider_connection_id: body.provider_connection_id,
            model_id: body.model_id,
            reasoning_effort: body.reasoning_effort,
            asset_ids: body.asset_ids,
            reply_to: body.reply_to,
            reply_to_agent: body.reply_to_agent,
            defer_decisions: body.defer_decisions,
            ..crate::messages::Outgoing::new(body.text)
        },
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(PostedMessage {
            event: posted.event,
            addressing_note: posted.addressing_note,
            routing_note: posted.routing_note,
            decisions_pending: posted.decisions_pending,
        }),
    ))
}

/// Demo: a deterministic `workspace.list` of the workspace root on the
/// Space's runner. Never calls a model.
pub async fn demo(
    State(state): State<AppState>,
    who: Human,
    Path((sid, rid)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<Event>)> {
    empty_object(&body)?;
    let space = member_of(&state, &sid, &who).await?;
    let room = room_of(&state, &space, &rid).await?;
    if !space.runner_paired {
        return Err(ApiError::conflict(
            "this Space has no runner; its owner can attach a registered runner or issue a \
             dedicated runner token",
        ));
    }
    let Ok(permit) = state.demo_runs.clone().try_acquire_owned() else {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too many Demo runs in progress",
        ));
    };
    let event = {
        let (sid, rid, author) = (space.id.clone(), room.id.clone(), who.author());
        let text = "Demo: workspace.list on the paired runner".to_owned();
        db(&state, move |db| {
            db.append_event(&sid, &rid, kind::DEMO, &author, &text, &Addressed::room())
        })
        .await?
    };
    let task_state = state.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let tool = RunnerTool::List;
        let arguments = tool.runner_arguments(&json!({})).expect("static arguments");
        let caller = Author::server(actor::SYSTEM);
        // Its own `demo` activity, never an agent turn: no model is involved.
        let run = Activity::demo();
        let lifecycle = |phase: &'static str| {
            let (state, space, room, run, caller) = (
                task_state.clone(),
                space.id.clone(),
                room.id.clone(),
                run.at(phase),
                caller.clone(),
            );
            async move {
                state
                    .append_with(&space, &room, kind::ACTIVITY, &caller, "", Some(&run))
                    .await;
            }
        };
        lifecycle(activity::STARTED).await;
        let result = crate::tools::call_runner(
            &task_state,
            &space.id,
            &room.id,
            &caller,
            Some(&run),
            tool,
            arguments,
        )
        .await;
        let phase = match result {
            Ok(outcome) if outcome.ok => activity::COMPLETED,
            _ => activity::FAILED,
        };
        lifecycle(phase).await;
    });
    Ok((StatusCode::ACCEPTED, Json(event)))
}

// ---- Reactions ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReactionBody {
    emoji: String,
}

async fn react(
    state: &AppState,
    who: &Human,
    scope: ReactionScope,
    eid: i64,
    emoji: String,
    present: bool,
) -> ApiResult<Json<serde_json::Value>> {
    if !valid_emoji(&emoji) {
        return Err(ApiError::bad_request(
            "emoji must be one of 👍 👀 ✅ 🙏 ❤️ 🎉",
        ));
    }
    let reactor = Reactor::Human {
        id: who.id.clone(),
        name: who.name.clone(),
    };
    match db(state, move |db| {
        db.set_reaction(&scope, eid, &reactor, &emoji, present)
    })
    .await?
    {
        Some(changed) => Ok(Json(json!({"changed": changed}))),
        None => Err(ApiError::not_found("no such message")),
    }
}

async fn space_reaction(
    state: AppState,
    who: Human,
    (sid, rid, eid): (String, String, i64),
    body: JsonBody<ReactionBody>,
    present: bool,
) -> ApiResult<Json<serde_json::Value>> {
    let Json(body) = body?;
    let space = member_of(&state, &sid, &who).await?;
    let room = room_of(&state, &space, &rid).await?;
    let scope = ReactionScope::Space {
        space_id: space.id,
        room_id: room.id,
    };
    react(&state, &who, scope, eid, body.emoji, present).await
}

pub async fn put_space_reaction(
    State(state): State<AppState>,
    who: Human,
    Path(path): Path<(String, String, i64)>,
    body: JsonBody<ReactionBody>,
) -> ApiResult<Json<serde_json::Value>> {
    space_reaction(state, who, path, body, true).await
}

pub async fn delete_space_reaction(
    State(state): State<AppState>,
    who: Human,
    Path(path): Path<(String, String, i64)>,
    body: JsonBody<ReactionBody>,
) -> ApiResult<Json<serde_json::Value>> {
    space_reaction(state, who, path, body, false).await
}

pub async fn put_chat_reaction(
    State(state): State<AppState>,
    who: Human,
    Path(eid): Path<i64>,
    body: JsonBody<ReactionBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let Json(body) = body?;
    let scope = ReactionScope::Personal {
        user_id: who.id.clone(),
    };
    react(&state, &who, scope, eid, body.emoji, true).await
}

pub async fn delete_chat_reaction(
    State(state): State<AppState>,
    who: Human,
    Path(eid): Path<i64>,
    body: JsonBody<ReactionBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let Json(body) = body?;
    let scope = ReactionScope::Personal {
        user_id: who.id.clone(),
    };
    react(&state, &who, scope, eid, body.emoji, false).await
}

// ---- Personal Chat ----

pub async fn chat_events(
    State(state): State<AppState>,
    who: Human,
    query: Result<Query<AfterQuery>, QueryRejection>,
) -> ApiResult<Json<serde_json::Value>> {
    let after = after_cursor(query)?;
    let page = state.config.limits.events_page;
    let events = db(&state, move |db| {
        let mut events = db.personal_after(&who.id, after, page)?;
        let place = Place::Personal {
            user_id: who.id.clone(),
        };
        attachments::hydrate(db, &who.id, &place, &mut events)?;
        Ok(events)
    })
    .await?;
    Ok(Json(json!({"events": events})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    text: String,
    /// Omission is a normal direct send. Keep accepting the legacy flag:
    /// false saves a note unless the message expressly mentions `@agent`.
    #[serde(default = "chat_invokes_agent")]
    invoke_agent: bool,
    /// Explicit Save note: takes precedence over the legacy flag and text.
    #[serde(default)]
    save_note: bool,
    /// Answer with this one of the sender's own model connections instead of
    /// their default. Never falls back to the default or the server model.
    #[serde(default)]
    provider_connection_id: Option<String>,
    model_id: Option<String>,
    reasoning_effort: Option<String>,
    /// The sender's own personal library files to attach.
    #[serde(default)]
    asset_ids: Vec<String>,
}

fn chat_invokes_agent() -> bool {
    true
}

impl ChatMessage {
    fn recipients(&self, text: &str) -> ApiResult<Vec<Mention>> {
        if self.save_note {
            Ok(Vec::new())
        } else if self.invoke_agent {
            Ok(vec![Mention::Agent])
        } else {
            mentions::resolve(text, &[], false, &[]).map_err(ApiError::bad_request)
        }
    }
}

pub async fn chat_message(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<ChatMessage>,
) -> ApiResult<(StatusCode, Json<PostedMessage>)> {
    let Json(body) = body?;
    let text = check_message(&state, &who.id, None, &body.text, &body.asset_ids).await?;
    let resolved = body.recipients(&text)?;
    let addressing = Addressing::of(&resolved);
    let chosen = if body.save_note {
        if body.provider_connection_id.is_some()
            || body.model_id.is_some()
            || body.reasoning_effort.is_some()
        {
            return Err(ApiError::bad_request("a note cannot select a model"));
        }
        None
    } else {
        check_choice(
            &state,
            &who,
            None,
            body.provider_connection_id.clone(),
            body.model_id.clone(),
            body.reasoning_effort.clone(),
            addressing,
        )
        .await?
    };
    // With a choice: only that connection. Otherwise routing, if the sender
    // turned it on, or their default; a connection that cannot be used fails
    // the reply. Notes have neither a choice nor routing settings.
    let how = match chosen {
        Some(route) => Some(ChatRoute::Chosen(Box::new(route))),
        None if addressing == Addressing::Agent => Some(ChatRoute::Routing(
            routing::preferences(&state, &who.id)
                .await
                .map_err(ApiError::internal)?,
        )),
        None => None,
    };
    // A turn to start is durable with its message: a restart starts it.
    let pending = how.as_ref().map(|_| crate::decisions::NewPending {
        scope: crate::decisions::Scope::Personal,
        user_id: who.id.clone(),
        place: None,
        payload: json!({"choice": body.provider_connection_id.as_ref().map(|id| json!({
            "provider_connection_id": id,
            "model_id": body.model_id,
            "reasoning_effort": body.reasoning_effort,
        }))}),
    });
    let addressed = addressed(&resolved);
    let (uid, author, stored) = (who.id.clone(), who.author(), text.clone());
    let ids = body.asset_ids;
    let place = Place::Personal { user_id: uid };
    let event = match db(&state, move |db| {
        attachments::append_message(
            db,
            &place,
            &author,
            &stored,
            &addressed,
            &ids,
            &[],
            &MessageMeta::default(),
            pending.as_ref(),
        )
    })
    .await?
    {
        Append::Posted(event) => *event,
        Append::NoRoom | Append::NoAsset | Append::NoRecipient | Append::NoReplyTarget => {
            return Err(no_attachment());
        }
    };
    // Routing (a System One call) and the turn come after the response:
    // the message is already stored and shown, in the order it was sent.
    if let Some(how) = how {
        let ticket = state.decisions.chat_ticket(&who.id);
        let (state, user, event) = (state.clone(), who.id.clone(), event.clone());
        tokio::spawn(chat_turn(state, user, event, how, ticket));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(PostedMessage {
            event,
            addressing_note: None,
            routing_note: None,
            decisions_pending: false,
        }),
    ))
}

/// How a personal Chat turn gets its model.
pub(crate) enum ChatRoute {
    /// The model chosen for this turn, already checked.
    Chosen(Box<routing::Route>),
    /// The sender's routing, if on, or their default.
    Routing(routing::RoutingSettings),
}

/// Route and start the Chat turn answering `event` once `ticket` is served,
/// so a person's turns start in send order. The turn is registered first,
/// while it waits and routes, so Stop and the busy check see it; one
/// stopped meanwhile never starts. Its pending decision is settled with its
/// `started` marker, or here when nothing starts.
pub(crate) async fn chat_turn(
    state: AppState,
    user: String,
    event: Event,
    how: ChatRoute,
    ticket: crate::decisions::Ticket,
) {
    use crate::decisions::{Scope, forget};
    let turn = crate::db::Activity::chat();
    let (cancel, guard) = state.chat_runs.register(&turn.id, &user);
    let _in_line = ticket.wait().await;
    let route = match how {
        ChatRoute::Chosen(route) => *route,
        ChatRoute::Routing(settings) => {
            // Only use-case routing calls System One.
            let _slot = match settings.enabled {
                true => Some(state.decisions.slot(&user).await),
                false => None,
            };
            routing::route_turn(
                &state,
                &user,
                None,
                &settings,
                &event.text,
                AddressedBy::Explicit,
                None,
            )
            .await
        }
    };
    if *cancel.borrow() {
        return forget(&state, Scope::Personal, event.id).await;
    }
    let (started, written) = tokio::sync::oneshot::channel();
    let start = crate::agent::ChatStart {
        turn,
        cancel,
        guard,
        started,
    };
    // A turn that cannot start says why in the Chat.
    if crate::agent::start_chat_turn(&state, &user, &event, route, start)
        .await
        .is_err()
    {
        return forget(&state, Scope::Personal, event.id).await;
    }
    // Hold the line until this turn's `started` marker is written (or it
    // found its message deleted).
    let _ = written.await;
}

/// Start a Chat turn left pending at shutdown ([`crate::decisions::resume`]).
pub(crate) async fn resume_chat_turn(
    state: AppState,
    row: crate::decisions::Stored,
    ticket: crate::decisions::Ticket,
) {
    use crate::decisions::{Scope, forget};
    let (user, eid) = (row.user_id.clone(), row.event_id);
    let found = state
        .db
        .call(move |db| {
            let conn = db.lock();
            let mut stmt = conn.prepare(
                "SELECT id, kind, actor, actor_id, text, mentions, audience, activity, created_at
                 FROM personal_events WHERE id = ?1 AND user_id = ?2 AND kind = 'message'",
            )?;
            Ok(stmt
                .query_row(rusqlite::params![eid, user], crate::db::row_event)
                .ok())
        })
        .await;
    let event = match found {
        Ok(Some(event)) => event,
        Ok(None) => return forget(&state, Scope::Personal, row.event_id).await,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "resuming a pending Chat turn");
            return;
        }
    };
    let how = match row.payload["choice"].as_object() {
        Some(choice) => {
            let who = Human {
                id: row.user_id.clone(),
                name: event.actor.clone(),
            };
            let text = |k: &str| choice.get(k).and_then(|v| v.as_str()).map(str::to_owned);
            match check_choice(
                &state,
                &who,
                None,
                text("provider_connection_id"),
                text("model_id"),
                text("reasoning_effort"),
                Addressing::Agent,
            )
            .await
            {
                Ok(Some(route)) => ChatRoute::Chosen(Box::new(route)),
                Ok(None) => return forget(&state, Scope::Personal, row.event_id).await,
                Err(e) => {
                    let message = format!(
                        "Metro restarted before this turn started, and its chosen model can't be \
                         used now: {} No agent ran.",
                        e.message()
                    );
                    let (user, author) = (row.user_id.clone(), Author::server(actor::SYSTEM));
                    let _ = state
                        .db
                        .call(move |db| {
                            db.append_personal(
                                &user,
                                kind::ERROR,
                                &author,
                                &message,
                                &Addressed::room(),
                            )
                        })
                        .await;
                    return forget(&state, Scope::Personal, row.event_id).await;
                }
            }
        }
        None => match routing::preferences(&state, &row.user_id).await {
            Ok(settings) => ChatRoute::Routing(settings),
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "routing settings for a pending turn");
                return;
            }
        },
    };
    chat_turn(state, row.user_id, event, how, ticket).await;
}

#[cfg(test)]
#[test]
fn explicit_chat_note_never_resolves_legacy_mentions() {
    // Rust recognizes U+0085 as whitespace; JavaScript's \s doesn't.
    for text in ["@agent ping", "line\u{0085}@agent ping"] {
        let legacy: ChatMessage =
            serde_json::from_value(json!({"text": text, "invoke_agent": false})).unwrap();
        assert_eq!(legacy.recipients(text).unwrap(), [Mention::Agent]);
        for invoke_agent in [false, true] {
            let note: ChatMessage = serde_json::from_value(
                json!({"text": text, "invoke_agent": invoke_agent, "save_note": true}),
            )
            .unwrap();
            assert!(note.recipients(text).unwrap().is_empty());
        }
    }
}

// ---- Runs ----

pub async fn list_runs(
    State(state): State<AppState>,
    who: Human,
) -> ApiResult<Json<serde_json::Value>> {
    let user = who.id.clone();
    let spaces = db(&state, move |db| db.list_spaces(&user)).await?;
    let runs: Vec<_> = state
        .runs
        .list_with_failures()
        .into_iter()
        .filter(|(r, _)| spaces.iter().any(|s| s.id == r.space_id))
        .map(|(r, failure)| run_view(&state.compaction, &who, r, failure))
        .collect::<ApiResult<_>>()?;
    Ok(Json(json!({"runs": runs})))
}

/// A failed membership check on a run's Space, as the run's API answers
/// it: not being a member reads like an unknown run (404); anything else
/// (an internal error) keeps its own status.
fn run_not_found(e: ApiError, not_found: impl FnOnce() -> ApiError) -> ApiError {
    if e.status() == StatusCode::NOT_FOUND {
        not_found()
    } else {
        e
    }
}

/// One run, for a member of its Space: 404 for anyone else.
pub async fn get_run(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let not_found = || ApiError::not_found("no such run");
    let (run, failure) = state.runs.get_with_failure(&id).ok_or_else(not_found)?;
    member_of(&state, &run.space_id, &who)
        .await
        .map_err(|e| run_not_found(e, not_found))?;
    Ok(Json(run_view(&state.compaction, &who, run, failure)?))
}

/// A run as `who` sees it: with `failure`, the full reason it failed, only
/// when `who` started it (the room showed a generic error); never for
/// other members, including the Space owner.
fn run_view(
    compaction: &crate::compaction::Compaction,
    who: &Human,
    run: crate::orchestration::RunInfo,
    failure: Option<crate::orchestration::RunFailure>,
) -> ApiResult<serde_json::Value> {
    let failure = failure.filter(|_| run.actor == who.id);
    let note = compaction.note_for(&run.id, &who.id);
    let mut view = serde_json::to_value(&run).map_err(|e| ApiError::internal(e.into()))?;
    if let Some(message) = note {
        view["compaction_failure"] = json!({"message": message});
    }
    if let Some(failure) = failure {
        view["failure"] =
            serde_json::to_value(failure).map_err(|e| ApiError::internal(e.into()))?;
    }
    Ok(view)
}

pub async fn stop_run(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Json<serde_json::Value>> {
    empty_object(&body)?;
    let not_found = || ApiError::not_found("no such run");
    let run = state.runs.get(&id).ok_or_else(not_found)?;
    let space = member_of(&state, &run.space_id, &who)
        .await
        .map_err(|e| run_not_found(e, not_found))?;
    if run.actor != who.id && space.role != Role::Owner {
        return Err(ApiError::forbidden(
            "only the person who started a run, or the Space owner, can stop it",
        ));
    }
    let run = state
        .runs
        .stop(&id)
        .ok_or_else(|| ApiError::conflict("the run is not running"))?;
    Ok(Json(
        serde_json::to_value(run).map_err(|e| ApiError::internal(e.into()))?,
    ))
}

// ---- Runner ----

/// Claim the oldest queued job of any Space this runner serves. Protocol 2
/// claims name the job's `space_id`; a registered runner keeps a separate
/// workspace per Space and must check it.
///
/// The optional body (`{"runner": {version, install, …}}`, at most 2 KiB)
/// is untrusted presence data for Settings → Runners; it never fails a
/// claim. While its manager has asked this runner to update, the response
/// carries `X-Metro-Runner-Update: X.Y.Z` and nothing else about it.
pub async fn runner_claim(
    State(state): State<AppState>,
    runner: Runner,
    body: axum::body::Bytes,
) -> Response {
    let update = crate::updates::on_claim(&state, &runner, crate::updates::claim_body(&body)).await;
    let protocol = runner.protocol;
    let jobs = state.jobs.clone();
    let claimed = db(&state, move |db| {
        db.with_runner_queue_access(&runner.spaces, |spaces| {
            jobs.claim_at(&runner.claimer(), spaces)
        })
    })
    .await;
    let mut response = match claimed {
        Ok(Some(mut job)) => {
            if protocol < 2 {
                job.space_id = None;
            }
            (StatusCode::OK, Json(job)).into_response()
        }
        Ok(None) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => return e.into_response(),
    };
    if let Some(version) = update
        && let Ok(value) = HeaderValue::from_str(&version)
    {
        response
            .headers_mut()
            .insert(crate::updates::UPDATE_HEADER, value);
    }
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobResult {
    ok: bool,
    output: String,
}

pub async fn runner_result(
    State(state): State<AppState>,
    runner: Runner,
    Path(id): Path<String>,
    body: JsonBody<JobResult>,
) -> ApiResult<Json<serde_json::Value>> {
    let Json(body) = body?;
    if body.output.len() > state.config.limits.max_result_bytes {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "output too large",
        ));
    }
    let outcome = JobOutcome {
        ok: body.ok,
        output: body.output,
    };
    let jobs = state.jobs.clone();
    match db(&state, move |db| {
        db.with_runner_queue_access(&runner.spaces, |spaces| {
            jobs.complete_at(&runner.claimer(), spaces, &id, outcome)
        })
    })
    .await?
    {
        Ok(()) => Ok(Json(json!({"accepted": true}))),
        // A retried delivery of a result we already have: acknowledge it so
        // the runner stops retrying, but say it was not used.
        Err(CompleteError::Duplicate) => {
            Ok(Json(json!({"accepted": false, "reason": "duplicate"})))
        }
        Err(CompleteError::Expired) => Err(ApiError::new(StatusCode::GONE, "job expired")),
        Err(CompleteError::NotClaimed) => Err(ApiError::conflict("job has not been claimed")),
        Err(CompleteError::Unknown) => Err(ApiError::not_found("no such job")),
    }
}

#[cfg(test)]
#[test]
fn only_a_failed_membership_reads_as_an_unknown_run() {
    let unknown = || ApiError::not_found("no such run");
    let outsider = run_not_found(ApiError::not_found(NO_SPACE), unknown);
    assert_eq!(
        (outsider.status(), outsider.message()),
        (StatusCode::NOT_FOUND, "no such run")
    );
    let broken = run_not_found(ApiError::internal(anyhow::anyhow!("disk")), unknown);
    assert_eq!(broken.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
