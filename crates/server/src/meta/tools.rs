//! The Metro-in-Chat tools: schemas, the shared call path (budget, Chat
//! events, untrusted output), and the schedule, page and settings tools.
//! Reads are in [`super::reads`], posting in [`super::send`], runs in
//! [`super::runs`].

use rig_core::message::ToolName;
use rig_core::tool::{DynamicTool, ToolExecutionError, ToolOutput};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::budget::Kind;
use super::grant::{Denied, Grant};
use super::{Ctx, MetaNote, reads, runs, send, untrusted};
use crate::db::{Addressed, Author, actor, kind, rfc3339_utc};
use crate::pages::store::{self, DraftCard, DraftError, DraftKind, Need, Scope};
use crate::schedules::{self, ScheduleError, ScheduleIn, Task};
use crate::tools::truncate;

/// What a tool returns: the model's data, and the Chat note about it.
pub(super) struct Out {
    pub value: Value,
    pub spaces: Vec<String>,
    pub count: Option<usize>,
    /// The `tool_result` text in the person's Chat: what was done, never
    /// content (e.g. "Read #general in Design").
    pub summary: String,
}

impl Out {
    pub fn new(value: Value, summary: impl Into<String>) -> Self {
        Self {
            value,
            spaces: Vec::new(),
            count: None,
            summary: summary.into(),
        }
    }

    pub fn spaces(mut self, spaces: Vec<String>) -> Self {
        self.spaces = spaces;
        self
    }

    /// `value` with its list under `items`, counted.
    pub fn list(items: Vec<Value>, mut extra: Value, summary: impl Into<String>) -> Self {
        let n = items.len();
        extra["items"] = Value::Array(items);
        Self {
            count: Some(n),
            ..Self::new(extra, summary)
        }
    }
}

pub(super) type R = Result<Out, String>;

pub(super) const INVALID: &str = "invalid arguments; use the tool schema";

/// Arguments of `T`'s shape, unknown fields refused; `null` is `{}`.
pub(super) fn args<T: DeserializeOwned>(v: Value) -> Result<T, String> {
    let v = if v.is_null() { json!({}) } else { v };
    serde_json::from_value(v).map_err(|_| INVALID.to_owned())
}

pub(super) fn internal(e: anyhow::Error) -> String {
    tracing::error!(error = %format!("{e:#}"), "Metro in Chat tool");
    "the operation couldn't be completed".to_owned()
}

/// Run `f` on the database; an internal error becomes the tool's error.
pub(super) async fn on_db<T, F>(ctx: &Ctx, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&crate::db::Db, &Grant) -> anyhow::Result<Result<T, String>> + Send + 'static,
{
    let grant = Grant::new(&ctx.user);
    ctx.state
        .db
        .call(move |db| f(db, &grant))
        .await
        .map_err(internal)?
}

pub(super) fn denied(d: Denied) -> String {
    d.text().to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    SpacesList,
    SpaceRead,
    CatchUp,
    Search,
    WaitingOnMe,
    Send,
    RunsList,
    RunCancel,
    RunRetry,
    SchedulesList,
    ScheduleCreate,
    ScheduleSetEnabled,
    PagesFind,
    PagePropose,
    SettingsExplain,
}

impl Tool {
    const ALL: [Tool; 15] = [
        Tool::SpacesList,
        Tool::SpaceRead,
        Tool::CatchUp,
        Tool::Search,
        Tool::WaitingOnMe,
        Tool::Send,
        Tool::RunsList,
        Tool::RunCancel,
        Tool::RunRetry,
        Tool::SchedulesList,
        Tool::ScheduleCreate,
        Tool::ScheduleSetEnabled,
        Tool::PagesFind,
        Tool::PagePropose,
        Tool::SettingsExplain,
    ];

