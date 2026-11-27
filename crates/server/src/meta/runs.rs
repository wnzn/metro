//! Runs across the person's Spaces: list, cancel and retry, with the same
//! rules as `GET /v1/runs`, `POST /v1/runs/{id}/stop` and
//! `POST /v1/runs/{id}/retry`, limited to Spaces Metro may use.

use serde::Deserialize;
use serde_json::{Value, json};

use super::Ctx;
use super::grant::Denied;
use super::tools::{Out, R, args, denied, on_db};
use crate::auth::Human;
use crate::db::{Role, rfc3339_utc};
use crate::orchestration::RunStatus;
use crate::search::clip;

const LIST_MAX: usize = 30;
const TASK: usize = 200;
const NO_RUN: &str = "no such run";

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Filter {
    Active,
    Failed,
    #[default]
    Recent,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    filter: Filter,
}

pub(super) async fn list(ctx: &Ctx, a: Value) -> R {
    let a: ListArgs = args(a)?;
    let runs = ctx.state.runs.list_with_failures();
    let user = ctx.user.clone();
    on_db(ctx, move |db, grant| {
        let conn = db.lock();
        let spaces = match grant.spaces(&conn)? {
            Ok(s) => s,
            Err(d) => return Ok(Err(denied(d))),
        };
        let mut items = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for (run, failure) in runs {
            let keep = match a.filter {
                Filter::Active => run.status == RunStatus::Running,
                Filter::Failed => matches!(run.status, RunStatus::Failed | RunStatus::Cancelled),
                Filter::Recent => true,
            };
            let Some(space) = spaces.iter().find(|s| s.id == run.space_id).filter(|_| keep) else {
                continue;
            };
            let room = crate::db::Db::room_in(&conn, &space.id, &run.room_id)?;
            // A deleted trigger's text never reaches the model.
            let task = match crate::orchestration::trigger_live(&conn, &run)? {
                true => clip(&run.task, TASK, "…"),
                false => String::new(),
            };
            let agent = match &run.agent_id {
                Some(id) => crate::space_agents::get_in(&conn, &space.id, id)?
                    .map(|a| json!({"id": a.id, "handle": a.handle, "name": a.name})),
                None => None,
            };
            let mut v = json!({
                "run_id": run.id, "space": {"id": space.id, "name": space.name},
                "room": room.map(|r| json!({"id": r.id, "name": if r.is_reply_thread() { "Thread".to_owned() } else { r.name }})),
                "agent": agent, "task": task, "status": run.status,
                "started_at": rfc3339_utc(run.started_at), "started_by_you": run.actor == user,
            });
            // The full reason is for the person who started it only.
            if let Some(f) = failure.filter(|_| run.actor == user) {
                v["failure"] = json!(f);
            }
            if !names.contains(&space.name) {
                names.push(space.name.clone());
            }
            items.push(v);
            if items.len() == LIST_MAX {
                break;
            }
        }
        Ok(Ok(Out::list(
            items,
            json!({"note": "Runs are kept in memory only; a server restart clears this list."}),
            "Checked runs",
        )
        .spaces(names)))
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    run_id: String,
}

pub(super) async fn cancel(ctx: &Ctx, a: Value) -> R {
    let a: RunArgs = args(a)?;
    let run = ctx.state.runs.get(&a.run_id).ok_or(NO_RUN)?;
    let (sid, user) = (run.space_id.clone(), ctx.user.clone());
    let space = on_db(ctx, move |db, grant| {
        Ok(match grant.check(&db.lock(), &sid)? {
            Ok(s) => Ok(s),
            Err(Denied::NoSpace) => Err(NO_RUN.to_owned()),
            Err(d) => Err(denied(d)),
        })
    })
    .await?;
    if run.actor != user && space.role != Role::Owner {
        return Err(
            "only the person who started a run, or the Space owner, can stop it".to_owned(),
        );
    }
    let stopped = ctx
        .state
        .runs
        .stop(&run.id)
        .ok_or("the run is not running")?;
    Ok(Out::new(
        json!({"run_id": stopped.id, "status": stopped.status}),
        format!("Stopped a run in {}", space.name),
    )
    .spaces(vec![space.name]))
}

pub(super) async fn retry(ctx: &Ctx, a: Value) -> R {
    let a: RunArgs = args(a)?;
    let run = ctx.state.runs.get(&a.run_id).ok_or(NO_RUN)?;
    let sid = run.space_id.clone();
    let space = on_db(ctx, move |db, grant| {
        Ok(match grant.check(&db.lock(), &sid)? {
            Ok(s) => Ok(s),
            Err(Denied::NoSpace) => Err(NO_RUN.to_owned()),
            Err(d) => Err(denied(d)),
        })
    })
    .await?;
    let who = Human {
        id: ctx.user.clone(),
        name: ctx.name.clone(),
    };
    let new = crate::run_retry::retry(&ctx.state, &who, &run.id)
        .await
        .map_err(|e| e.text())?;
    Ok(Out::new(
        json!({"new_run_id": new.id, "status": new.status}),
        format!("Retried a run in {}", space.name),
    )
    .spaces(vec![space.name]))
}
