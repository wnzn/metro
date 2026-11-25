//! Prompt to page, prompt to edit, and templates.
//!
//! A person describes a page (or a change, optionally to one selected
//! block). The server asks **their own chat model**, in their normal order
//! (the model they chose for this request; in a Space their default model
//! there; then their routing or default chat connection), with no tools,
//! no runner and no transcript: only the Page TSX reference, the names of
//! rooms, agents and their own schedules they can use, and the current
//! source when editing. A model that can't be used fails the draft; nothing
//! falls back to another model.
//!
//! The reply is compiled as the author. On diagnostics the model is asked
//! again with them, at most twice. The result is a **draft** (`ready` or
//! `failed`), visible only to its author, that they preview, diff and
//! apply. Applying compiles it again as the person applying it, and saving
//! uses the same version check as a manual save (409 on a newer head).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value as Json, json};

use super::compile::{self, Compiled};
use super::spec::{Block, PageSpec};
use super::{access, store, tsx};
use crate::AppState;
use crate::model_routing::{self as routing, AddressedBy, Route};

/// The Page TSX reference given to the model.
pub const REFERENCE: &str = include_str!("prompt.md");
/// Page builder model turns running at once, server-wide.
pub const MAX_PAGE_BUILDS: usize = 4;
/// Retries after the first attempt when the page doesn't compile.
pub const MAX_RETRIES: usize = 2;
/// Longest request, in characters.
pub const MAX_PROMPT_CHARS: usize = 4_000;
/// Rooms, agents and schedules listed per Space in the model's context.
const MAX_LISTED: usize = 40;

// ---- Templates ----

pub struct Template {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// `space`, `personal` or both.
    pub scopes: &'static [&'static str],
    source: &'static str,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "space-overview",
        title: "Space overview",
        description: "The Space's agents, active runs and latest answer.",
        scopes: &["space"],
        source: include_str!("templates/space-overview.tsx"),
    },
    Template {
        id: "release-watch",
        title: "Release watch",
        description: "Runs in one room and an agent's status answer with a button to refresh it.",
        scopes: &["space"],
        source: include_str!("templates/release-watch.tsx"),
    },
    Template {
        id: "daily-brief",
        title: "Daily brief",
        description: "An agent's daily brief with a button to write a new one.",
        scopes: &["space", "personal"],
        source: include_str!("templates/daily-brief.tsx"),
    },
    Template {
        id: "my-runs",
        title: "Runs",
        description: "Active and recent agent runs.",
        scopes: &["space", "personal"],
        source: include_str!("templates/my-runs.tsx"),
    },
    Template {
        id: "files",
        title: "Files",
        description: "Recent files in the library.",
        scopes: &["space", "personal"],
        source: include_str!("templates/files.tsx"),
    },
];

pub fn template(id: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.id == id)
}

/// A JSX attribute string for `value`: double quotes, single quotes, or
/// `None` when it contains both.
fn attr(value: &str) -> Option<String> {
    if !value.contains('"') {
        Some(format!("\"{value}\""))
    } else if !value.contains('\'') {
        Some(format!("'{value}'"))
    } else {
        None
    }
}

/// `template`'s source for a page in `space` (with its `room` and `agent`
/// handle), or for a personal page (personal Chat).
pub fn instantiate(t: &Template, space: Option<(&str, &str, &str)>) -> String {
    let body: String = t
        .source
        .lines()
        .filter(|l| !l.trim_start().starts_with("// Template"))
        .map(|l| format!("{l}\n"))
        .collect();
    let (target, room_only) = match space {
        Some((room_id, room_name, agent)) => {
            let room = attr(room_name).unwrap_or_else(|| format!("\"{room_id}\""));
            (
                format!("room={room} agent=\"{agent}\""),
                format!("room={room}"),
            )
        }
        None => ("room=\"chat\"".to_owned(), String::new()),
    };
    body.replace("__TARGET__", &target)
        .replace(
            " __ROOM_ONLY__",
            if room_only.is_empty() { "" } else { " " },
        )
        .replace("__ROOM_ONLY__", &room_only)
}

// ---- Context for the model ----

