//! Metro server: private Spaces of shared rooms and threads with durable
//! SQLite history, a private personal Chat per user, a bounded job queue per
//! Space for that Space's optional paired runner, and Rig agents whose tools
//! run on that runner.

pub mod agent;
pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod jobs;
pub mod mentions;
pub mod orchestration;
pub mod tools;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post, put};
use tokio::sync::Semaphore;

pub use auth::HumanAuth;
pub use config::{Config, Limits, ModelConfig};
use db::{Addressed, Author, Db, Event};
use jobs::JobQueue;
use orchestration::RunRegistry;

/// Request bodies from people.
const HUMAN_BODY_LIMIT: usize = 64 * 1024;
/// Finished runs kept for `GET /v1/runs`.
const RUN_HISTORY: usize = 200;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
    pub jobs: JobQueue,
    pub auth: Arc<dyn HumanAuth>,
    /// Agent runs at once, Spaces and personal Chat together.
    pub agent_runs: Arc<Semaphore>,
    pub demo_runs: Arc<Semaphore>,
    /// Space agent runs, active and recent.
    pub runs: Arc<RunRegistry>,
}

impl AppState {
    /// Bind the database to the authenticator's identity namespace (refusing
    /// a database whose user ids came from another provider) and build the
    /// shared state.
    pub async fn new(config: Config, db: Db, auth: Arc<dyn HumanAuth>) -> anyhow::Result<Self> {
        let namespace = auth.namespace().to_owned();
        db.call(move |db| db.bind_identity_namespace(&namespace))
            .await?;
        let limits = &config.limits;
        Ok(Self {
            jobs: JobQueue::new(
                limits.queue_capacity,
                limits.claim_timeout,
                limits.run_timeout,
            ),
            agent_runs: Arc::new(Semaphore::new(limits.max_agent_runs)),
            demo_runs: Arc::new(Semaphore::new(limits.max_demo_runs)),
            runs: Arc::new(RunRegistry::with_limits(limits.max_agent_runs, RUN_HISTORY)),
            config: Arc::new(config),
            db,
            auth,
        })
    }

    /// Append a server-written event to a Space room; failures are logged,
    /// not returned, because background tasks have no caller to report to.
    pub async fn append(
        &self,
        space: &str,
        room: &str,
        kind: &str,
        author: &Author,
        text: &str,
    ) -> Option<Event> {
        let (space, room, kind, author, text) = (
            space.to_owned(),
            room.to_owned(),
            kind.to_owned(),
            author.clone(),
            text.to_owned(),
        );
        match self
            .db
            .call(move |db| {
                db.append_event(&space, &room, &kind, &author, &text, &Addressed::room())
            })
            .await
        {
            Ok(event) => Some(event),
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "appending room event");
                None
            }
        }
    }
}

pub fn router(state: AppState) -> Router {
    let human = Router::new()
        .route("/v1/me", get(api::me))
        .route("/v1/spaces", get(api::list_spaces).post(api::create_space))
        .route(
            "/v1/spaces/{sid}/members",
            get(api::list_members)
                .post(api::add_member)
                .delete(api::remove_member),
        )
        .route(
            "/v1/spaces/{sid}/runner-token",
            post(api::issue_runner_token).delete(api::revoke_runner_token),
        )
        .route(
            "/v1/spaces/{sid}/rooms",
            get(api::list_rooms).post(api::create_room),
        )
        .route("/v1/spaces/{sid}/rooms/{rid}/events", get(api::list_events))
        .route(
            "/v1/spaces/{sid}/rooms/{rid}/messages",
            post(api::post_message),
        )
        .route("/v1/spaces/{sid}/rooms/{rid}/demo", post(api::demo))
        .route(
            "/v1/spaces/{sid}/rooms/{rid}/events/{eid}/reactions",
            put(api::put_space_reaction).delete(api::delete_space_reaction),
        )
        .route("/v1/chat/events", get(api::chat_events))
        .route("/v1/chat/messages", post(api::chat_message))
        .route(
            "/v1/chat/events/{eid}/reactions",
            put(api::put_chat_reaction).delete(api::delete_chat_reaction),
        )
        .route("/v1/runs", get(api::list_runs))
        .route("/v1/runs/{id}/stop", post(api::stop_run))
        .layer(DefaultBodyLimit::max(HUMAN_BODY_LIMIT));
    // Room for the largest result plus JSON escaping.
    let runner_limit = state.config.limits.max_result_bytes * 2 + 1024;
    let runner = Router::new()
        .route("/v1/runner/claim", post(api::runner_claim))
        .route("/v1/runner/jobs/{id}/result", post(api::runner_result))
        .layer(DefaultBodyLimit::max(runner_limit));
    Router::new()
        .route("/healthz", get(api::healthz))
        .merge(human)
        .merge(runner)
        .with_state(state)
}
