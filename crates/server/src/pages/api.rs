//! `/v1/pages`: pages, versions, data, actions, drafts and templates.
//!
//! Every request rechecks access now ([`super::access`]): outsiders and
//! unknown ids get 404, people who may see a page but not do the thing
//! get 403. Saves are optimistic: a save based on an older head gets 409
//! with the current `head_version`, never a silent overwrite.

use axum::extract::rejection::QueryRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Deserializer};
use serde_json::{Value as Json_, json};

use super::access::{self, Rights};
use super::compile::{self, Compiled};
use super::limits::{self, PageLimits};
use super::spec::PageSpec;
use super::store::{self, CreateError, NewVersion, PageRow, VersionRow};
use super::{actions, data, generate, tsx};
use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody, db, member_of};
use crate::auth::Human;
use crate::db::rfc3339_utc;

/// Bodies that carry page source: the source limit plus JSON escaping.
const SOURCE_BODY_LIMIT: usize = tsx::MAX_SOURCE_BYTES * 2 + 4 * 1024;

pub fn router() -> Router<AppState> {
    let big = || DefaultBodyLimit::max(SOURCE_BODY_LIMIT);
    Router::new()
        .route("/v1/pages", get(list).post(create).layer(big()))
        .route("/v1/pages/templates", get(templates))
        .route("/v1/pages/validate", post(validate).layer(big()))
        .route("/v1/pages/drafts", post(create_draft))
        .route("/v1/pages/drafts/{id}", get(get_draft).delete(delete_draft))
        .route("/v1/pages/drafts/{id}/apply", post(apply_draft))
        .route("/v1/pages/drafts/{id}/data", post(draft_data))
        .route(
            "/v1/pages/{pid}",
            get(get_page).patch(patch_page).delete(delete_page),
        )
        .route("/v1/pages/{pid}/undelete", post(undelete))
        .route(
            "/v1/pages/{pid}/versions",
            get(list_versions).post(save_version).layer(big()),
        )
        .route("/v1/pages/{pid}/versions/{n}", get(get_version))
        .route(
            "/v1/pages/{pid}/versions/{n}/restore",
            post(restore_version),
        )
        .route("/v1/pages/{pid}/diff", get(diff))
        .route("/v1/pages/{pid}/data", post(page_data))
        .route("/v1/pages/{pid}/actions", post(run_action))
        .route("/v1/pages/{pid}/prefs", put(put_prefs))
}

const NO_PAGE: &str = "no such page";
const NO_DRAFT: &str = "no such draft";

fn forbidden(m: &str) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, m)
}

fn conflict_head(head: i64) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "the page changed since this edit was started; review the newest version and try again",
            "head_version": head,
        })),
    )
        .into_response()
}

fn invalid(c: &Compiled) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "error": "the page has problems",
            "diagnostics": c.diagnostics,
            "source": c.source,
        })),
    )
        .into_response()
}

/// The page `pid` and `who`'s rights on it; 404 when they may not see it.
async fn load(state: &AppState, who: &Human, pid: &str) -> ApiResult<(PageRow, Rights)> {
    if !compile::valid_page_id(pid) {
        return Err(ApiError::not_found(NO_PAGE));
    }
    let (pid, uid) = (pid.to_owned(), who.id.clone());
    db(state, move |db| {
        let Some(page) = store::get(db, &pid)? else {
            return Ok(None);
        };
        Ok(access::rights(db, &page, &uid)?.map(|r| (page, r)))
    })
    .await?
    .ok_or_else(|| ApiError::not_found(NO_PAGE))
}

/// Like [`load`], for a live (not deleted) page.
async fn load_live(state: &AppState, who: &Human, pid: &str) -> ApiResult<(PageRow, Rights)> {
    let (page, rights) = load(state, who, pid).await?;
    if page.deleted_at.is_some() {
        return Err(ApiError::not_found(NO_PAGE));
    }
    Ok((page, rights))
}

async fn head_of(state: &AppState, page: &PageRow) -> ApiResult<VersionRow> {
    let (pid, n) = (page.id.clone(), page.head_version);
    db(state, move |db| store::version(db, &pid, n))
        .await?
        .ok_or_else(|| ApiError::internal(anyhow::anyhow!("a page without its head version")))
}

fn spec_of(v: &VersionRow) -> ApiResult<PageSpec> {
    serde_json::from_str(&v.spec).map_err(|e| ApiError::internal(e.into()))
}