/// What the author may reference, as text for the model: names only, never
/// message content.
fn catalogue(db: &crate::db::Db, user: &str, space: Option<&str>) -> anyhow::Result<String> {
    let mut out = String::new();
    let spaces: Vec<crate::db::SpaceView> = match space {
        Some(sid) => db.membership(sid, user)?.into_iter().collect(),
        None => db.list_spaces(user)?,
    };
    match space {
        Some(_) => out.push_str("This is a Space page. Its Space:\n"),
        None => out.push_str(
            "This is a personal page. Use room=\"chat\" for the person's own Chat, or name a Space below with space=\"…\".\nSpaces the person is a member of:\n",
        ),
    }
    for s in spaces.iter().take(MAX_LISTED) {
        out.push_str(&format!("- Space \"{}\" (space=\"{}\")\n", s.name, s.id));
        let rooms = db.list_rooms(&s.id)?;
        let names: Vec<String> = rooms
            .iter()
            .take(MAX_LISTED)
            .map(|r| match &r.parent_room_id {
                Some(_) => format!("\"{}\" (thread)", r.name),
                None => format!("\"{}\"", r.name),
            })
            .collect();
        out.push_str(&format!("  rooms: {}\n", names.join(", ")));
        for a in db.space_agents(&s.id)?.iter().take(MAX_LISTED) {
            out.push_str(&format!(
                "  agent @{}{}: {}\n",
                a.handle,
                if a.is_default { " (default)" } else { "" },
                a.description
            ));
        }
    }
    let tasks = crate::schedules::list_tasks(db, user)?;
    let tasks: Vec<_> = tasks
        .iter()
        .filter(|t| match (space, &t.target) {
            (Some(sid), Some((s, _))) => s == sid,
            (Some(_), None) => false,
            (None, _) => true,
        })
        .take(MAX_LISTED)
        .collect();
    if !tasks.is_empty() {
        out.push_str("The person's own scheduled tasks:\n");
        for t in tasks {
            out.push_str(&format!("- \"{}\" (schedule=\"{}\")\n", t.name, t.id));
        }
    }
    Ok(out)
}

/// The model's reply as page source: the first fenced block, if any.
pub fn extract_source(reply: &str) -> String {
    let Some(start) = reply.find("```") else {
        return reply.trim().to_owned();
    };
    let after = &reply[start + 3..];
    let body_start = after.find('\n').map(|n| n + 1).unwrap_or(after.len());
    let body = &after[body_start..];
    match body.find("```") {
        Some(end) => body[..end].trim().to_owned(),
        None => body.trim().to_owned(),
    }
}