    fn name(self) -> &'static str {
        match self {
            Tool::SpacesList => "spaces_list",
            Tool::SpaceRead => "space_read",
            Tool::CatchUp => "spaces_catch_up",
            Tool::Search => "spaces_search",
            Tool::WaitingOnMe => "waiting_on_me",
            Tool::Send => "space_send",
            Tool::RunsList => "runs_list",
            Tool::RunCancel => "run_cancel",
            Tool::RunRetry => "run_retry",
            Tool::SchedulesList => "schedules_list",
            Tool::ScheduleCreate => "schedule_create",
            Tool::ScheduleSetEnabled => "schedule_set_enabled",
            Tool::PagesFind => "pages_find",
            Tool::PagePropose => "page_propose",
            Tool::SettingsExplain => "settings_explain",
        }
    }

    fn kind(self) -> Kind {
        match self {
            Tool::Search => Kind::Search,
            Tool::Send => Kind::Send,
            _ => Kind::Read,
        }
    }

    fn definition(self) -> (&'static str, Value) {
        let id = json!({"type": "string"});
        let (description, properties, required): (&str, Value, Value) = match self {
            Tool::SpacesList => (
                "List the user's Spaces (that Metro may use), with their rooms, agent threads and \
                 agents. Use the ids it returns in the other tools.",
                json!({}),
                json!([]),
            ),
            Tool::SpaceRead => (
                "Read recent messages of one room or thread, newest last. Page back with \
                 before_event_id.",
                json!({"space_id": id, "room_id": id, "before_event_id": {"type": "integer"},
                       "limit": {"type": "integer", "minimum": 1, "maximum": reads::READ_MAX}}),
                json!(["space_id", "room_id"]),
            ),
            Tool::CatchUp => (
                "What's new in the user's Spaces since a time (default: their previous Chat \
                 message, at most 7 days back): per room, new message counts, mentions of the \
                 user and the latest messages.",
                json!({"since": {"type": "string", "description": "RFC 3339 time"},
                       "space_ids": {"type": "array", "items": id}}),
                json!([]),
            ),
            Tool::Search => (
                "Search messages in the user's Spaces. Semantic when the user has an embeddings \
                 connection, otherwise literal text.",
                json!({"query": {"type": "string", "maxLength": reads::QUERY_MAX},
                       "space_ids": {"type": "array", "items": id},
                       "limit": {"type": "integer", "minimum": 1, "maximum": reads::SEARCH_MAX}}),
                json!(["query"]),
            ),
            Tool::WaitingOnMe => (
                "Messages that mention the user or reply to them, with no later message from \
                 the user in that room or thread.",
                json!({"days": {"type": "integer", "minimum": 1, "maximum": reads::WAITING_DAYS_MAX}}),
                json!([]),
            ),
            Tool::Send => (
                "Post a message into a Space room or thread now, as the user, marked via Metro. \
                 It asks exactly one agent: agent_id, else the agent whose message reply_to \
                 answers, else the Space's default agent. Only to a \
                 Space the user named in their own messages. With wait, wait up to two minutes \
                 for the agent's answer.",
                json!({"space_id": id, "room_id": id,
                       "text": {"type": "string", "maxLength": send::TEXT_MAX},
                       "agent_id": id,
                       "mention_user_ids": {"type": "array", "maxItems": send::MENTIONS_MAX, "items": id},
                       "wait": {"type": "boolean"},
                       "reply_to": {"type": "integer"}}),
                json!(["space_id", "room_id", "text"]),
            ),
            Tool::RunsList => (
                "Agent runs in the user's Spaces since the server started (they aren't kept \
                 across restarts).",
                json!({"filter": {"type": "string", "enum": ["active", "failed", "recent"]}}),
                json!([]),
            ),
            Tool::RunCancel => (
                "Stop a running agent run the user started, or any run in a Space they own.",
                json!({"run_id": id}),
                json!(["run_id"]),
            ),
            Tool::RunRetry => (
                "Start a failed or cancelled run the user started again, as a new run.",
                json!({"run_id": id}),
                json!(["run_id"]),
            ),
            Tool::SchedulesList => ("The user's scheduled prompts.", json!({}), json!([])),
            Tool::ScheduleCreate => (
                "Create a scheduled prompt in the user's Chat, or in a Space room the user named. \
                 schedule is {\"kind\":\"once\",\"at\":RFC 3339 with offset} or \
                 {\"kind\":\"cron\",\"expression\":\"m h dom mon dow\",\"timezone\":IANA zone}.",
                json!({"name": {"type": "string"},
                       "prompt": {"type": "string", "maxLength": PROMPT_MAX},
                       "destination": {"description": "\"chat\" or {\"space_id\",\"room_id\"}"},
                       "schedule": {"type": "object"}}),
                json!(["name", "prompt", "destination", "schedule"]),
            ),
            Tool::ScheduleSetEnabled => (
                "Pause or resume a scheduled prompt. Give the revision from schedules_list.",
                json!({"id": id, "enabled": {"type": "boolean"}, "revision": {"type": "integer"}}),
                json!(["id", "enabled", "revision"]),
            ),
            Tool::PagesFind => (
                "Find the user's Pages (personal and in their Spaces) by title.",
                json!({"query": {"type": "string", "maxLength": PAGE_QUERY_MAX}}),
                json!(["query"]),
            ),
            Tool::PagePropose => (
                "Propose a page draft for the user to review: an edit (page_id) or a new page \
                 (title, and space_id for a Space page). Metro's page builder writes it from \
                 request. Only the user can apply it.",
                json!({"page_id": id, "space_id": id, "title": {"type": "string"},
                       "request": {"type": "string", "maxLength": REQUEST_MAX}}),
                json!(["request"]),
            ),
            Tool::SettingsExplain => (
                "The user's own Metro settings, read-only, and where to change them. Topics: \
                 connections, routing, classification, web, spaces, schedules, pages.",
                json!({"topic": {"type": "string"}}),
                json!([]),
            ),
        };
        (
            description,
            json!({"type": "object", "properties": properties, "required": required,
                   "additionalProperties": false}),
        )
    }
}

