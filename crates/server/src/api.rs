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
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::agent::{respond_personal, start_room_run, stop_runs_of};
use crate::auth::{Human, Runner, new_runner_token, runner_token_hash, valid_id};
use crate::db::reactions::{ReactionScope, Reactor, valid_emoji};
use crate::db::{
    AddMember, Addressed, Author, Event, LimitError, RemoveMember, Role, Room, SpaceView, actor,
    kind,
};
use crate::jobs::{CompleteError, JobOutcome};
use crate::mentions::{self, Mention};
use crate::orchestration::RunStatus;
use crate::tools::RunnerTool;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "missing or invalid bearer token")
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    pub fn internal(e: anyhow::Error) -> Self {
        tracing::error!(error = %format!("{e:#}"), "internal error");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(json!({"error": self.message}))).into_response();
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

type ApiResult<T> = Result<T, ApiError>;
type JsonBody<T> = Result<Json<T>, JsonRejection>;

async fn db<T, F>(state: &AppState, f: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce(&crate::db::Db) -> anyhow::Result<T> + Send + 'static,
{
    state.db.call(f).await.map_err(ApiError::internal)
}

const NO_SPACE: &str = "no such Space";

/// The Space `sid` as `who` sees it; 404 unless they are a member.
async fn member_of(state: &AppState, sid: &str, who: &Human) -> ApiResult<SpaceView> {
    if crate::config::validate_id(sid).is_err() {
        return Err(ApiError::not_found(NO_SPACE));
    }
    let (sid, uid) = (sid.to_owned(), who.id.clone());
    db(state, move |db| db.membership(&sid, &uid))
        .await?
        .ok_or_else(|| ApiError::not_found(NO_SPACE))
}

/// Like [`member_of`], and 403 unless they own it.
async fn owner_of(state: &AppState, sid: &str, who: &Human) -> ApiResult<SpaceView> {
    let space = member_of(state, sid, who).await?;
    if space.role != Role::Owner {
        return Err(ApiError::forbidden("only the Space owner can do this"));
    }
    Ok(space)
}

async fn room_of(state: &AppState, space: &SpaceView, rid: &str) -> ApiResult<Room> {
    if crate::config::validate_id(rid).is_err() {
        return Err(ApiError::not_found("no such room"));
    }
    let (sid, rid) = (space.id.clone(), rid.to_owned());
    db(state, move |db| db.get_room(&sid, &rid))
        .await?
        .ok_or_else(|| ApiError::not_found("no such room"))
}

fn check_name(state: &AppState, name: &str) -> ApiResult<String> {
    let name = name.trim().to_owned();
    let max = state.config.limits.max_room_name_chars;
    if name.is_empty() || name.chars().count() > max || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "name must be 1..={max} printable characters"
        )));
    }
    Ok(name)
}

fn check_text(state: &AppState, text: &str) -> ApiResult<String> {
    let text = text.trim().to_owned();
    let max = state.config.limits.max_message_chars;
    if text.is_empty() || text.chars().count() > max {
        return Err(ApiError::bad_request(format!(
            "text must be 1..={max} characters"
        )));
    }
    Ok(text)
}