fn diagnostics_text(diags: &[tsx::Diagnostic]) -> String {
    diags
        .iter()
        .map(|d| format!("- {d}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---- Block-level changes ----

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Changes {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    /// `<Data>` names added, removed or changed.
    pub data: Vec<String>,
    /// The page's own title or refresh changed.
    pub page: bool,
}

fn shallow(b: &Block) -> Json {
    // A block without its keyed children; Columns stay, they're part of
    // their table.
    let mut b = b.clone();
    b.children.retain(|c| c.key.is_none());
    serde_json::to_value(b).unwrap_or(Json::Null)
}

fn flat(spec: &PageSpec) -> BTreeMap<String, Json> {
    spec.all_blocks()
        .into_iter()
        .filter_map(|b| b.key.clone().map(|k| (k, shallow(b))))
        .collect()
}

/// What changed between two specs, by block key.
pub fn changes(old: Option<&PageSpec>, new: &PageSpec) -> Changes {
    let empty = BTreeMap::new();
    let (a, b) = (old.map(flat).unwrap_or_default(), flat(new));
    let a = if old.is_some() { &a } else { &empty };
    let mut c = Changes::default();
    for (k, v) in &b {
        match a.get(k) {
            None => c.added.push(k.clone()),
            Some(o) if o != v => c.changed.push(k.clone()),
            _ => {}
        }
    }
    for k in a.keys() {
        if !b.contains_key(k) {
            c.removed.push(k.clone());
        }
    }
    let data = |s: Option<&PageSpec>| -> BTreeMap<String, Json> {
        s.map(|s| {
            s.data
                .iter()
                .map(|d| {
                    (
                        d.name.clone(),
                        serde_json::to_value(d).unwrap_or(Json::Null),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
    };
    let (da, db) = (data(old), data(Some(new)));
    let names: BTreeSet<&String> = da.keys().chain(db.keys()).collect();
    c.data = names
        .into_iter()
        .filter(|n| da.get(*n) != db.get(*n))
        .cloned()
        .collect();
    c.page = old.is_some_and(|o| o.title != new.title || o.refresh != new.refresh);
    c
}

fn subtree_keys(spec: &PageSpec, key: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Some(b) = spec.block(key) {
        let mut all = Vec::new();
        b.walk(&mut all);
        out.extend(all.into_iter().filter_map(|b| b.key.clone()));
    }
    out
}

/// Changes outside the selected block's subtree (in either version).
pub fn outside_selection(old: &PageSpec, new: &PageSpec, key: &str, c: &Changes) -> Vec<String> {
    let mut inside = subtree_keys(old, key);
    inside.extend(subtree_keys(new, key));
    let mut out: Vec<String> = c
        .added
        .iter()
        .chain(&c.removed)
        .chain(&c.changed)
        .filter(|k| !inside.contains(*k))
        .cloned()
        .collect();
    out.extend(c.data.iter().map(|d| format!("data:{d}")));
    if c.page {
        out.push("page".to_owned());
    }
    out.sort();
    out.dedup();
    out
}

// ---- Generation ----

/// The route for a page-builder turn of `user`: the model they chose for
/// this request, else in a Space their default model there, else their
/// routing (if on, and in a Space if on there) or default chat connection.
pub async fn route_for(
    state: &AppState,
    user: &str,
    space: Option<&str>,
    prompt: &str,
    chosen: Option<Route>,
) -> anyhow::Result<Route> {
    if let Some(route) = chosen {
        return Ok(route);
    }
    let settings = routing::preferences(state, user).await?;
    Ok(match space {
        Some(sid) => {
            let prefs = crate::space_prefs::load(state, sid, user).await?;
            // No agent: the per-agent step never matches.
            crate::space_prefs::route_for(
                state,
                user,
                sid,
                &prefs,
                &settings,
                "",
                prompt,
                AddressedBy::Explicit,
            )
            .await
        }
        None => {
            routing::route_turn(
                state,
                user,
                None,
                &settings,
                prompt,
                AddressedBy::Explicit,
                None,
            )
            .await
        }
    })
}

pub struct Job {
    pub draft_id: String,
    pub user: String,
    pub space: Option<String>,
    pub prompt: String,
    pub base_source: Option<String>,
    pub block_key: Option<String>,
    pub route: Route,
}

fn model_ref(note: &crate::model_routing::RouteNote) -> Option<String> {
    match (&note.connection_id, &note.model_id) {
        (Some(c), Some(m)) => Some(format!("{c}/{m}")),
        (Some(c), None) => Some(c.clone()),
        (None, Some(m)) => Some(format!("server/{m}")),
        _ => None,
    }
}

/// Generate the draft of `job` and record the result.
/// How long a generation can possibly take: every attempt at the agent
/// timeout, plus a margin. A draft generating for longer than this is no
/// longer counted against its author.
pub fn busy_window_ms(limits: &crate::Limits) -> i64 {
    let attempts = (MAX_RETRIES + 1) as u128;
    let ms = limits.agent_timeout.as_millis() * attempts + 60_000;
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// Fails its draft when dropped while still armed: a job that panics (or
/// whose task is dropped) still releases its generation slot and tells its
/// author why.
struct Guard {
    db: crate::db::Db,
    id: String,
    armed: bool,
}

/// What a draft says when its job ended without a result.
pub const STOPPED: &str = "The page builder stopped unexpectedly; try again.";

impl Drop for Guard {
    fn drop(&mut self) {
        if self.armed
            && let Err(e) = store::finish_draft(
                &self.db,
                &self.id,
                "failed",
                None,
                None,
                None,
                Some(STOPPED),
                None,
                0,
            )
        {
            tracing::error!(error = %format!("{e:#}"), "failing an abandoned page draft");
        }
    }
}

/// Run `job` (which records its own result) for draft `id`; if it never
/// finishes normally, the draft fails and its slot is freed.
pub async fn guarded(db: crate::db::Db, id: String, job: impl std::future::Future<Output = ()>) {
    let mut guard = Guard {
        db,
        id,
        armed: true,
    };
    job.await;
    guard.armed = false;
}

/// Generate the draft of `job` and record the result.
pub async fn run(state: AppState, job: Job) {
    let (db, id) = (state.db.clone(), job.draft_id.clone());
    guarded(db, id, run_inner(state, job)).await;
}

async fn run_inner(state: AppState, job: Job) {
    let result = generate(&state, &job).await;
    let id = job.draft_id.clone();
    let write = state
        .db
        .call(move |db| match result {
            Outcome::Ready {
                source,
                spec,
                model,
                attempts,
            } => store::finish_draft(
                db,
                &id,
                "ready",
                Some(&source),
                Some(&spec),
                None,
                None,
                model.as_deref(),
                attempts,
            ),
            Outcome::Invalid {
                source,
                diagnostics,
                model,
                attempts,
            } => store::finish_draft(
                db,
                &id,
                "failed",
                Some(&source),
                None,
                Some(&diagnostics),
                Some("The model's page still had problems after two retries."),
                model.as_deref(),
                attempts,
            ),
            Outcome::Error { error, attempts } => store::finish_draft(
                db,
                &id,
                "failed",
                None,
                None,
                None,
                Some(&error),
                None,
                attempts,
            ),
        })
        .await;
    if let Err(e) = write {
        tracing::error!(error = %format!("{e:#}"), "recording a page draft");
    }
}

enum Outcome {
    Ready {
        source: String,
        spec: String,
        model: Option<String>,
        attempts: i64,
    },
    Invalid {
        source: String,
        diagnostics: String,
        model: Option<String>,
        attempts: i64,
    },
    Error {
        error: String,
        attempts: i64,
    },
}

async fn compile_as(
    state: &AppState,
    user: &str,
    space: Option<&str>,
    source: String,
) -> anyhow::Result<Compiled> {
    let (user, space) = (user.to_owned(), space.map(str::to_owned));
    state
        .db
        .call(move |db| {
            let mut r = access::DbResolver {
                db,
                user: &user,
                space,
            };
            Ok(compile::compile(&source, &mut r))
        })
        .await
}

async fn generate(state: &AppState, job: &Job) -> Outcome {
    let (db_user, db_space) = (job.user.clone(), job.space.clone());
    let context = match state
        .db
        .call(move |db| catalogue(db, &db_user, db_space.as_deref()))
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "page builder context");
            return Outcome::Error {
                error: "The page builder couldn't read what this page may use.".to_owned(),
                attempts: 0,
            };
        }
    };
    let preamble = format!("{REFERENCE}\n# Context\n\n{context}");
    let mut request = String::new();
    if let Some(base) = &job.base_source {
        request.push_str("Current page source:\n```tsx\n");
        request.push_str(base);
        request.push_str("\n```\n\n");
        if let Some(key) = &job.block_key {
            let excerpt = tsx::element_source(base, key).unwrap_or_default();
            request.push_str(&format!(
                "Change only the block with key=\"{key}\" (and what's inside it); keep every \
                 other block, key and <Data> exactly as it is. The selected block:\n```tsx\n{excerpt}\n```\n\n"
            ));
        } else {
            request
                .push_str("Change the page as asked; keep existing keys for blocks you keep.\n\n");
        }
        request.push_str("Request: ");
    } else {
        request.push_str("Write a new page. Request: ");
    }
    request.push_str(&job.prompt);

    let mut history: Vec<(bool, String)> = Vec::new();
    let mut prompt = request;
    let mut last: Option<(String, String)> = None;
    let mut model = None;
    for attempt in 0..=MAX_RETRIES {
        let attempts = attempt as i64 + 1;
        let reply = crate::agent::page_builder_turn(
            state,
            &job.user,
            job.space.as_deref(),
            job.route.clone(),
            &preamble,
            history.clone(),
            prompt.clone(),
        )
        .await;
        let (reply, note) = match reply {
            Ok(r) => r,
            Err(error) => return Outcome::Error { error, attempts },
        };
        model = model_ref(&note);
        let source = extract_source(&reply);
        let compiled =
            match compile_as(state, &job.user, job.space.as_deref(), source.clone()).await {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), "compiling a generated page");
                    return Outcome::Error {
                        error: "The page couldn't be checked.".to_owned(),
                        attempts,
                    };
                }
            };
        if let Some(spec) = &compiled.spec {
            return Outcome::Ready {
                source: compiled.source.clone(),
                spec: serde_json::to_string(spec).unwrap_or_default(),
                model,
                attempts,
            };
        }
        let diags = serde_json::to_string(&compiled.diagnostics).unwrap_or_default();
        last = Some((source.clone(), diags));
        history.push((true, prompt));
        history.push((false, reply));
        prompt = format!(
            "Metro couldn't use that page:\n{}\nReturn the whole corrected page in one ```tsx block.",
            diagnostics_text(&compiled.diagnostics)
        );
    }
    let (source, diagnostics) = last.unwrap_or_default();
    Outcome::Invalid {
        source,
        diagnostics,
        model,
        attempts: (MAX_RETRIES + 1) as i64,
    }
}

/// The JSON view of a draft for its author, with a diff against its base.
pub fn draft_view(d: &store::DraftRow, base: Option<&store::VersionRow>) -> Json {
    let spec: Option<PageSpec> = d.spec.as_deref().and_then(|s| serde_json::from_str(s).ok());
    let base_spec: Option<PageSpec> = base.and_then(|b| serde_json::from_str(&b.spec).ok());
    let (changes, outside) = match &spec {
        Some(new) => {
            let c = changes(base_spec.as_ref(), new);
            let outside = match (&d.block_key, &base_spec) {
                (Some(k), Some(old)) => outside_selection(old, new, k, &c),
                _ => Vec::new(),
            };
            (Some(c), outside)
        }
        None => (None, Vec::new()),
    };
    let diagnostics: Json = d
        .diagnostics
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(json!([]));
    json!({
        "id": d.id,
        "page_id": d.page_id,
        "scope": d.scope.as_str(),
        "space_id": d.space_id,
        "base_version": d.base_version,
        "block_key": d.block_key,
        "prompt": d.prompt,
        "status": d.status,
        "source": d.source,
        "spec": spec,
        "diagnostics": diagnostics,
        "error": d.error,
        "model": d.model_ref,
        "attempts": d.attempts,
        "created_at": crate::db::rfc3339_utc(d.created_at),
        "base_source": base.map(|b| b.source.clone()),
        "changes": changes,
        "outside_selection": outside,
    })
}