fn meta(page: &PageRow, rights: &Rights, prefs: &store::Prefs) -> Json_ {
    json!({
        "id": page.id,
        "scope": page.scope.as_str(),
        "space_id": page.space_id,
        "creator_id": page.creator_id,
        "title": page.title,
        "icon": page.icon,
        "members_edit": page.members_edit,
        "head_version": page.head_version,
        "created_at": rfc3339_utc(page.created_at),
        "updated_at": rfc3339_utc(page.updated_at),
        "deleted_at": page.deleted_at.and_then(rfc3339_utc),
        "can_edit": rights.can_edit,
        "can_delete": rights.can_delete,
        "can_manage": rights.can_manage,
        "pinned": prefs.pinned,
        "position": prefs.position,
    })
}

fn list_meta(v: &store::VersionMeta) -> Json_ {
    json!({
        "version": v.version,
        "author_id": v.author_id,
        "kind": v.kind,
        "prompt": v.prompt,
        "model": v.model_ref,
        "restored_from": v.restored_from,
        "created_at": rfc3339_utc(v.created_at),
    })
}

fn version_meta(v: &VersionRow) -> Json_ {
    json!({
        "version": v.version,
        "author_id": v.author_id,
        "kind": v.kind,
        "prompt": v.prompt,
        "model": v.model_ref,
        "restored_from": v.restored_from,
        "created_at": rfc3339_utc(v.created_at),
    })
}

async fn full_view(
    state: &AppState,
    who: &Human,
    page: &PageRow,
    rights: &Rights,
) -> ApiResult<Json_> {
    let head = versions_of(state, who, &page.id, vec![page.head_version])
        .await?
        .remove(0);
    let (uid, pid) = (who.id.clone(), page.id.clone());
    let prefs = db(state, move |db| store::prefs(db, &uid, &pid)).await?;
    let mut v = meta(page, rights, &prefs);
    v["version"] = version_meta(&head);
    v["source"] = json!(head.source);
    v["spec"] = serde_json::from_str(&head.spec).unwrap_or(Json_::Null);
    Ok(v)
}

/// Compile `source` as `who` for a page in `space` (or personal).
async fn compile_as(
    state: &AppState,
    who: &Human,
    space: Option<String>,
    source: String,
) -> ApiResult<Compiled> {
    let uid = who.id.clone();
    db(state, move |db| {
        let mut r = access::DbResolver {
            db,
            user: &uid,
            space,
        };
        Ok(compile::compile(&source, &mut r))
    })
    .await
}

fn new_version(who: &Human, kind: &'static str, c: &Compiled) -> ApiResult<NewVersion> {
    let spec = c
        .spec
        .as_ref()
        .ok_or_else(|| ApiError::internal(anyhow::anyhow!("saving an invalid page")))?;
    Ok(NewVersion {
        author_id: who.id.clone(),
        kind,
        prompt: None,
        model_ref: None,
        restored_from: None,
        source: c.source.clone(),
        spec: serde_json::to_string(spec).map_err(|e| ApiError::internal(e.into()))?,
        title: spec.title.clone(),
    })
}

fn too_many(e: CreateError) -> ApiError {
    match e {
        CreateError::TooMany(n) => ApiError::new(
            StatusCode::CONFLICT,
            format!("this Space (or person) already has {n} pages, the most this server allows"),
        ),
        CreateError::NotAllowed => ApiError::not_found("no such Space"),
    }
}

/// A refused save: 409 with the head, or 403.
fn save_refused(e: store::SaveError) -> Response {
    match e {
        store::SaveError::Conflict { head } => conflict_head(head),
        store::SaveError::Denied(store::Denied::Gone) => {
            ApiError::not_found(NO_PAGE).into_response()
        }
        store::SaveError::Denied(store::Denied::Forbidden) => {
            forbidden("you can't edit this page").into_response()
        }
    }
}

// ---- Scope ----

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum WireScope {
    Personal,
    Space,
}

/// The Space a new page or draft is for, checked: `None` for personal.
async fn scope_space(
    state: &AppState,
    who: &Human,
    scope: WireScope,
    space_id: Option<String>,
) -> ApiResult<Option<String>> {
    match (scope, space_id) {
        (WireScope::Personal, None) => Ok(None),
        (WireScope::Personal, Some(_)) => Err(ApiError::bad_request(
            "space_id is only for scope \"space\"",
        )),
        (WireScope::Space, None) => Err(ApiError::bad_request("scope \"space\" needs space_id")),
        (WireScope::Space, Some(sid)) => Ok(Some(member_of(state, &sid, who).await?.id)),
    }
}

// ---- Pages ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    #[serde(default)]
    deleted: bool,
}

