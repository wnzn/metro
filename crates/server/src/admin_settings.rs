//! Organization settings. Read through the caller's SQLite transaction when
//! a setting limits a write, so the check and write use the same snapshot.
//!
//! `GET /v1/admin/settings` returns the effective values as a JSON object.
//! `PATCH /v1/admin/settings` takes a JSON object with either or both keys
//! and returns the whole updated object. Omitted keys are unchanged; nulls,
//! unknown keys and non-integers are rejected. Both endpoints require a
//! human bearer token (401 without one) whose stable id is listed in
//! `METRO_ORG_ADMINS` (403 otherwise).
//!
//! - `pages_per_space`: 1–500 (default 50)
//! - `versions_per_page`: 2–1000 (default 200; room for the first and latest)
//!
//! Updating a limit does not itself delete anything.

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use rusqlite::{Connection, params};
use serde::{Deserialize, Deserializer, Serialize};

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody, db};
use crate::auth::Human;

const PAGES: (u32, u32, u32) = (1, 500, 50);
const VERSIONS: (u32, u32, u32) = (2, 1_000, 200);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Settings {
    pub pages_per_space: u32,
    pub versions_per_page: u32,
}

pub(crate) fn init(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS admin_settings (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            pages_per_space INTEGER NOT NULL CHECK (pages_per_space BETWEEN {} AND {}),
            versions_per_page INTEGER NOT NULL CHECK (versions_per_page BETWEEN {} AND {})
        );
        INSERT OR IGNORE INTO admin_settings VALUES (1, {}, {});",
        PAGES.0, PAGES.1, VERSIONS.0, VERSIONS.1, PAGES.2, VERSIONS.2
    ))?;
    Ok(())
}

pub fn read(conn: &Connection) -> rusqlite::Result<Settings> {
    conn.query_row(
        "SELECT pages_per_space, versions_per_page FROM admin_settings WHERE singleton = 1",
        [],
        |r| {
            Ok(Settings {
                pages_per_space: r.get(0)?,
                versions_per_page: r.get(1)?,
            })
        },
    )
}

// Omission leaves the field unchanged. An explicit null is not a number.
fn integer<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    u32::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    #[serde(default, deserialize_with = "integer")]
    pages_per_space: Option<u32>,
    #[serde(default, deserialize_with = "integer")]
    versions_per_page: Option<u32>,
}

fn check(value: Option<u32>, field: &str, (min, max, _): (u32, u32, u32)) -> ApiResult<()> {
    if value.is_some_and(|n| !(min..=max).contains(&n)) {
        return Err(ApiError::bad_request(format!(
            "{field} must be {min}..={max}"
        )));
    }
    Ok(())
}

fn require_admin(state: &AppState, who: &Human) -> ApiResult<()> {
    if !state.config.is_org_admin(&who.id) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "only an organization admin can manage server settings",
        ));
    }
    Ok(())
}

async fn get_settings(State(state): State<AppState>, who: Human) -> ApiResult<Json<Settings>> {
    require_admin(&state, &who)?;
    db(&state, |db| Ok(read(&db.lock())?)).await.map(Json)
}

async fn patch_settings(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<Patch>,
) -> ApiResult<Json<Settings>> {
    require_admin(&state, &who)?;
    let Json(patch) = body?;
    check(patch.pages_per_space, "pages_per_space", PAGES)?;
    check(patch.versions_per_page, "versions_per_page", VERSIONS)?;
    db(&state, move |db| {
        let conn = db.lock();
        conn.execute(
            "UPDATE admin_settings
             SET pages_per_space = coalesce(?1, pages_per_space),
                 versions_per_page = coalesce(?2, versions_per_page)
             WHERE singleton = 1",
            params![patch.pages_per_space, patch.versions_per_page],
        )?;
        Ok(read(&conn)?)
    })
    .await
    .map(Json)
}

pub(crate) fn router() -> Router<AppState> {
    Router::new().route(
        "/v1/admin/settings",
        get(get_settings).patch(patch_settings),
    )
}