fn addressed(resolved: &[Mention]) -> Addressed {
    Addressed {
        mentions: serde_json::to_value(resolved).unwrap_or_else(|_| json!([])),
        audience: mentions::audience(resolved).to_owned(),
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

pub async fn me(who: Human) -> Json<serde_json::Value> {
    Json(json!({"id": who.id, "actor": who.name}))
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
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    empty_object(&body)?;
    let space = owner_of(&state, &sid, &who).await?;
    let token = new_runner_token().map_err(ApiError::internal)?;
    let hash = runner_token_hash(&token);
    let space_id = space.id.clone();
    let generation = db(&state, move |db| db.set_runner_token(&space_id, &hash)).await?;
    // Jobs claimed with the previous token fail now; queued ones wait for
    // the runner that holds the new token.
    state.jobs.rotate(&space.id, generation);
    tracing::info!(space = %space.id, generation, "runner token issued");
    let mut response = Json(json!({"token": token})).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    Ok(response)
}

pub async fn revoke_runner_token(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
) -> ApiResult<StatusCode> {
    let space = owner_of(&state, &sid, &who).await?;
    let space_id = space.id.clone();
    let generation = db(&state, move |db| db.clear_runner_token(&space_id)).await?;
    state.jobs.revoke(&space.id, generation);
    tracing::info!(space = %space.id, generation, "runner token revoked");
    Ok(StatusCode::NO_CONTENT)
}

/// An empty body or `{}`.
fn empty_object(body: &[u8]) -> ApiResult<()> {
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

pub async fn list_rooms(
    State(state): State<AppState>,
    who: Human,
    Path(sid): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let space = member_of(&state, &sid, &who).await?;
    let rooms = db(&state, move |db| db.list_rooms(&space.id)).await?;
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
        db.events_after(&space.id, &room.id, after, page)
    })
    .await?;
    Ok(Json(json!({"events": events})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostMessage {
    text: String,
    #[serde(default)]
    invoke_agent: bool,
    #[serde(default)]
    mentions: Vec<Mention>,
}

pub async fn post_message(
    State(state): State<AppState>,
    who: Human,
    Path((sid, rid)): Path<(String, String)>,
    body: JsonBody<PostMessage>,
) -> ApiResult<(StatusCode, Json<Event>)> {
    let Json(body) = body?;
    let space = member_of(&state, &sid, &who).await?;
    let room = room_of(&state, &space, &rid).await?;
    let text = check_text(&state, &body.text)?;
    let space_id = space.id.clone();
    let member_ids: Vec<String> = db(&state, move |db| db.list_members(&space_id))
        .await?
        .into_iter()
        .map(|m| m.user_id)
        .collect();
    let resolved = mentions::resolve(&text, &body.mentions, body.invoke_agent, &member_ids)
        .map_err(ApiError::bad_request)?;
    let addressed = addressed(&resolved);
    let author = who.author();
    let event = {
        let (sid, rid, author) = (space.id.clone(), room.id.clone(), author.clone());
        db(&state, move |db| {
            db.append_event(&sid, &rid, kind::MESSAGE, &author, &text, &addressed)
        })
        .await?
    };
    // Only `@agent` or Ask agent starts agent work; mentioning people never does.
    if mentions::invokes_agent(&resolved)
        && let Err(reason) = start_room_run(&state, &space.id, &room, &author, &event).await
    {
        state
            .append(
                &space.id,
                &room.id,
                kind::ERROR,
                &Author::server(actor::SYSTEM),
                &reason,
            )
            .await;
    }
    Ok((StatusCode::ACCEPTED, Json(event)))
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
            "this Space has no paired runner; its owner can issue a runner token to pair one",
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
        let _ =
            crate::tools::call_runner(&task_state, &space.id, &room.id, &caller, tool, arguments)
                .await;
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
    let events = db(&state, move |db| db.personal_after(&who.id, after, page)).await?;
    Ok(Json(json!({"events": events})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    text: String,
    #[serde(default)]
    invoke_agent: bool,
}

pub async fn chat_message(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<ChatMessage>,
) -> ApiResult<(StatusCode, Json<Event>)> {
    let Json(body) = body?;
    let text = check_text(&state, &body.text)?;
    // Personal Chat has no other members: only `@agent` or the flag resolve.
    let resolved =
        mentions::resolve(&text, &[], body.invoke_agent, &[]).map_err(ApiError::bad_request)?;
    let addressed = addressed(&resolved);
    let (uid, author) = (who.id.clone(), who.author());
    let event = db(&state, move |db| {
        db.append_personal(&uid, kind::MESSAGE, &author, &text, &addressed)
    })
    .await?;
    if mentions::invokes_agent(&resolved) {
        tokio::spawn(respond_personal(
            state.clone(),
            who.id.clone(),
            event.clone(),
        ));
    }
    Ok((StatusCode::ACCEPTED, Json(event)))
}

// ---- Runs ----

pub async fn list_runs(
    State(state): State<AppState>,
    who: Human,
) -> ApiResult<Json<serde_json::Value>> {
    let spaces = db(&state, move |db| db.list_spaces(&who.id)).await?;
    let runs: Vec<_> = state
        .runs
        .list()
        .into_iter()
        .filter(|r| spaces.iter().any(|s| s.id == r.space_id))
        .collect();
    Ok(Json(json!({"runs": runs})))
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
        .map_err(|_| not_found())?;
    if run.actor != who.id && space.role != Role::Owner {
        return Err(ApiError::forbidden(
            "only the person who started a run, or the Space owner, can stop it",
        ));
    }
    if !state.runs.stop(&id) {
        return Err(ApiError::conflict("the run is not running"));
    }
    let run = state.runs.get(&id).unwrap_or(run);
    debug_assert_eq!(run.status, RunStatus::Cancelled);
    Ok(Json(
        serde_json::to_value(run).map_err(|e| ApiError::internal(e.into()))?,
    ))
}

// ---- Runner ----

pub async fn runner_claim(State(state): State<AppState>, runner: Runner) -> Response {
    match state.jobs.claim(&runner.space_id, runner.generation) {
        Some(job) => (StatusCode::OK, Json(job)).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
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
    match state
        .jobs
        .complete(&runner.space_id, runner.generation, &id, outcome)
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
