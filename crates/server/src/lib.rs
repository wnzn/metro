//! Metro server: private Spaces of shared rooms and threads with durable
//! SQLite history, a private personal Chat per user, a bounded job queue per
//! Space for the runner serving it (a dedicated Space token, or a registered
//! personal or organization runner attached to it), and Rig agents whose
//! tools run on that runner.

pub mod admin_users;
pub mod agent;
pub mod agent_dispatch;
pub mod api;
pub mod attachments;
pub mod auth;
pub mod config;
pub mod db;
pub mod device_relay;
pub mod jobs;
pub mod library;
pub mod library_search;
pub mod mentions;
pub mod model_routing;
pub mod monitoring;
pub mod orchestration;
pub mod pages;
pub mod profiles;
pub mod providers;
pub mod quick_search;
pub mod runner_pairing;
pub mod runners;
pub mod schedules;
pub mod search;
pub mod space_agents;
pub mod space_prefs;
pub mod spaces;
pub mod tools;
pub mod web_devices;
pub mod web_tools;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
use tokio::sync::Semaphore;

pub use auth::HumanAuth;
pub use config::{Config, Limits, ModelConfig};
use db::{Activity, Addressed, Author, Db, Event};
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
    /// Personal model connections.
    pub providers: crate::providers::ProviderService,
    /// Relay to people's own `metro-connector` devices for device connections.
    pub device: crate::device_relay::DeviceRelay,
    /// Personal model routing and System One decisions.
    pub routing: crate::model_routing::Routing,
    /// Documents and media library limits and upload slots.
    pub library: crate::library::Library,
    /// Managed local accounts (dev namespace only).
    pub managed: crate::admin_users::Managed,
    /// Avatar re-encodes running at once (see [`crate::profiles`]).
    pub avatar_encodes: Arc<Semaphore>,
    /// Page builder model turns at once, apart from agent runs.
    pub page_builds: Arc<Semaphore>,
}

impl AppState {
    /// Bind the database to the authenticator's identity namespace (refusing
    /// a database whose user ids came from another provider) and build the
    /// shared state.
    pub async fn new(config: Config, db: Db, auth: Arc<dyn HumanAuth>) -> anyhow::Result<Self> {
        let namespace = auth.namespace().to_owned();
        let managed = crate::admin_users::Managed::from_env(&namespace);
        if managed.enabled()
            && let Some(bad) = config
                .org_admins
                .iter()
                .find(|a| crate::admin_users::is_managed_id(a))
        {
            anyhow::bail!(
                "METRO_ORG_ADMINS lists the managed user id {bad}; managed users can't be \
                 organization admins"
            );
        }
        db.call(move |db| db.bind_identity_namespace(&namespace))
            .await?;
        let providers = crate::providers::ProviderService::from_env(&config.db_path, db.clone())?;
        let limits = &config.limits;
        let library = crate::library::Library::from_env()?;
        let retention = crate::monitoring::Retention::of(&config.limits);
        db.call(move |db| crate::monitoring::prune(db, retention))
            .await?;
        let state = Self {
            avatar_encodes: Arc::new(Semaphore::new(crate::profiles::MAX_AVATAR_ENCODES)),
            page_builds: Arc::new(Semaphore::new(crate::pages::generate::MAX_PAGE_BUILDS)),
            managed,
            library,
            providers,
            routing: crate::model_routing::Routing::default(),
            device: crate::device_relay::DeviceRelay::new(
                crate::device_relay::DeviceLimits::default(),
                Arc::new(crate::providers::DeviceDirectory),
            ),
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
        };
        state
            .monitor(
                crate::monitoring::Scope::Instance,
                crate::monitoring::Code::ServerStarted,
            )
            .await;
        Ok(state)
    }

    /// Replace the env user names that managed display names may not take
    /// (read from `METRO_DEV_USERS` by [`AppState::new`]). For tests.
    pub fn with_env_user_ids(mut self, ids: Vec<String>) -> Self {
        self.managed = crate::admin_users::Managed::new(self.auth.namespace(), ids);
        self
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
        self.append_with(space, room, kind, author, text, None)
            .await
    }

    /// [`AppState::append`] tagged with the agent turn or Demo run that wrote it.
    pub async fn append_with(
        &self,
        space: &str,
        room: &str,
        kind: &str,
        author: &Author,
        text: &str,
        activity: Option<&Activity>,
    ) -> Option<Event> {
        let (space, room, kind, author, text, activity) = (
            space.to_owned(),
            room.to_owned(),
            kind.to_owned(),
            author.clone(),
            text.to_owned(),
            activity.cloned(),
        );
        match self
            .db
            .call(move |db| {
                db.append_event_with(
                    &space,
                    &room,
                    &kind,
                    &author,
                    &text,
                    &Addressed::room(),
                    activity.as_ref(),
                )
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
        .route(
            "/v1/runners",
            get(runners::list_runners).post(runners::create_runner),
        )
        .route("/v1/runners/{id}", delete(runners::delete_runner))
        .route("/v1/runners/{id}/token", post(runners::rotate_runner_token))
        .route(
            "/v1/spaces/{sid}/runner",
            get(runners::get_space_runner)
                .put(runners::put_space_runner)
                .delete(runners::delete_space_runner),
        )
        // Provider routes carry their full paths and authenticate people
        // themselves; they share the human body limit.
        .merge(crate::providers::router())
        // Personal device connector routes (`/v1/device/*` and
        // `POST /v1/providers/{id}/device-token`), with full paths.
        .merge(crate::device_relay::router())
        // A person's own web search computers and their personal Chat opt-in.
        .merge(crate::web_devices::router())
        // `GET|PUT /v1/model-routing`, personal routing settings.
        .merge(crate::model_routing::router())
        // Personal semantic search and scheduled prompts, with full paths.
        .merge(crate::search::router())
        .merge(crate::quick_search::router())
        .merge(crate::schedules::router())
        // Documents and media library; its upload route sets its own limit.
        .merge(crate::library::router())
        // Documents and media search, with full paths.
        .merge(crate::library_search::router())
        // Managed user accounts, for org admins (dev namespace only).
        .merge(crate::admin_users::api_router())
        // Personal profiles and avatars; the avatar upload sets its own limit.
        .merge(crate::profiles::router())
        // `GET /v1/monitoring/events`: Space owners and org admins.
        .merge(crate::monitoring::router())
        // Space edit/delete/leave, Space agents and per-Space preferences.
        .merge(crate::spaces::router())
        .merge(crate::space_agents::router())
        .merge(crate::space_prefs::router())
        // Pages; routes that carry page source set their own limit.
        .merge(crate::pages::router())
        .layer(DefaultBodyLimit::max(HUMAN_BODY_LIMIT));
    // Room for the largest result plus JSON escaping.
    let runner_limit = state.config.limits.max_result_bytes * 2 + 1024;
    let runner = Router::new()
        .route("/v1/runner/claim", post(api::runner_claim))
        .route("/v1/runner/jobs/{id}/result", post(api::runner_result))
        .layer(DefaultBodyLimit::max(runner_limit));
    Router::new()
        .route("/healthz", get(api::healthz))
        // Static admin shell at `/admin`; `/` redirects there.
        .merge(crate::admin_users::web_router())
        .merge(human)
        .merge(runner)
        .with_state(state)
}