async fn list(
    State(state): State<AppState>,
    who: Human,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> ApiResult<Json<Json_>> {
    let Query(q) = query?;
    let uid = who.id.clone();
    let rows = db(&state, move |db| {
        store::purge_expired(db, crate::db::now_ms())?;
        let pages = store::list_for(db, &uid, q.deleted)?;
        let prefs = store::all_prefs(db, &uid)?;
        let mut out = Vec::new();
        for p in pages {
            if let Some(r) = access::rights(db, &p, &uid)? {
                let pr = prefs.get(&p.id).cloned().unwrap_or_default();
                out.push(meta(&p, &r, &pr));
            }
        }
        Ok(out)
    })
    .await?;
    Ok(Json(json!({"pages": rows})))
}

async fn templates(_who: Human) -> Json<Json_> {
    let list: Vec<Json_> = generate::TEMPLATES
        .iter()
        .map(|t| {
            json!({"id": t.id, "title": t.title, "description": t.description, "scopes": t.scopes})
        })
        .collect();
    Json(json!({"templates": list}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    scope: WireScope,
    #[serde(default)]
    space_id: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    template: Option<String>,
    /// Templates in a Space: the room and agent to use (names).
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    agent: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<CreateBody>,
) -> ApiResult<Response> {
    let Json(body) = body?;
    let space = scope_space(&state, &who, body.scope, body.space_id).await?;
    let (source, kind) = match (body.source, body.template) {
        (Some(s), None) => {
            if body.room.is_some() || body.agent.is_some() {
                return Err(ApiError::bad_request(
                    "room and agent are only for templates",
                ));
            }
            (s, "create")
        }
        (None, Some(t)) => {
            let Some(t) = generate::template(&t) else {
                return Err(ApiError::not_found("no such template"));
            };
            let wire = if space.is_some() { "space" } else { "personal" };
            if !t.scopes.contains(&wire) {
                return Err(ApiError::bad_request(format!(
                    "the template \"{}\" is for {} pages",
                    t.id,
                    t.scopes.join(" and ")
                )));
            }
            let target = match &space {
                Some(sid) => {
                    let (sid, room, agent) = (sid.clone(), body.room.clone(), body.agent.clone());
                    let picked = db(&state, move |db| {
                        let rooms = db.list_rooms(&sid)?;
                        let room = match room {
                            Some(name) => rooms
                                .into_iter()
                                .find(|r| r.name.eq_ignore_ascii_case(&name)),
                            None => rooms.into_iter().find(|r| r.parent_room_id.is_none()),
                        };
                        let agents = db.space_agents(&sid)?;
                        let agent = match agent {
                            Some(h) => agents
                                .into_iter()
                                .find(|a| a.handle.eq_ignore_ascii_case(h.trim_start_matches('@'))),
                            None => agents.into_iter().find(|a| a.is_default),
                        };
                        Ok(room.zip(agent))
                    })
                    .await?;
                    let Some((room, agent)) = picked else {
                        return Err(ApiError::not_found("no such room or agent in this Space"));
                    };
                    Some((room.id, room.name, agent.handle))
                }
                None => None,
            };
            let source = generate::instantiate(
                t,
                target
                    .as_ref()
                    .map(|(i, n, a)| (i.as_str(), n.as_str(), a.as_str())),
            );
            (source, "template")
        }
        _ => {
            return Err(ApiError::bad_request(
                "give exactly one of source or template",
            ));
        }
    };
    let compiled = compile_as(&state, &who, space.clone(), source).await?;
    if compiled.spec.is_none() {
        return Ok(invalid(&compiled));
    }
    let version = new_version(&who, kind, &compiled)?;
    let limits = limits::current(&state).await;
    let created = db(&state, move |db| {
        store::create(db, space.as_deref(), &version, &limits)
    })
    .await?
    .map_err(too_many)?;
    let rights = access::Rights {
        can_edit: true,
        can_delete: true,
        can_manage: true,
    };
    let view = full_view(&state, &who, &created, &rights).await?;
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidateBody {
    scope: WireScope,
    #[serde(default)]
    space_id: Option<String>,
    source: String,
}

/// Check a source without saving anything.
async fn validate(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<ValidateBody>,
) -> ApiResult<Json<Json_>> {
    let Json(body) = body?;
    let space = scope_space(&state, &who, body.scope, body.space_id).await?;
    let c = compile_as(&state, &who, space, body.source).await?;
    Ok(Json(json!({
        "ok": c.spec.is_some(),
        "source": c.source,
        "spec": c.spec,
        "diagnostics": c.diagnostics,
    })))
}

async fn get_page(
    State(state): State<AppState>,
    who: Human,
    Path(pid): Path<String>,
) -> ApiResult<Json<Json_>> {
    let (page, rights) = load_live(&state, &who, &pid).await?;
    Ok(Json(full_view(&state, &who, &page, &rights).await?))
}

fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchBody {
    /// Renaming saves a new version with the `<Page title>` changed.
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    base_version: Option<i64>,
    #[serde(default, deserialize_with = "present")]
    icon: Option<Option<String>>,
    #[serde(default)]
    members_edit: Option<bool>,
}

/// `source` with its `<Page title>` replaced.
fn retitle(source: &str, title: &str) -> Result<String, &'static str> {
    let root = tsx::parse(source).map_err(|_| "the current source doesn't parse")?;
    let a = root.attr("title").ok_or("the page has no title")?;
    // `title="…"`, `title='…'` and `title={"…"}` alike: the whole attribute
    // is replaced.
    // Canonical: any title round-trips through the parser.
    let quoted = tsx::jsx_attr_string(title);
    Ok(format!(
        "{}title={quoted}{}",
        &source[..a.at],
        &source[a.end..]
    ))
}

async fn patch_page(
    State(state): State<AppState>,
    who: Human,
    Path(pid): Path<String>,
    body: JsonBody<PatchBody>,
) -> ApiResult<Response> {
    let (page, _) = load_live(&state, &who, &pid).await?;
    let Json(body) = body?;
    if let Some(Some(icon)) = &body.icon
        && (icon.chars().count() > 16 || tsx::invalid_char_in(icon).is_some())
    {
        return Err(ApiError::bad_request(
            "icon is at most 16 printable characters, without control, invisible or \
             text-direction characters",
        ));
    }
    let mut rename = None;
    if let Some(title) = &body.title {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > compile::MAX_TITLE_CHARS {
            return Err(ApiError::bad_request(format!(
                "title must be 1..={} characters",
                compile::MAX_TITLE_CHARS
            )));
        }
        let head = head_of(&state, &page).await?;
        let base = body.base_version.unwrap_or(head.version);
        if base != head.version {
            return Ok(conflict_head(head.version));
        }
        let source = retitle(&head.source, title).map_err(ApiError::bad_request)?;
        let compiled = compile_as(&state, &who, page.space_id.clone(), source).await?;
        if compiled.spec.is_none() {
            return Ok(invalid(&compiled));
        }
        // A rename: it doesn't count as reviewing the page's buttons.
        rename = Some((base, new_version(&who, "rename", &compiled)?));
    }
    // Everything at once: rights, liveness and the head are checked in the
    // same transaction as the writes, and nothing is written unless all may be.
    let ops = store::PatchOps {
        rename,
        icon: body.icon,
        members_edit: body.members_edit,
    };
    let limits = limits::current(&state).await;
    let (pid, uid) = (page.id.clone(), who.id.clone());
    match db(&state, move |db| {
        store::patch(db, &pid, &uid, &ops, &limits)
    })
    .await?
    {
        Ok(()) => {}
        Err(store::PatchError::Denied(store::Denied::Gone)) => {
            return Err(ApiError::not_found(NO_PAGE));
        }
        Err(store::PatchError::Denied(store::Denied::Forbidden)) => {
            return Err(forbidden(
                "you can't change this: editing needs edit rights, and who edits needs the \
                 page's creator or the Space owner",
            ));
        }
        Err(store::PatchError::Conflict { head }) => return Ok(conflict_head(head)),
        Err(store::PatchError::NotSpacePage) => {
            return Err(ApiError::bad_request(
                "members_edit is only for Space pages",
            ));
        }
    }
    let (page, rights) = load_live(&state, &who, &page.id).await?;
    Ok(Json(full_view(&state, &who, &page, &rights).await?).into_response())
}

async fn delete_page(
    State(state): State<AppState>,
    who: Human,
    Path(pid): Path<String>,
) -> ApiResult<StatusCode> {
    if !compile::valid_page_id(&pid) {
        return Err(ApiError::not_found(NO_PAGE));
    }
    let uid = who.id.clone();
    match db(&state, move |db| store::soft_delete(db, &pid, &uid)).await? {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(store::Denied::Gone) => Err(ApiError::not_found(NO_PAGE)),
        Err(store::Denied::Forbidden) => Err(forbidden(
            "only the page's creator or the Space owner can delete it",
        )),
    }
}

async fn undelete(
    State(state): State<AppState>,
    who: Human,
    Path(pid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Json<Json_>> {
    crate::api::empty_object(&body)?;
    if !compile::valid_page_id(&pid) {
        return Err(ApiError::not_found(NO_PAGE));
    }
    let limits: PageLimits = limits::current(&state).await;
    let (id, uid) = (pid.clone(), who.id.clone());
    match db(&state, move |db| store::undelete(db, &id, &uid, &limits)).await? {
        Ok(()) => {}
        Err(store::UndeleteError::Denied(_)) => return Err(ApiError::not_found(NO_PAGE)),
        Err(store::UndeleteError::NotDeleted) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "the page isn't deleted",
            ));
        }
        Err(store::UndeleteError::TooMany(n)) => return Err(too_many(CreateError::TooMany(n))),
    }
    let (page, rights) = load_live(&state, &who, &pid).await?;
    Ok(Json(full_view(&state, &who, &page, &rights).await?))
}

// ---- Versions ----

async fn list_versions(
    State(state): State<AppState>,
    who: Human,
    Path(pid): Path<String>,
) -> ApiResult<Json<Json_>> {
    if !compile::valid_page_id(&pid) {
        return Err(ApiError::not_found(NO_PAGE));
    }
    let uid = who.id.clone();
    let (head, versions) = db(&state, move |db| store::version_list_for(db, &pid, &uid))
        .await?
        .map_err(|_| ApiError::not_found(NO_PAGE))?;
    let list: Vec<Json_> = versions.iter().map(list_meta).collect();
    Ok(Json(json!({"head_version": head, "versions": list})))
}

/// Versions `ns` of page `pid` (`0` = the head) for `who`, read under the
/// same lock as their grant check: a removal can't land in between.
async fn versions_of(
    state: &AppState,
    who: &Human,
    pid: &str,
    ns: Vec<i64>,
) -> ApiResult<Vec<VersionRow>> {
    let (id, uid) = (pid.to_owned(), who.id.clone());
    match db(state, move |db| store::versions_for(db, &id, &uid, &ns)).await? {
        Ok(found) => found
            .into_iter()
            .map(|v| v.ok_or_else(|| ApiError::not_found("no such version")))
            .collect(),
        Err(_) => Err(ApiError::not_found(NO_PAGE)),
    }
}

async fn version_n(state: &AppState, who: &Human, page: &PageRow, n: i64) -> ApiResult<VersionRow> {
    if n < 1 {
        return Err(ApiError::not_found("no such version"));
    }
    Ok(versions_of(state, who, &page.id, vec![n]).await?.remove(0))
}

async fn get_version(
    State(state): State<AppState>,
    who: Human,
    Path((pid, n)): Path<(String, i64)>,
) -> ApiResult<Json<Json_>> {
    if !compile::valid_page_id(&pid) || n < 1 {
        return Err(ApiError::not_found(NO_PAGE));
    }
    let v = versions_of(&state, &who, &pid, vec![n]).await?.remove(0);
    let mut out = version_meta(&v);
    out["source"] = json!(v.source);
    out["spec"] = serde_json::from_str(&v.spec).unwrap_or(Json_::Null);
    Ok(Json(out))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveBody {
    base_version: i64,
    source: String,
}

async fn save(
    state: &AppState,
    who: &Human,
    page: &PageRow,
    base: i64,
    mut version: NewVersion,
) -> ApiResult<Response> {
    let limits = limits::current(state).await;
    let id = page.id.clone();
    version.author_id = who.id.clone();
    match db(state, move |db| {
        store::add_version(db, &id, base, &version, &limits)
    })
    .await?
    {
        Ok(_) => {
            let (page, rights) = load_live(state, who, &page.id).await?;
            Ok((
                StatusCode::CREATED,
                Json(full_view(state, who, &page, &rights).await?),
            )
                .into_response())
        }
        Err(e) => Ok(save_refused(e)),
    }
}

async fn save_version(
    State(state): State<AppState>,
    who: Human,
    Path(pid): Path<String>,
    body: JsonBody<SaveBody>,
) -> ApiResult<Response> {
    let (page, rights) = load_live(&state, &who, &pid).await?;
    if !rights.can_edit {
        return Err(forbidden("you can't edit this page"));
    }
    let Json(body) = body?;
    if body.base_version != page.head_version {
        return Ok(conflict_head(page.head_version));
    }
    let compiled = compile_as(&state, &who, page.space_id.clone(), body.source).await?;
    if compiled.spec.is_none() {
        return Ok(invalid(&compiled));
    }
    let version = new_version(&who, "manual", &compiled)?;
    save(&state, &who, &page, body.base_version, version).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BaseBody {
    base_version: i64,
}

async fn restore_version(
    State(state): State<AppState>,
    who: Human,
    Path((pid, n)): Path<(String, i64)>,
    body: JsonBody<BaseBody>,
) -> ApiResult<Response> {
    let (page, rights) = load_live(&state, &who, &pid).await?;
    if !rights.can_edit {
        return Err(forbidden("you can't edit this page"));
    }
    let Json(body) = body?;
    let old = version_n(&state, &who, &page, n).await?;
    if body.base_version != page.head_version {
        return Ok(conflict_head(page.head_version));
    }
    // Compiled again as the person restoring it, with their access now.
    let compiled = compile_as(&state, &who, page.space_id.clone(), old.source).await?;
    if compiled.spec.is_none() {
        return Ok(invalid(&compiled));
    }
    let mut version = new_version(&who, "restore", &compiled)?;
    version.restored_from = Some(n);
    save(&state, &who, &page, body.base_version, version).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffQuery {
    from: i64,
    #[serde(default)]
    to: Option<i64>,
}

async fn diff(
    State(state): State<AppState>,
    who: Human,
    Path(pid): Path<String>,
    query: Result<Query<DiffQuery>, QueryRejection>,
) -> ApiResult<Json<Json_>> {
    let Query(q) = query?;
    if !compile::valid_page_id(&pid) || q.from < 1 || q.to.is_some_and(|t| t < 1) {
        return Err(ApiError::not_found(NO_PAGE));
    }
    // Both versions and the grant in one read.
    let mut found = versions_of(&state, &who, &pid, vec![q.from, q.to.unwrap_or(0)]).await?;
    let (a, b) = (found.remove(0), found.remove(0));
    let (sa, sb) = (spec_of(&a)?, spec_of(&b)?);
    Ok(Json(json!({
        "from": {"version": a.version, "source": a.source},
        "to": {"version": b.version, "source": b.source},
        "changes": generate::changes(Some(&sa), &sb),
    })))
}

// ---- Data, actions, preferences ----

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DataBody {
    #[serde(default)]
    sources: Option<Vec<String>>,
}

fn data_body(body: &[u8]) -> ApiResult<DataBody> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(DataBody::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("bad body: {e}")))
}

async fn page_data(
    State(state): State<AppState>,
    who: Human,
    Path(pid): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Json<Json_>> {
    let body = data_body(&body)?;
    let (page, _) = load_live(&state, &who, &pid).await?;
    let head = head_of(&state, &page).await?;
    let spec = spec_of(&head)?;
    let ctx = data::Context {
        page_id: Some(page.id.clone()),
        space_id: page.space_id.clone(),
        version: Some(head.version),
        draft: false,
    };
    let mut out = data::resolve(&state, ctx, spec, who.id.clone(), body.sources)
        .await
        .map_err(ApiError::internal)?;
    out["version"] = json!(head.version);
    Ok(Json(out))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionBody {
    version: i64,
    block_key: String,
    #[serde(default)]
    confirm: bool,
    /// For a schedule button: the task revision the confirmation showed.
    #[serde(default)]
    schedule_revision: Option<i64>,
}

async fn run_action(
    State(state): State<AppState>,
    who: Human,
    Path(pid): Path<String>,
    body: JsonBody<ActionBody>,
) -> ApiResult<Response> {
    if !compile::valid_page_id(&pid) {
        return Err(ApiError::not_found(NO_PAGE));
    }
    let Json(body) = body?;
    let click = actions::Click {
        version: body.version,
        block_key: body.block_key,
        confirm: body.confirm,
        schedule_revision: body.schedule_revision,
    };
    actions::run(&state, &who, &pid, click).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrefsBody {
    pinned: bool,
    #[serde(default)]
    position: Option<i64>,
}

async fn put_prefs(
    State(state): State<AppState>,
    who: Human,
    Path(pid): Path<String>,
    body: JsonBody<PrefsBody>,
) -> ApiResult<Json<Json_>> {
    if !compile::valid_page_id(&pid) {
        return Err(ApiError::not_found(NO_PAGE));
    }
    let Json(body) = body?;
    let p = store::Prefs {
        pinned: body.pinned,
        position: body.position,
    };
    let (uid, saved) = (who.id.clone(), p.clone());
    db(&state, move |db| store::put_prefs(db, &uid, &pid, &saved))
        .await?
        .map_err(|_| ApiError::not_found(NO_PAGE))?;
    Ok(Json(json!({"pinned": p.pinned, "position": p.position})))
}

// ---- Drafts ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftBody {
    /// Edit this page; otherwise a new page in `scope`.
    #[serde(default)]
    page_id: Option<String>,
    #[serde(default)]
    scope: Option<WireScope>,
    #[serde(default)]
    space_id: Option<String>,
    prompt: String,
    /// Change only this block.
    #[serde(default)]
    block_key: Option<String>,
    /// The model for this request (one of the person's usable connections).
    #[serde(default)]
    provider_connection_id: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
}

/// An explicit model choice, checked before anything is stored.
async fn chosen_route(
    state: &AppState,
    who: &Human,
    space: Option<&str>,
    id: Option<String>,
    model_id: Option<String>,
    effort: Option<String>,
) -> ApiResult<Option<crate::model_routing::Route>> {
    let Some(id) = id else {
        if model_id.is_some() || effort.is_some() {
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
    let mut route =
        crate::model_routing::Route::chosen(crate::model_routing::AddressedBy::Explicit, &id);
    if model_id.is_some() || effort.is_some() {
        route.turn_selection = Some(
            crate::providers::turn::check(
                state,
                &who.id,
                space,
                &id,
                model_id.as_deref(),
                effort.as_deref(),
            )
            .await
            .map_err(crate::providers::api::use_error)?,
        );
        return Ok(Some(route));
    }
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

async fn create_draft(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<DraftBody>,
) -> ApiResult<Response> {
    let Json(body) = body?;
    let prompt = body.prompt.trim().to_owned();
    if prompt.is_empty() || prompt.chars().count() > generate::MAX_PROMPT_CHARS {
        return Err(ApiError::bad_request(format!(
            "prompt must be 1..={} characters",
            generate::MAX_PROMPT_CHARS
        )));
    }
    let (page_id, space) = match &body.page_id {
        Some(pid) => {
            if body.scope.is_some() || body.space_id.is_some() {
                return Err(ApiError::bad_request(
                    "an edit takes page_id, not scope or space_id",
                ));
            }
            let (page, _) = load_live(&state, &who, pid).await?;
            (Some(page.id), page.space_id)
        }
        None => {
            if body.block_key.is_some() {
                return Err(ApiError::bad_request("block_key needs page_id"));
            }
            let scope = body
                .scope
                .ok_or_else(|| ApiError::bad_request("give page_id, or scope for a new page"))?;
            (None, scope_space(&state, &who, scope, body.space_id).await?)
        }
    };
    // An explicit model choice is checked (no model is called) before the
    // slot is taken.
    let chosen = chosen_route(
        &state,
        &who,
        space.as_deref(),
        body.provider_connection_id,
        body.model_id,
        body.reasoning_effort,
    )
    .await?;
    // Reserve the generation slot, with the page, edit rights and
    // membership checked in the same transaction. Any model-backed routing
    // (use-case routing may ask System One) happens only after this, in the
    // job, under the reservation.
    let (uid, pid, sid, bk, pr) = (
        who.id.clone(),
        page_id.clone(),
        space.clone(),
        body.block_key.clone(),
        prompt.clone(),
    );
    let window = generate::busy_window_ms(&state.config.limits);
    let draft = match db(&state, move |db| {
        store::create_draft(
            db,
            &store::NewDraft {
                page_id: pid.as_deref(),
                space_id: sid.as_deref(),
                author_id: &uid,
                block_key: bk.as_deref(),
                prompt: &pr,
                busy_window_ms: window,
            },
        )
    })
    .await?
    {
        Ok(d) => d,
        Err(store::DraftError::Busy) => {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "you already have pages being generated; wait for them to finish",
            ));
        }
        Err(store::DraftError::NoBlock) => {
            return Err(ApiError::bad_request(
                "block_key isn't a block of this page",
            ));
        }
        Err(store::DraftError::Denied(store::Denied::Gone)) => {
            return Err(ApiError::not_found(NO_PAGE));
        }
        Err(store::DraftError::Denied(store::Denied::Forbidden)) => {
            return Err(forbidden("you can't edit this page"));
        }
    };
    let base = draft_base(&state, &draft).await?;
    let job = generate::Job {
        draft_id: draft.id.clone(),
        user: who.id.clone(),
        space,
        prompt,
        base_source: base.as_ref().map(|b| b.source.clone()),
        block_key: body.block_key,
        chosen,
    };
    tokio::spawn(generate::run(state.clone(), job));
    Ok((
        StatusCode::ACCEPTED,
        Json(generate::draft_view(&draft, base.as_ref())),
    )
        .into_response())
}

/// `who`'s own draft `id`; 404 for anyone else's.
async fn own_draft(state: &AppState, who: &Human, id: &str) -> ApiResult<store::DraftRow> {
    let id = id.to_owned();
    let d = db(state, move |db| store::draft(db, &id)).await?;
    match d {
        Some(d) if d.author_id == who.id => Ok(d),
        _ => Err(ApiError::not_found(NO_DRAFT)),
    }
}

async fn draft_base(state: &AppState, d: &store::DraftRow) -> ApiResult<Option<VersionRow>> {
    let (Some(pid), Some(n)) = (d.page_id.clone(), d.base_version) else {
        return Ok(None);
    };
    db(state, move |db| store::version(db, &pid, n)).await
}

/// A draft's page (if any) still visible to its author, and their access
/// to the draft's Space now.
async fn draft_access(
    state: &AppState,
    who: &Human,
    d: &store::DraftRow,
) -> ApiResult<Option<(PageRow, Rights)>> {
    if let Some(sid) = &d.space_id {
        member_of(state, sid, who)
            .await
            .map_err(|_| ApiError::not_found(NO_DRAFT))?;
    }
    match &d.page_id {
        Some(pid) => Ok(Some(
            load_live(state, who, pid)
                .await
                .map_err(|_| ApiError::not_found(NO_DRAFT))?,
        )),
        None => Ok(None),
    }
}

async fn get_draft(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<Json<Json_>> {
    let d = own_draft(&state, &who, &id).await?;
    draft_access(&state, &who, &d).await?;
    let base = draft_base(&state, &d).await?;
    Ok(Json(generate::draft_view(&d, base.as_ref())))
}

async fn delete_draft(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let uid = who.id.clone();
    if db(&state, move |db| store::discard_draft(db, &id, &uid)).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(NO_DRAFT))
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ApplyBody {
    /// Apply even though the draft changes blocks outside the selected one.
    #[serde(default)]
    confirm_outside: bool,
}

async fn apply_draft(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Response> {
    let body: ApplyBody = if body.iter().all(u8::is_ascii_whitespace) {
        ApplyBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|e| ApiError::bad_request(format!("bad body: {e}")))?
    };
    let d = own_draft(&state, &who, &id).await?;
    draft_access(&state, &who, &d).await?;
    if d.status != "ready" {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "the draft isn't ready to apply",
        ));
    }
    let source = d.source.clone().unwrap_or_default();
    // Checked again as the person applying it, with their access now.
    let compiled = compile_as(&state, &who, d.space_id.clone(), source).await?;
    let Some(new) = compiled.spec.as_ref() else {
        return Ok(invalid(&compiled));
    };
    if let Some(k) = &d.block_key
        && let Some(old) = draft_base(&state, &d).await?
    {
        let old = spec_of(&old)?;
        let c = generate::changes(Some(&old), new);
        let outside = generate::outside_selection(&old, new, k, &c);
        if !outside.is_empty() && !body.confirm_outside {
            return Ok((
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "this draft also changes blocks outside the selected one; apply with confirm_outside to keep them",
                    "outside_selection": outside,
                })),
            )
                .into_response());
        }
    }
    let mut version = new_version(
        &who,
        if d.page_id.is_some() {
            "prompt"
        } else {
            "create"
        },
        &compiled,
    )?;
    version.prompt = Some(d.prompt.chars().take(4_000).collect());
    version.model_ref = d.model_ref.clone();
    // One transaction: the draft is still ready and theirs, the page is
    // created or the version saved with every check, then the draft is
    // consumed. Any refusal leaves the draft as it was.
    let limits = limits::current(&state).await;
    let (did, uid) = (d.id.clone(), who.id.clone());
    let page_id = match db(&state, move |db| {
        store::apply_draft(db, &did, &uid, &version, &limits)
    })
    .await?
    {
        Ok(page_id) => page_id,
        Err(store::ApplyError::Gone) => return Err(ApiError::not_found(NO_DRAFT)),
        Err(store::ApplyError::NotReady) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "the draft isn't ready to apply",
            ));
        }
        Err(store::ApplyError::Denied(store::Denied::Gone)) => {
            return Err(ApiError::not_found(NO_PAGE));
        }
        Err(store::ApplyError::Denied(store::Denied::Forbidden)) => {
            return Err(forbidden("you can't edit this page"));
        }
        Err(store::ApplyError::Conflict { head }) => return Ok(conflict_head(head)),
        Err(store::ApplyError::TooMany(n)) => return Err(too_many(CreateError::TooMany(n))),
    };
    let (page, rights) = load_live(&state, &who, &page_id).await?;
    Ok((
        StatusCode::CREATED,
        Json(full_view(&state, &who, &page, &rights).await?),
    )
        .into_response())
}

async fn draft_data(
    State(state): State<AppState>,
    who: Human,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Json<Json_>> {
    let body = data_body(&body)?;
    let d = own_draft(&state, &who, &id).await?;
    draft_access(&state, &who, &d).await?;
    let Some(spec) = d
        .spec
        .as_deref()
        .and_then(|s| serde_json::from_str::<PageSpec>(s).ok())
    else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "the draft has no page to preview yet",
        ));
    };
    let ctx = data::Context {
        page_id: d.page_id.clone(),
        space_id: d.space_id.clone(),
        version: None,
        draft: true,
    };
    let out = data::resolve(&state, ctx, spec, who.id.clone(), body.sources)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(out))
}