/// The Metro-in-Chat tools of one Chat turn.
pub fn tools(ctx: &Ctx) -> Vec<DynamicTool> {
    Tool::ALL
        .into_iter()
        .map(|tool| {
            let (description, parameters) = tool.definition();
            let ctx = ctx.clone();
            DynamicTool::new(
                ToolName::new(tool.name()).expect("tool names are nonempty"),
                description,
                parameters,
                move |args| {
                    let ctx = ctx.clone();
                    Box::pin(async move { call(&ctx, tool, args).await })
                },
            )
        })
        .collect()
}

async fn call(ctx: &Ctx, tool: Tool, args: Value) -> Result<ToolOutput, ToolExecutionError> {
    let note = |spaces: Vec<String>, count: Option<usize>| MetaNote {
        tool: tool.name().to_owned(),
        spaces,
        count,
    };
    if let Err(why) = ctx.budget.admit(tool.kind()) {
        return Err(refuse(ctx, note(Vec::new(), None), why).await);
    }
    record(ctx, kind::TOOL_CALL, tool.name(), note(Vec::new(), None)).await;
    let result = match tool {
        Tool::SpacesList => reads::spaces_list(ctx).await,
        Tool::SpaceRead => reads::space_read(ctx, args).await,
        Tool::CatchUp => reads::catch_up(ctx, args).await,
        Tool::Search => reads::search(ctx, args).await,
        Tool::WaitingOnMe => reads::waiting_on_me(ctx, args).await,
        Tool::Send => send::space_send(ctx, args).await,
        Tool::RunsList => runs::list(ctx, args).await,
        Tool::RunCancel => runs::cancel(ctx, args).await,
        Tool::RunRetry => runs::retry(ctx, args).await,
        Tool::SchedulesList => schedules_list(ctx).await,
        Tool::ScheduleCreate => schedule_create(ctx, args).await,
        Tool::ScheduleSetEnabled => schedule_set_enabled(ctx, args).await,
        Tool::PagesFind => pages_find(ctx, args).await,
        Tool::PagePropose => page_propose(ctx, args).await,
        Tool::SettingsExplain => settings_explain(ctx, args).await,
    };
    match result {
        Ok(mut out) => {
            let text = render(ctx, &mut out.value);
            record(
                ctx,
                kind::TOOL_RESULT,
                &out.summary,
                note(out.spaces, out.count),
            )
            .await;
            Ok(ToolOutput::text(text))
        }
        Err(why) => Err(refuse(ctx, note(Vec::new(), None), why).await),
    }
}

/// `value` as the model sees it: delimited untrusted data that fits both
/// the per-result cap and what's left of the turn's budget. A list under
/// `items` loses its last items first (`truncated: true`).
pub(super) fn render(ctx: &Ctx, value: &mut Value) -> String {
    let len = |v: &Value| untrusted(&v.to_string()).len();
    // Room for the delimiters is part of the size measured.
    let max = ctx.state.config.limits.max_tool_output_to_model;
    let wanted = len(value);
    let allowed = ctx.budget.take_output(wanted, max);
    if wanted > allowed {
        if let Some(items) = value.get_mut("items").and_then(Value::as_array_mut) {
            items.pop();
        }
        value["truncated"] = json!(true);
        while len(value) > allowed
            && value["items"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        {
            if let Some(items) = value["items"].as_array_mut() {
                items.pop();
            }
        }
    }
    let text = untrusted(&value.to_string());
    if text.len() <= allowed.max(1) {
        text
    } else {
        truncate(&text, allowed)
    }
}

/// A Chat event of this turn with `note`.
async fn record(ctx: &Ctx, event_kind: &'static str, text: &str, note: MetaNote) {
    let author = Author::server(if event_kind == kind::ERROR {
        actor::SYSTEM
    } else {
        actor::AGENT
    });
    let (user, text, activity) = (
        ctx.user.clone(),
        truncate(text, 2_000),
        ctx.turn.with_meta(note),
    );
    let r = ctx
        .state
        .db
        .call(move |db| {
            db.append_personal_with(
                &user,
                event_kind,
                &author,
                &text,
                &Addressed::room(),
                Some(&activity),
            )
        })
        .await;
    if let Err(e) = r {
        tracing::error!(error = %format!("{e:#}"), "appending Metro in Chat event");
    }
}

async fn refuse(ctx: &Ctx, note: MetaNote, why: String) -> ToolExecutionError {
    record(
        ctx,
        kind::ERROR,
        &format!("{} refused: {why}", note.tool),
        note,
    )
    .await;
    ToolExecutionError::other(why)
}

// ---- Schedules ----

pub(super) const PROMPT_MAX: usize = 4_000;
const SCHEDULES_MAX: usize = 50;

fn schedule_error(e: ScheduleError) -> String {
    match e {
        ScheduleError::BadRequest(m) | ScheduleError::Conflict(m) => m,
        ScheduleError::NotFound(m) => m.to_owned(),
        ScheduleError::Internal(e) => internal(e),
    }
}

/// A task as the model sees it; `space` names its Space destination.
fn task_view(t: &Task, space: Option<&str>) -> Value {
    let full = t.view();
    let destination = match (&t.target, space) {
        (Some((sid, rid)), Some(name)) => {
            json!({"space_id": sid, "space_name": name, "room_id": rid})
        }
        _ => json!("chat"),
    };
    json!({"id": t.id, "name": t.name, "destination": destination, "schedule": full["schedule"],
           "enabled": t.enabled, "next_run_at": full["next_run_at"], "revision": t.revision})
}

async fn schedules_list(ctx: &Ctx) -> R {
    on_db(ctx, |db, grant| {
        let tasks = schedules::list_tasks(db, &grant.user)?;
        let spaces = match grant.spaces(&db.lock())? {
            Ok(s) => s,
            Err(d) => return Ok(Err(denied(d))),
        };
        let items: Vec<Value> = tasks
            .iter()
            .filter_map(|t| match &t.target {
                None => Some(task_view(t, None)),
                Some((sid, _)) => spaces
                    .iter()
                    .find(|s| &s.id == sid)
                    .map(|s| task_view(t, Some(&s.name))),
            })
            .take(SCHEDULES_MAX)
            .collect();
        Ok(Ok(Out::list(items, json!({}), "Checked scheduled prompts")))
    })
    .await
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Destination {
    Named(String),
    Space(SpaceDest),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpaceDest {
    space_id: String,
    room_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    name: String,
    prompt: String,
    destination: Destination,
    schedule: ScheduleIn,
}

async fn schedule_create(ctx: &Ctx, a: Value) -> R {
    let a: CreateArgs = args(a)?;
    if a.prompt.trim().chars().count() > PROMPT_MAX {
        return Err(format!("prompt must be at most {PROMPT_MAX} characters"));
    }
    let target = match a.destination {
        Destination::Named(s) if s == "chat" => None,
        Destination::Named(_) => return Err(INVALID.to_owned()),
        Destination::Space(d) => Some((d.space_id, d.room_id)),
    };
    let own = ctx.own_text.clone();
    let max = ctx.state.config.limits.max_message_chars.min(PROMPT_MAX);
    on_db(ctx, move |db, grant| {
        let space_name = match &target {
            None => {
                if let Err(d) = grant.on(&db.lock())? {
                    return Ok(Err(denied(d)));
                }
                None
            }
            Some((sid, _)) => {
                let space = match grant.check(&db.lock(), sid)? {
                    Ok(s) => s,
                    Err(d) => return Ok(Err(denied(d))),
                };
                if !send::named_by_user(&own, &space) {
                    return Ok(Err(send::NOT_NAMED.to_owned()));
                }
                Some(space.name)
            }
        };
        let (space_id, room_id) = target.clone().unzip();
        let body = schedules::CreateBody {
            name: a.name,
            prompt: a.prompt,
            space_id,
            room_id,
            schedule: a.schedule,
            enabled: true,
        };
        Ok(
            match schedules::create_task(db, &grant.user, body, chrono::Utc::now(), max) {
                Ok(t) => Ok(Out::new(
                    task_view(&t, space_name.as_deref()),
                    format!("Scheduled \"{}\"", t.name),
                )
                .spaces(space_name.into_iter().collect())),
                Err(e) => Err(schedule_error(e)),
            },
        )
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnableArgs {
    id: String,
    enabled: bool,
    revision: i64,
}

async fn schedule_set_enabled(ctx: &Ctx, a: Value) -> R {
    let a: EnableArgs = args(a)?;
    let max = ctx.state.config.limits.max_message_chars.min(PROMPT_MAX);
    on_db(ctx, move |db, grant| {
        let (task, space) = {
            let conn = db.lock();
            let task = schedules::get_task_in(&conn, &a.id)?.filter(|t| t.owner_id == grant.user);
            let Some(task) = task else {
                return Ok(Err("no such scheduled task".to_owned()));
            };
            let space = match &task.target {
                None => match grant.on(&conn)? {
                    Ok(()) => None,
                    Err(d) => return Ok(Err(denied(d))),
                },
                Some((sid, _)) => match grant.check(&conn, sid)? {
                    Ok(s) => Some(s.name),
                    // A task in a Space Metro may not use is not visible.
                    Err(Denied::NoSpace) => return Ok(Err("no such scheduled task".to_owned())),
                    Err(d) => return Ok(Err(denied(d))),
                },
            };
            if task.revision != a.revision {
                return Ok(Err(
                    "the task changed since it was listed; list it again and ask the user"
                        .to_owned(),
                ));
            }
            (task, space)
        };
        let patch = schedules::PatchBody {
            enabled: Some(a.enabled),
            ..Default::default()
        };
        Ok(
            match schedules::update_task(db, &grant.user, &task.id, patch, chrono::Utc::now(), max)
            {
                Ok(t) => Ok(Out::new(
                    task_view(&t, space.as_deref()),
                    format!(
                        "{} \"{}\"",
                        if a.enabled { "Resumed" } else { "Paused" },
                        t.name
                    ),
                )
                .spaces(space.into_iter().collect())),
                Err(e) => Err(schedule_error(e)),
            },
        )
    })
    .await
}

// ---- Pages ----

const PAGE_QUERY_MAX: usize = 200;
const PAGES_MAX: usize = 20;
const REQUEST_MAX: usize = 4_000;
const TITLE_MAX: usize = 120;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    query: String,
}

async fn pages_find(ctx: &Ctx, a: Value) -> R {
    let a: FindArgs = args(a)?;
    let q = a.query.trim().to_lowercase();
    if q.is_empty() || q.chars().count() > PAGE_QUERY_MAX {
        return Err(format!("query must be 1..={PAGE_QUERY_MAX} characters"));
    }
    on_db(ctx, move |db, grant| {
        let pages = store::list_for(db, &grant.user, false)?;
        let conn = db.lock();
        let spaces = match grant.spaces(&conn)? {
            Ok(s) => s,
            Err(d) => return Ok(Err(denied(d))),
        };
        let mut names = Vec::new();
        let mut items = Vec::new();
        for p in pages.iter().filter(|p| p.title.to_lowercase().contains(&q)) {
            // Read access is decided again now, as the person.
            if crate::pages::access::rights_in(&conn, p, &grant.user)?.is_none() {
                continue;
            }
            let space = match &p.space_id {
                None => Value::Null,
                Some(sid) => match spaces.iter().find(|s| &s.id == sid) {
                    Some(s) => {
                        if !names.contains(&s.name) {
                            names.push(s.name.clone());
                        }
                        json!({"id": s.id, "name": s.name})
                    }
                    None => continue,
                },
            };
            items.push(json!({"page_id": p.id, "title": p.title, "space": space,
                              "updated_at": rfc3339_utc(p.updated_at)}));
            if items.len() == PAGES_MAX {
                break;
            }
        }
        Ok(Ok(
            Out::list(items, json!({}), "Searched your Pages").spaces(names)
        ))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposeArgs {
    page_id: Option<String>,
    space_id: Option<String>,
    title: Option<String>,
    request: String,
}

async fn page_propose(ctx: &Ctx, a: Value) -> R {
    let a: ProposeArgs = args(a)?;
    let request = a.request.trim().to_owned();
    if request.is_empty() || request.chars().count() > REQUEST_MAX {
        return Err(format!("request must be 1..={REQUEST_MAX} characters"));
    }
    if a.page_id.is_some() && (a.space_id.is_some() || a.title.is_some()) {
        return Err("an edit takes page_id only; a new page takes title and space_id".to_owned());
    }
    let title = match (&a.page_id, a.title.as_deref().map(str::trim)) {
        (None, Some(t)) if !t.is_empty() && t.chars().count() <= TITLE_MAX && !t.contains('\n') => {
            Some(t.to_owned())
        }
        (None, _) => {
            return Err(format!(
                "a new page needs a title of 1..={TITLE_MAX} characters"
            ));
        }
        (Some(_), _) => None,
    };
    let prompt = match &title {
        Some(t) => format!("Title: {t}\n\n{request}"),
        None => request,
    };
    let window = crate::pages::generate::busy_window_ms(&ctx.state.config.limits);
    let (page_id, space_id) = (a.page_id, a.space_id);
    let (draft, base, card, space_name) = on_db(ctx, move |db, grant| {
        let mut conn = db.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Err(d) = grant.on(&tx)? {
            return Ok(Err(denied(d)));
        }
        let no_page = || Ok(Err("no such page".to_owned()));
        // The page's (or new page's) Space must be one Metro may use.
        let (space, title) = match &page_id {
            Some(pid) => {
                let Ok((page, _)) = store::load_in(&tx, pid, &grant.user, Need::Edit)? else {
                    return no_page();
                };
                (page.space_id.clone(), page.title)
            }
            None => (space_id.clone(), title.clone().unwrap_or_default()),
        };
        let space_name = match &space {
            None => None,
            Some(sid) => match grant.check(&tx, sid)? {
                Ok(s) => Some(s.name),
                Err(Denied::NoSpace) if page_id.is_some() => return no_page(),
                Err(d) => return Ok(Err(denied(d))),
            },
        };
        let reserved = store::reserve_draft_in(
            &tx,
            &store::NewDraft {
                page_id: page_id.as_deref(),
                space_id: space.as_deref(),
                author_id: &grant.user,
                block_key: None,
                prompt: &prompt,
                busy_window_ms: window,
            },
            None,
        )?;
        let (draft, base) = match reserved {
            Ok(r) => r,
            Err(DraftError::Denied(store::Denied::Gone)) => return no_page(),
            Err(DraftError::Denied(store::Denied::Forbidden)) => {
                return Ok(Err("the user can't edit this page".to_owned()));
            }
            Err(DraftError::Busy) => {
                return Ok(Err(
                    "the user already has pages being generated; try again later".to_owned(),
                ));
            }
            Err(DraftError::Stale | DraftError::NoBlock) => return no_page(),
        };
        tx.commit()?;
        let card = DraftCard {
            draft_id: draft.id.clone(),
            title,
            summary: "Draft proposed in your Chat. Review it, then Apply it if you want it."
                .to_owned(),
            kind: if draft.page_id.is_some() {
                DraftKind::Edit
            } else {
                DraftKind::New
            },
            page_id: draft.page_id.clone(),
            author_id: draft.author_id.clone(),
            expires_at: rfc3339_utc(draft.created_at + store::DRAFT_RETENTION_MS)
                .unwrap_or_default(),
        };
        Ok(Ok((draft, base, card, space_name)))
    })
    .await?;
    // The card in the person's Chat; only they can open or apply the draft.
    let mut activity = ctx.turn.clone();
    activity.page_draft = Some(card.clone());
    let (user, summary) = (ctx.user.clone(), card.summary.clone());
    if let Err(e) = ctx
        .state
        .db
        .call(move |db| {
            db.append_personal_with(
                &user,
                kind::PAGE_DRAFT,
                &Author::server(actor::AGENT),
                &summary,
                &Addressed::room(),
                Some(&activity),
            )
        })
        .await
    {
        tracing::error!(error = %format!("{e:#}"), "appending a Chat page draft card");
    }
    let base_spaces = base
        .as_ref()
        .and_then(|b| serde_json::from_str(&b.spec).ok())
        .map(|spec| crate::pages::generate::spec_spaces(&spec))
        .unwrap_or_default();
    tokio::spawn(crate::pages::generate::run(
        ctx.state.clone(),
        crate::pages::generate::Job {
            draft_id: draft.id.clone(),
            user: ctx.user.clone(),
            space: draft.space_id.clone(),
            prompt: draft.prompt.clone(),
            base_source: base.map(|b| b.source),
            block_key: None,
            chosen: None,
            base_spaces,
            room_origin: None,
        },
    ));
    let scope = if draft.scope == Scope::Personal {
        "personal"
    } else {
        "space"
    };
    Ok(Out::new(
        json!({"draft_id": draft.id, "page_id": draft.page_id, "scope": scope,
               "status": draft.status, "applied": false,
               "summary": "A draft is being built. The user reviews it and decides whether to apply it."}),
        "Proposed a page draft",
    )
    .spaces(space_name.into_iter().collect()))
}

// ---- Settings ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExplainArgs {
    topic: Option<String>,
}

const HELP: [(&str, &str); 7] = [
    (
        "connections",
        "Settings → Models: add model connections (chat, System One, embeddings), choose defaults, \
         and share your own with Spaces or the organization. Whoever owns a connection sees the \
         requests that go through it and may pay for them.",
    ),
    (
        "routing",
        "Settings → Models → Model choices: use-case routing picks which of your chat connections \
         answers; a model chosen for one turn, or for an agent or Space, comes first.",
    ),
    (
        "classification",
        "Space settings → Your preferences: let your System One connection decide whether your \
         unaddressed messages in that Space are for an agent.",
    ),
    (
        "web",
        "Settings → Web search: run web search on your own computer, and turn on web tools for \
         this Chat.",
    ),
    (
        "spaces",
        "Settings → Metro in Chat: turn \"Use my Spaces in Chat\" on or off, and choose which \
         Spaces Metro can use.",
    ),
    (
        "schedules",
        "Scheduled tasks in the sidebar: create, pause, edit or delete scheduled prompts.",
    ),
    (
        "pages",
        "Pages in the sidebar: review drafts, then Apply or discard them.",
    ),
];

pub(super) async fn settings_explain(ctx: &Ctx, a: Value) -> R {
    let a: ExplainArgs = args(a)?;
    let help: Vec<Value> = HELP
        .iter()
        .filter(|(t, _)| a.topic.as_deref().is_none_or(|topic| topic.trim() == *t))
        .map(|(t, text)| json!({"topic": t, "where": text}))
        .collect();
    let routing = crate::model_routing::preferences(&ctx.state, &ctx.user)
        .await
        .map_err(internal)?;
    on_db(ctx, move |db, grant| {
        let conn = db.lock();
        let spaces = match grant.spaces(&conn)? {
            Ok(s) => s,
            Err(d) => return Ok(Err(denied(d))),
        };
        // Only names and roles of the person's own connections: never a URL,
        // key, endpoint, device or anyone else's connection.
        let mut stmt = conn.prepare_cached(
            "SELECT c.name, c.capability, c.is_default,
                    EXISTS (SELECT 1 FROM provider_shares s WHERE s.connection_id = c.id)
             FROM provider_connections c WHERE c.user_id = ?1 ORDER BY c.created_at, c.rowid",
        )?;
        let connections: Vec<Value> = stmt
            .query_map([&grant.user], |r| {
                Ok(json!({"name": r.get::<_, String>(0)?, "capability": r.get::<_, String>(1)?,
                          "default": r.get::<_, bool>(2)?, "shared_by_me": r.get::<_, bool>(3)?}))
            })?
            .collect::<Result<_, _>>()?;
        let mut classification = Vec::new();
        for s in &spaces {
            let prefs = crate::space_prefs::load_in(&conn, &s.id, &grant.user)?;
            classification.push(json!({"space": s.name, "classify_unaddressed": prefs.classify_unaddressed,
                                       "routing": prefs.routing}));
        }
        let excluded: i64 = conn.query_row(
            "SELECT COUNT(*) FROM meta_space_exclusions x JOIN space_members m
               ON m.space_id = x.space_id AND m.user_id = x.user_id WHERE x.user_id = ?1",
            [&grant.user],
            |r| r.get(0),
        )?;
        let web = crate::web_devices::personal_chat_enabled(&conn, &grant.user)?;
        Ok(Ok(Out::new(
            json!({
                "connections": connections,
                "use_case_routing": routing.enabled,
                "system_one_connection_set": routing.classifier_id.is_some(),
                "embeddings_connection_set": routing.embedding_id.is_some(),
                "space_preferences": classification,
                "web_tools_in_chat": web,
                "spaces_in_chat": {"enabled": true, "usable_spaces": spaces.len(), "excluded_spaces": excluded},
                "help": help,
                "note": "Metro can't change settings; tell the user where to change them.",
            }),
            "Checked your settings",
        )))
    })
    .await
}
