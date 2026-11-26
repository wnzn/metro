//! Personal model routing: which of a person's usable chat connections
//! answers a turn. Use-case routing is a personal opt-in, persisted per
//! user (`GET|PUT /v1/model-routing`), and off by default.
//!
//! A send in personal Chat is direct. Space addressing is handled separately
//! by [`crate::agent_dispatch`], which uses this module for agent Choice
//! questions. Space addressing never applies to personal Chat.
//!
//! **Use-case routing** (`enabled`) runs on any turn that starts agent work
//! for the person, unless they picked a model explicitly for that turn. It
//! asks System One one Choice over the person's own case keys plus the
//! reserved [`NO_MATCH_KEY`] option. A case is used only if it is chosen
//! with a probability of at least `min_route_probability`, and its key maps
//! to a connection id from the person's own saved settings; nothing the
//! model returns is ever used as an id or endpoint. A valid `no_match` or
//! a weak match uses the person's default. A failed decision (the System
//! One connection errs, times out, answers malformed, or can no longer be
//! used) is not a no-match: the turn fails visibly before any model is
//! called. A chosen connection that cannot be used fails the same way;
//! nothing falls back to another model.
//!
//! Every System One request carries exactly one question, because some
//! endpoints accept no more. A personal Chat send asks at most one Choice,
//! for use-case routing. A Space can ask an agent Choice before model routing.
//!
//! The route that was actually used is recorded on the turn's `started`
//! activity marker ([`RouteNote`]): where the model came from, the case, the
//! connection and model ids, and a short reason. No prompts, no credentials.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use metro_model_protocol::{self as protocol, Answers, Capability, Question, Questions};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::AppState;
use crate::api::{ApiError, ApiResult, JsonBody};
use crate::auth::Human;
use crate::db::{Addressed, Author, kind};
use crate::providers::{SelectedModel, UseError};

/// Most use cases one person may define.
pub const MAX_CASES: usize = 16;
/// Longest case key (`[a-z][a-z0-9_]*`).
pub const MAX_CASE_KEY: usize = 32;
/// Longest case label, in characters.
pub const MAX_CASE_LABEL: usize = 64;
/// Longest case description (the criterion System One judges), in characters.
pub const MAX_CASE_DESCRIPTION: usize = 500;
pub const DEFAULT_MIN_DIRECTEDNESS: f64 = 0.35;
pub const DEFAULT_MIN_ROUTE_PROBABILITY: f64 = 0.65;
/// Longest message text sent to System One, in characters.
pub const MAX_CLASSIFIED_CHARS: usize = 4_000;
/// Wall clock for one System One call.
const DECISION_TIMEOUT: Duration = Duration::from_secs(10);
/// System One calls in flight at once, across the server.
const MAX_DECISIONS: usize = 8;

const Q_USE_CASE: &str = "use_case";
const Q_AGENT: &str = "agent";
/// The agent Choice's reserved "nobody" option. Agent options are keyed
/// `agent_1`, `agent_2`, ... so no agent can take it.
pub const NO_AGENT_KEY: &str = "no_agent";

// ---- Settings ----

/// One person's routing settings, as stored and as `GET|PUT /v1/model-routing`
/// carries them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingSettings {
    /// Use-case routing on turns that start agent work for this person.
    #[serde(default)]
    pub enabled: bool,
    /// Legacy field kept for wire/storage compatibility and the one-time
    /// Space preferences migration. Not used for dispatch or validation.
    #[serde(default)]
    pub infer_addressing: bool,
    /// The person's System One connection, used for both decisions.
    #[serde(default)]
    pub classifier_id: Option<String>,
    /// The person's embeddings connection, for their semantic search.
    #[serde(default)]
    pub embedding_id: Option<String>,
    /// The chat connection used when routing finds no case. `None` keeps
    /// the person's default chat connection.
    #[serde(default)]
    pub default_chat_id: Option<String>,
    /// Confidence floor for unaddressed messages in opted-in Spaces.
    #[serde(default = "default_min_directedness")]
    pub min_directedness: f64,
    #[serde(default = "default_min_route_probability")]
    pub min_route_probability: f64,
    #[serde(default)]
    pub cases: Vec<RouteCase>,
}

fn default_min_directedness() -> f64 {
    DEFAULT_MIN_DIRECTEDNESS
}

fn default_min_route_probability() -> f64 {
    DEFAULT_MIN_ROUTE_PROBABILITY
}

impl Default for RoutingSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            infer_addressing: false,
            classifier_id: None,
            embedding_id: None,
            default_chat_id: None,
            min_directedness: DEFAULT_MIN_DIRECTEDNESS,
            min_route_probability: DEFAULT_MIN_ROUTE_PROBABILITY,
            cases: Vec::new(),
        }
    }
}

/// One use case: System One picks it by `key` from `label` and
/// `description`; `connection_id` is what then answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteCase {
    pub key: String,
    pub label: String,
    pub description: String,
    pub connection_id: String,
}

impl RoutingSettings {
    /// Whether use-case routing calls System One for a turn.
    pub(crate) fn wants_case_routing(&self) -> bool {
        self.enabled && !self.cases.is_empty() && self.classifier_id.is_some()
    }

    /// Shape checks that need no database.
    fn check_shape(&self) -> Result<(), String> {
        let unit = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
        if !unit(self.min_directedness) || self.min_directedness < 0.2 {
            return Err("min_directedness must be a number from 0.2 to 1".to_owned());
        }
        if !unit(self.min_route_probability) {
            return Err("min_route_probability must be a number from 0 to 1".to_owned());
        }
        if self.cases.len() > MAX_CASES {
            return Err(format!("at most {MAX_CASES} use cases"));
        }
        let mut keys: Vec<&str> = Vec::new();
        for case in &self.cases {
            if !protocol::valid_key(&case.key, MAX_CASE_KEY) {
                return Err(format!(
                    "a use case key must be 1..={MAX_CASE_KEY} of a-z, 0-9 and _, starting with a letter"
                ));
            }
            if case.key == NO_MATCH_KEY {
                return Err(format!(
                    "\"{NO_MATCH_KEY}\" is reserved and can't be a use case key"
                ));
            }
            if keys.contains(&case.key.as_str()) {
                return Err("use case keys must be unique".to_owned());
            }
            keys.push(&case.key);
            let label = case.label.trim();
            if label.is_empty()
                || label.chars().count() > MAX_CASE_LABEL
                || label.chars().any(char::is_control)
            {
                return Err(format!(
                    "a use case label must be 1..={MAX_CASE_LABEL} printable characters"
                ));
            }
            let description = case.description.trim();
            if description.is_empty()
                || description.chars().count() > MAX_CASE_DESCRIPTION
                || description.chars().any(|c| c.is_control() && c != '\n')
            {
                return Err(format!(
                    "a use case description must be 1..={MAX_CASE_DESCRIPTION} characters"
                ));
            }
        }
        if self.enabled && !self.cases.is_empty() && self.classifier_id.is_none() {
            return Err(
                "classifier_id must name a System One connection for use-case routing".to_owned(),
            );
        }
        Ok(())
    }

    fn trimmed(mut self) -> Self {
        for case in &mut self.cases {
            case.label = case.label.trim().to_owned();
            case.description = case.description.trim().to_owned();
        }
        self
    }
}

/// Create the settings table and migrate the old default once. Called from `Db::init`.
pub fn init(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS model_routing_settings (
             user_id    TEXT PRIMARY KEY NOT NULL,
             settings   TEXT NOT NULL,
             updated_at INTEGER NOT NULL
         );",
    )?;
    let tx = conn.unchecked_transaction()?;
    if tx.execute(
        "INSERT INTO metro_meta (key, value) VALUES ('routing_directedness_v2', '1')
         ON CONFLICT (key) DO NOTHING",
        [],
    )? == 1
    {
        tx.execute(
            "UPDATE model_routing_settings
             SET settings = json_set(settings, '$.min_directedness', ?1)
             WHERE CASE WHEN json_valid(settings)
                        THEN json_extract(settings, '$.min_directedness') = 0.85 END",
            [DEFAULT_MIN_DIRECTEDNESS],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn load(conn: &Connection, user_id: &str) -> anyhow::Result<RoutingSettings> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT settings FROM model_routing_settings WHERE user_id = ?1",
            [user_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match raw {
        // Unreadable settings are treated as off, never guessed.
        Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "stored routing settings are unreadable; treating as off");
            RoutingSettings::default()
        }),
        None => RoutingSettings::default(),
    })
}

fn store(conn: &Connection, user_id: &str, settings: &RoutingSettings) -> anyhow::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO model_routing_settings (user_id, settings, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (user_id) DO UPDATE SET settings = excluded.settings, updated_at = excluded.updated_at",
        params![user_id, serde_json::to_string(settings)?, now],
    )?;
    Ok(())
}

/// `user`'s routing settings (defaults when they never saved any).
pub async fn preferences(state: &AppState, user: &str) -> anyhow::Result<RoutingSettings> {
    let user = user.to_owned();
    state.db.call(move |db| load(&db.lock(), &user)).await
}

/// `GET|PUT /v1/model-routing`.
pub fn router() -> Router<AppState> {
    Router::new().route("/v1/model-routing", get(get_settings).put(put_settings))
}

async fn get_settings(
    State(state): State<AppState>,
    who: Human,
) -> ApiResult<Json<RoutingSettings>> {
    preferences(&state, &who.id)
        .await
        .map(Json)
        .map_err(ApiError::internal)
}

async fn put_settings(
    State(state): State<AppState>,
    who: Human,
    body: JsonBody<RoutingSettings>,
) -> ApiResult<Json<RoutingSettings>> {
    let Json(settings) = body?;
    let settings = settings.trimmed();
    settings.check_shape().map_err(ApiError::bad_request)?;
    // Every reference must be a connection the person can use somewhere,
    // set up for the right capability. Use is rechecked on every turn.
    let refs = settings
        .classifier_id
        .iter()
        .map(|id| ("classifier_id", id, Capability::SystemOne))
        .chain(
            settings
                .embedding_id
                .iter()
                .map(|id| ("embedding_id", id, Capability::Embeddings)),
        )
        .chain(
            settings
                .default_chat_id
                .iter()
                .map(|id| ("default_chat_id", id, Capability::Chat)),
        )
        .chain(
            settings
                .cases
                .iter()
                .map(|c| ("cases[].connection_id", &c.connection_id, Capability::Chat)),
        );
    for (field, id, wanted) in refs {
        let visible = if crate::providers::api::valid_connection_id(id) {
            state
                .routing
                .backend
                .visible_capability(&state, &who.id, id)
                .await
                .map_err(ApiError::internal)?
        } else {
            None
        };
        // A missing, foreign and wrong-capability connection look the same.
        if visible != Some(wanted) {
            return Err(ApiError::bad_request(format!(
                "{field} must be a {wanted} connection of yours or one shared with you"
            )));
        }
    }
    let (uid, saved) = (who.id.clone(), settings.clone());
    state
        .db
        .call(move |db| store(&db.lock(), &uid, &saved))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(settings))
}

// ---- Provider backend ----

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The model calls routing makes, behind one seam so the server's
/// provider lane can change and tests can substitute a fake.
pub trait RoutingBackend: Send + Sync + 'static {
    /// One System One call with connection `id`, as `user` would use it in
    /// `space` (`None`: personal Chat). Returns the raw response body; the
    /// caller validates it.
    fn system_one<'a>(
        &'a self,
        state: &'a AppState,
        user: &'a str,
        space: Option<&'a str>,
        id: &'a str,
        state_payload: Value,
        questions: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>>;

    /// The capability of `id` if `user` can use it somewhere (their own, or
    /// shared with the organization or with a Space they are a member of).
    fn visible_capability<'a>(
        &'a self,
        state: &'a AppState,
        user: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Capability>>>;

    /// Whether `user` may use System One connection `id` in Space `space`
    /// now. By default: visible somewhere with the System One capability.
    fn system_one_usable<'a>(
        &'a self,
        state: &'a AppState,
        user: &'a str,
        space: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        let _ = space;
        Box::pin(async move {
            Ok(self.visible_capability(state, user, id).await? == Some(Capability::SystemOne))
        })
    }

    /// The chat model of `id` for a turn `user` starts in `space`
    /// (`None`: personal Chat), if they may use it there.
    fn chat_model<'a>(
        &'a self,
        state: &'a AppState,
        user: &'a str,
        space: Option<&'a str>,
        id: &'a str,
    ) -> BoxFuture<'a, Result<SelectedModel, UseError>>;
}

/// The real backend: [`crate::providers`].
pub struct ProviderBackend;

impl RoutingBackend for ProviderBackend {
    fn system_one<'a>(
        &'a self,
        state: &'a AppState,
        user: &'a str,
        space: Option<&'a str>,
        id: &'a str,
        state_payload: Value,
        questions: Value,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async move {
            crate::providers::decide(state, user, space, id, state_payload, questions)
                .await
                .map_err(anyhow::Error::new)
        })
    }

    fn visible_capability<'a>(
        &'a self,
        state: &'a AppState,
        user: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<Capability>>> {
        let (user, id) = (user.to_owned(), id.to_owned());
        Box::pin(async move {
            state
                .db
                .call(move |db| {
                    use crate::providers::{sharing, store};
                    let conn = db.lock();
                    let Some(row) = store::find(&conn, &id)? else {
                        return Ok(None);
                    };
                    // The providers' own access rule, in personal Chat and
                    // in each Space the connection is shared with.
                    let mut visible = sharing::visible(&conn, &user, None, &row)?.is_some();
                    if !visible {
                        for sid in store::share_of(&conn, &row.id)?.space_ids {
                            if sharing::visible(&conn, &user, Some(&sid), &row)?.is_some() {
                                visible = true;
                                break;
                            }
                        }
                    }
                    Ok(visible
                        .then(|| serde_json::from_value(json!(row.capability.as_str())).ok())
                        .flatten())
                })
                .await
        })
    }

    fn system_one_usable<'a>(
        &'a self,
        state: &'a AppState,
        user: &'a str,
        space: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        let (user, space, id) = (user.to_owned(), space.to_owned(), id.to_owned());
        Box::pin(async move {
            state
                .db
                .call(move |db| {
                    use crate::providers::{sharing, store};
                    let conn = db.lock();
                    let Some(row) = store::find(&conn, &id)? else {
                        return Ok(false);
                    };
                    Ok(row.capability.as_str() == Capability::SystemOne.as_str()
                        && sharing::visible(&conn, &user, Some(&space), &row)?.is_some())
                })
                .await
        })
    }

    fn chat_model<'a>(
        &'a self,
        state: &'a AppState,
        user: &'a str,
        space: Option<&'a str>,
        id: &'a str,
    ) -> BoxFuture<'a, Result<SelectedModel, UseError>> {
        Box::pin(crate::providers::connection_model_for(
            state, user, space, id,
        ))
    }
}

/// Routing state shared by the server.
#[derive(Clone)]
pub struct Routing {
    pub backend: Arc<dyn RoutingBackend>,
    decisions: Arc<Semaphore>,
}

impl Routing {
    pub fn new(backend: Arc<dyn RoutingBackend>) -> Self {
        Self {
            backend,
            decisions: Arc::new(Semaphore::new(MAX_DECISIONS)),
        }
    }
}

impl Default for Routing {
    fn default() -> Self {
        Self::new(Arc::new(ProviderBackend))
    }
}

// ---- Decisions ----

/// Who a message is addressed to, from its explicit recipients alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addressing {
    /// `@bob` only: never the agent, never System One.
    PeopleOnly,
    /// `@agent`, Ask agent, or both people and the agent.
    Agent,
    /// No recipients at all.
    Unaddressed,
}

impl Addressing {
    pub fn of(mentions: &[crate::mentions::Mention]) -> Self {
        if crate::mentions::invokes_agent(mentions) {
            Addressing::Agent
        } else if mentions.is_empty() {
            Addressing::Unaddressed
        } else {
            Addressing::PeopleOnly
        }
    }
}

/// How a turn came to be addressed to the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressedBy {
    /// `@agent` or Ask agent.
    Explicit,
    /// The sender's opt-in System One judgement.
    Classifier,
    /// The creator's scheduled prompt.
    Schedule,
    /// A parent agent's delegation.
    Thread,
}

/// Where the model of a turn came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteSource {
    /// A model the person chose for this turn.
    Override,
    /// A use case System One picked.
    Case,
    /// The routing settings' `default_chat_id`.
    RoutingDefault,
    /// The person's default chat connection.
    Default,
    /// The operator's `METRO_MODEL` (the person has no default).
    Environment,
    /// The parent turn's model, for a thread.
    Inherited,
    /// The person's default model for this agent in this Space
    /// ([`crate::space_prefs`]).
    AgentDefault,
    /// The person's default model in this Space ([`crate::space_prefs`]).
    SpaceDefault,
}

/// The route a turn actually took, as shown on its `started` marker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteNote {
    pub source: RouteSource,
    pub addressed_by: AddressedBy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// A short, human-readable reason.
    pub reason: String,
}

/// Which model a turn will use, decided before it starts.
#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    /// `None`: the person's default chat connection, or the environment
    /// model when they have none (the behaviour without routing).
    pub connection_id: Option<String>,
    /// Validated explicit selection, not derived from the route note.
    pub(crate) turn_selection: Option<crate::providers::turn::TurnSelection>,
    pub note: RouteNote,
    /// Set when the person's routing could not decide (their System One
    /// connection failed, answered malformed, or can no longer be used):
    /// the turn fails with this text before any model is called. Never a
    /// silent switch to another model.
    pub blocked: Option<String>,
    /// Why the turn can't run, in full, for the sender's own POST response
    /// only: in a Space room, `blocked` reads the same for a lost grant as
    /// for an upstream failure.
    pub sender_note: Option<AddressingNote>,
}

impl Route {
    /// The behaviour without routing: the person's default.
    pub fn default_for(addressed_by: AddressedBy) -> Self {
        Self {
            connection_id: None,
            turn_selection: None,
            blocked: None,
            sender_note: None,
            note: RouteNote {
                source: RouteSource::Default,
                addressed_by,
                case_key: None,
                case_label: None,
                connection_id: None,
                provider_id: None,
                model_id: None,
                reasoning_effort: None,
                reason: "your default chat connection".to_owned(),
            },
        }
    }

    /// A model the person chose for this one turn.
    pub fn chosen(addressed_by: AddressedBy, connection_id: &str) -> Self {
        Self {
            connection_id: Some(connection_id.to_owned()),
            turn_selection: None,
            blocked: None,
            sender_note: None,
            note: RouteNote {
                source: RouteSource::Override,
                addressed_by,
                case_key: None,
                case_label: None,
                connection_id: Some(connection_id.to_owned()),
                provider_id: None,
                model_id: None,
                reasoning_effort: None,
                reason: "the model you chose for this message".to_owned(),
            },
        }
    }

    /// The route of a thread's turn: the same connection as its parent turn.
    pub fn inherited(parent: &Route) -> Self {
        let mut note = parent.note.clone();
        note.addressed_by = AddressedBy::Thread;
        if parent.connection_id.is_some() {
            note.source = RouteSource::Inherited;
            note.reason = "the parent turn's model".to_owned();
        }
        Self {
            connection_id: parent.connection_id.clone(),
            turn_selection: parent.turn_selection.clone(),
            blocked: None,
            sender_note: None,
            note,
        }
    }

    /// Why a turn on this route could not get its model, as it is shown:
    /// `space` is the Space whose members read it (`None`: the person's own
    /// Chat, which shows the full reason). In a Space room a lost share or
    /// connection reads exactly like any other failure, and the full reason
    /// goes to the sender alone, as the note.
    pub fn refused(&self, space: Option<&str>, error: &UseError) -> Refusal {
        let message = self.unavailable(error);
        match space {
            None => Refusal::shown(message),
            Some(_) => Refusal {
                shown: MODEL_UNAVAILABLE_IN_ROOM.to_owned(),
                note: Some(AddressingNote::ModelUnavailable {
                    source: self.note.source,
                    reason: use_reason(error),
                    message,
                }),
            },
        }
    }

    /// Why a turn on this route could not get its model, in full.
    pub fn unavailable(&self, error: &UseError) -> String {
        match (&self.note.source, error) {
            (RouteSource::Override, UseError::NotFound) => {
                "Ask agent is unavailable: you have no model connection with that id.".to_owned()
            }
            (RouteSource::Override, e) => format!("Ask agent is unavailable: {e}."),
            (RouteSource::Case, e) => format!(
                "Routing picked the use case \"{}\" for this message, but its model connection \
                 can't be used here: {e}. No other model was used.",
                self.note.case_label.as_deref().unwrap_or("?")
            ),
            (RouteSource::RoutingDefault, e) => format!(
                "Your routing default model connection can't be used here: {e}. No other model \
                 was used."
            ),
            (RouteSource::AgentDefault, e) => format!(
                "Your default model for this agent in this Space can't be used here: {e}. No \
                 other model was used."
            ),
            (RouteSource::SpaceDefault, e) => format!(
                "Your default model for this Space can't be used here: {e}. No other model was \
                 used."
            ),
            (RouteSource::Inherited, e) => format!(
                "The model connection of the parent turn can't be used here: {e}. No other \
                 model was used."
            ),
            (_, e) => format!("Ask agent is unavailable: {e}."),
        }
    }
}

/// What other members of a Space room read when a turn couldn't get the
/// sender's model, whatever the reason: a revoked share, a deleted or
/// misconfigured connection, a busy or offline device.
pub const MODEL_UNAVAILABLE_IN_ROOM: &str =
    "This run couldn't use the sender's model, so no agent answered. No other model was used.";

/// A turn that could not start: `shown` is posted where the turn was
/// asked for; `note` is the full reason for the sender's POST response.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    pub shown: String,
    pub note: Option<AddressingNote>,
}

impl Refusal {
    pub fn shown(text: impl Into<String>) -> Self {
        Self {
            shown: text.into(),
            note: None,
        }
    }

    /// The text for the person who asked: the note's full message if any.
    pub fn private(&self) -> &str {
        match &self.note {
            Some(
                AddressingNote::RoutingFailed { message, .. }
                | AddressingNote::ModelUnavailable { message, .. },
            ) => message,
            _ => &self.shown,
        }
    }
}

impl From<String> for Refusal {
    fn from(text: String) -> Self {
        Self::shown(text)
    }
}

/// A coarse, stable code for why a connection can't be used.
pub(crate) fn use_reason(error: &UseError) -> &'static str {
    match error {
        UseError::NotFound
        | UseError::Revoked
        | UseError::PresetNeedsOwnConnection(_)
        | UseError::WrongCapability { .. }
        | UseError::TurnModelNotAllowed
        | UseError::TurnEffortNotAllowed => "not_allowed",
        // The connection behind an explicit model changed: not a lost grant.
        UseError::TurnChanged => "connection_changed",
        UseError::Busy => "busy",
        UseError::DeviceOffline => "offline",
        UseError::NoModel
        | UseError::NoMasterKey
        | UseError::KeyUnreadable
        | UseError::KeyMissing
        | UseError::EndpointNotApproved
        | UseError::Unsupported
        | UseError::MediaUnsupported => "misconfigured",
        UseError::Internal => "internal",
    }
}

/// The Choice's reserved "none of these" option. Case keys may not use it
/// ([`RoutingSettings::check_shape`]), so it can never select a connection.
pub const NO_MATCH_KEY: &str = "no_match";

/// One System One answer, or why there is none.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision<T> {
    Answered(T),
    Failed(DecisionFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionFailure {
    Unreachable,
    Timeout,
    InvalidAnswer,
    NoClassifier,
    NotAllowed,
    Busy,
    InvalidQuestion,
}

impl std::fmt::Display for DecisionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unreachable => "the System One call failed",
            Self::Timeout => "System One took too long",
            Self::InvalidAnswer => "System One's answer was malformed",
            Self::NoClassifier => "no System One connection is set",
            Self::NotAllowed => "the System One connection is not usable here",
            Self::Busy => "System One is busy",
            Self::InvalidQuestion => "the routing question is invalid",
        })
    }
}

impl DecisionFailure {
    /// The failure as other members of a Space room may see it: one public
    /// category for every failure of this stage. A lost grant, a missing
    /// connection, a full queue, a timeout and a malformed answer all read
    /// alike, so the room never learns about the sender's access; the
    /// sender's own note carries the distinction.
    pub fn shared(self) -> Self {
        Self::Unreachable
    }

    fn upstream(error: &anyhow::Error) -> Self {
        use crate::device_relay::DeviceError;
        use crate::providers::ProviderError;
        match error.downcast_ref::<ProviderError>() {
            Some(ProviderError::Use(UseError::Busy) | ProviderError::Busy) => Self::Busy,
            // A device classifier: a lost share and a malformed answer mean
            // what they mean for a server connection.
            Some(ProviderError::Device(DeviceError::AccessRevoked | DeviceError::NotFound)) => {
                Self::NotAllowed
            }
            Some(ProviderError::Device(DeviceError::BadResponse(_))) => Self::InvalidAnswer,
            Some(ProviderError::Use(UseError::DeviceOffline | UseError::Internal)) => {
                Self::Unreachable
            }
            Some(ProviderError::Use(_)) => Self::NotAllowed,
            Some(error) if error.status() == axum::http::StatusCode::GATEWAY_TIMEOUT => {
                Self::Timeout
            }
            // Only locally generated protocol errors reach this branch, never an
            // upstream error body. The shared client currently returns fixed text.
            Some(ProviderError::Endpoint(text))
                if text
                    .starts_with("the model endpoint returned an invalid System One response")
                    || text == "the model endpoint's response is not JSON"
                    || text == "the model endpoint's response is too large" =>
            {
                Self::InvalidAnswer
            }
            _ => Self::Unreachable,
        }
    }
}

/// Extra metadata on the sender's POST response only; never on an Event
/// and never stored.
///
/// `addressing_note` carries `classification_failed` (an unaddressed
/// message's classification failed; nothing ran) or
/// `generic_agent_unavailable` (no agent could be chosen for `@agent`;
/// agents the message named by handle still ran). `routing_note` carries
/// the other two, for a turn the message started in a Space room whose
/// room error is generic: `routing_failed` (use-case routing could not
/// decide) and `model_unavailable` (the turn's model connection can't be
/// used). All but `classification_failed` have a stable `reason` code and
/// the full, actionable `message`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AddressingNote {
    ClassificationFailed {
        reason: &'static str,
    },
    /// `reason` as for `classification_failed`.
    GenericAgentUnavailable {
        reason: &'static str,
        message: String,
    },
    /// `reason` as for `classification_failed`.
    RoutingFailed {
        reason: &'static str,
        message: String,
    },
    /// `reason`: `not_allowed`, `busy`, `offline`, `misconfigured` or
    /// `internal`; `source` says which of the sender's choices it was.
    ModelUnavailable {
        source: RouteSource,
        reason: &'static str,
        message: String,
    },
}

impl AddressingNote {
    /// `@agent` could not be resolved. `named` when the message also named
    /// agents by handle: they were asked on their own (whether their runs
    /// start is reported separately), so the note claims nothing about them.
    pub fn generic_agent_unavailable(why: DecisionFailure, named: bool) -> Self {
        let others = if named {
            " This affects only @agent; the agents you mentioned by handle were asked on their \
             own."
        } else {
            ""
        };
        Self::GenericAgentUnavailable {
            reason: why.reason(),
            message: format!(
                "Your classifier could not choose an agent for @agent: {why}. No agent ran for \
                 @agent; mention an agent by its handle, or check your System One connection \
                 in Settings.{others}"
            ),
        }
    }
}

impl From<DecisionFailure> for AddressingNote {
    fn from(reason: DecisionFailure) -> Self {
        Self::ClassificationFailed {
            reason: reason.reason(),
        }
    }
}

impl DecisionFailure {
    /// The stable reason code of a sender-only note.
    fn reason(self) -> &'static str {
        match self {
            DecisionFailure::Unreachable
            | DecisionFailure::Busy
            | DecisionFailure::InvalidQuestion => "unreachable",
            DecisionFailure::Timeout => "timeout",
            DecisionFailure::InvalidAnswer => "invalid_answer",
            DecisionFailure::NoClassifier => "no_classifier",
            DecisionFailure::NotAllowed => "not_allowed",
        }
    }
}

/// The use-case Choice's answer: the option and its probability.
#[derive(Clone, Debug, PartialEq)]
pub struct CaseAnswer {
    /// One of the person's case keys, or [`NO_MATCH_KEY`].
    pub key: String,
    pub probability: f64,
}

fn case_question(settings: &RoutingSettings) -> Question {
    let mut criteria: BTreeMap<String, Value> = settings
        .cases
        .iter()
        .map(|c| {
            (
                c.key.clone(),
                json!({"label": c.label, "description": c.description}),
            )
        })
        .collect();
    criteria.insert(
        NO_MATCH_KEY.to_owned(),
        json!({"label": "None of these", "description": "The message is not clearly any of the other use cases"}),
    );
    Question::Choice {
        instructions: json!("Which use case does `message` belong to?"),
        criteria,
    }
}

fn message_payload(text: &str, in_space: bool) -> Value {
    let message: String = text.chars().take(MAX_CLASSIFIED_CHARS).collect();
    json!({
        "message": message,
        "where": if in_space { "a shared room" } else { "the sender's private chat with the assistant" },
    })
}

/// Some System One endpoints accept only one typed question per request.
async fn ask_one(
    state: &AppState,
    user: &str,
    space: Option<&str>,
    settings: &RoutingSettings,
    payload: Value,
    id: &str,
    question: Question,
) -> Decision<protocol::Answer> {
    let Some(classifier) = settings.classifier_id.as_deref() else {
        return Decision::Failed(DecisionFailure::NoClassifier);
    };
    let mut asked = Questions::new();
    asked.insert(id.to_owned(), question);
    if protocol::validate_questions(&asked).is_err() {
        return Decision::Failed(DecisionFailure::InvalidQuestion);
    }
    let Ok(_slot) = state.routing.decisions.clone().try_acquire_owned() else {
        return Decision::Failed(DecisionFailure::Busy);
    };
    let call = state.routing.backend.system_one(
        state,
        user,
        space,
        classifier,
        payload,
        serde_json::to_value(&asked).unwrap_or(Value::Null),
    );
    let response = match tokio::time::timeout(DECISION_TIMEOUT, call).await {
        Ok(Ok(response)) => response,
        Ok(Err(e)) => {
            let failure = DecisionFailure::upstream(&e);
            tracing::warn!(%failure, "System One call failed");
            return Decision::Failed(failure);
        }
        Err(_) => return Decision::Failed(DecisionFailure::Timeout),
    };
    let answers: Answers = match protocol::parse_answers(&asked, &response) {
        Ok(a) => a,
        Err(_) => {
            tracing::warn!("System One answer rejected");
            return Decision::Failed(DecisionFailure::InvalidAnswer);
        }
    };
    match answers.into_values().next() {
        Some(answer) => Decision::Answered(answer),
        None => Decision::Failed(DecisionFailure::InvalidAnswer),
    }
}

/// The use-case Choice alone, over the person's cases plus [`NO_MATCH_KEY`].
pub async fn use_case(
    state: &AppState,
    user: &str,
    space: Option<&str>,
    settings: &RoutingSettings,
    text: &str,
) -> Decision<CaseAnswer> {
    match ask_one(
        state,
        user,
        space,
        settings,
        message_payload(text, space.is_some()),
        Q_USE_CASE,
        case_question(settings),
    )
    .await
    {
        Decision::Answered(a) => match a.as_choice() {
            Some((key, p)) => Decision::Answered(CaseAnswer {
                key: key.to_owned(),
                probability: p.unwrap_or(0.0),
            }),
            None => Decision::Failed(DecisionFailure::InvalidAnswer),
        },
        Decision::Failed(why) => Decision::Failed(why),
    }
}

/// The agent Choice's answer.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentAnswer {
    /// The index of the chosen agent, or the strongest agent option if
    /// System One chose "No agent" but the directedness clears the floor.
    pub index: Option<usize>,
    /// The chosen option's probability (0 when none was given).
    pub probability: f64,
    /// The probability that the message is for some agent: one minus the
    /// probability of [`NO_AGENT_KEY`] when it was offered and answered,
    /// otherwise the chosen agent's probability.
    pub directed: f64,
}

impl AgentAnswer {
    pub fn meets_directedness(&self, floor: f64) -> bool {
        // A complement such as 1 - 0.8 can land just below 0.2.
        self.directed + f64::EPSILON >= floor
    }
}

fn agent_key(i: usize) -> String {
    format!("agent_{}", i + 1)
}

fn agent_question(agents: &[(String, String)], with_none: bool) -> Question {
    let mut criteria: BTreeMap<String, Value> = agents
        .iter()
        .enumerate()
        .map(|(i, (name, description))| {
            (
                agent_key(i),
                json!({"label": name, "description": description}),
            )
        })
        .collect();
    if with_none {
        criteria.insert(
            NO_AGENT_KEY.to_owned(),
            json!({"label": "No agent", "description": "Clearly only conversation between people, \
                   with nothing an agent could usefully add."}),
        );
    }
    Question::Choice {
        instructions: json!(if with_none {
            "Metro may let one agent decide whether to answer `message`. Pick the agent that would be \
             most useful to see it: anything that is a question, request, task, or a remark an agent could help with, \
             or a follow-up to an agent's recent reply in `previous`. Pick No agent only when `message` is clearly \
             people talking only to each other (greetings, social chat, a reply to another person) and no agent could \
             usefully add anything. When unsure, pick an agent."
        } else {
            "`message` asks an agent to answer or do something. Which agent fits it best?"
        }),
        criteria,
    }
}

/// One typed System One Choice over a Space's agents. A room id opts into
/// unaddressed classification with context; generic `@agent` passes `None`.
pub async fn pick_agent(
    state: &AppState,
    user: &str,
    space: &str,
    settings: &RoutingSettings,
    text: &str,
    agents: &[(String, String)],
    unaddressed_room: Option<(&str, i64)>,
) -> Decision<AgentAnswer> {
    let with_none = unaddressed_room.is_some();
    let mut payload = message_payload(text, true);
    if let Some((room, before)) = unaddressed_room {
        if settings.classifier_id.is_none() {
            return Decision::Failed(DecisionFailure::NoClassifier);
        }
        let (user, space, room) = (user.to_owned(), space.to_owned(), room.to_owned());
        match state
            .db
            .call(move |db| classifier_context(&db.lock(), &user, &space, &room, before))
            .await
        {
            Ok(Some(context)) => {
                for (key, value) in context {
                    payload[key] = value;
                }
            }
            Ok(None) => return Decision::Failed(DecisionFailure::NotAllowed),
            Err(e) => {
                tracing::warn!(error = %e, "loading Space classification context");
                return Decision::Failed(DecisionFailure::Unreachable);
            }
        }
    }
    let answer = ask_one(
        state,
        user,
        Some(space),
        settings,
        payload,
        Q_AGENT,
        agent_question(agents, with_none),
    )
    .await;
    let answer = match answer {
        Decision::Answered(a) => a,
        Decision::Failed(why) => return Decision::Failed(why),
    };
    let protocol::Answer::Choice {
        choice,
        probabilities,
        ..
    } = &answer
    else {
        return Decision::Failed(DecisionFailure::InvalidAnswer);
    };
    let probability = probabilities.get(choice).copied().unwrap_or(0.0);
    let index = if with_none && choice == NO_AGENT_KEY {
        None
    } else {
        match (0..agents.len()).find(|i| agent_key(*i) == *choice) {
            Some(i) => Some(i),
            // An option that was never offered.
            None => return Decision::Failed(DecisionFailure::InvalidAnswer),
        }
    };
    let directed = match (with_none, probabilities.get(NO_AGENT_KEY)) {
        (true, Some(none)) => 1.0 - none,
        _ if index.is_some() => probability,
        _ => 0.0,
    };
    let mut answer = AgentAnswer {
        index,
        probability,
        directed,
    };
    if answer.index.is_none() && answer.meets_directedness(settings.min_directedness) {
        // Missing scores are zero; ties retain the first agent in option order.
        let score = |i| probabilities.get(&agent_key(i)).copied().unwrap_or(0.0);
        answer.index = (0..agents.len())
            .rev()
            .max_by(|&a, &b| score(a).total_cmp(&score(b)));
        answer.probability = answer.index.map_or(0.0, score);
    }
    Decision::Answered(answer)
}

/// The room context of a classification: up to three messages or agent
/// replies before event `before` (the message being classified, which may
/// be stored already), never it or anything after it.
fn classifier_context(
    conn: &Connection,
    user: &str,
    space: &str,
    room: &str,
    before: i64,
) -> anyhow::Result<Option<[(&'static str, Value); 3]>> {
    if !crate::db::Db::room_member_in(conn, space, room, user)? {
        return Ok(None);
    }
    let (name, description, room_name): (String, String, String) = conn.query_row(
        "SELECT s.name, s.description, r.name FROM spaces s
         JOIN space_rooms r ON r.space_id = s.id WHERE s.id = ?1 AND r.id = ?2",
        params![space, room],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut stmt = conn.prepare_cached(
        "SELECT kind, actor, substr(text, 1, 400), json_extract(activity, '$.agent.name')
         FROM space_events WHERE space_id = ?1 AND room_id = ?2 AND kind IN ('message', 'agent')
           AND id < ?3
         ORDER BY id DESC LIMIT 3",
    )?;
    let mut previous = stmt
        .query_map(params![space, room, before], |r| {
            let kind: String = r.get(0)?;
            let actor: String = r.get(1)?;
            let text: String = r.get(2)?;
            let agent: Option<String> = r.get(3)?;
            Ok(json!({
                "speaker_kind": if kind == crate::db::kind::AGENT { "agent" } else { "person" },
                "speaker": agent.unwrap_or(actor),
                "text": text,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    previous.reverse();
    let description: String = description.chars().take(300).collect();
    Ok(Some([
        ("space", json!({"name": name, "description": description})),
        ("room", json!(room_name)),
        ("previous", json!(previous)),
    ]))
}

/// The route of a turn `user` starts. `chosen` (an explicit model for this
/// turn) wins and asks System One nothing; then use-case routing, if they
/// turned it on (one Choice call); then their default.
pub async fn route_turn(
    state: &AppState,
    user: &str,
    space: Option<&str>,
    settings: &RoutingSettings,
    text: &str,
    addressed_by: AddressedBy,
    chosen: Option<&str>,
) -> Route {
    if let Some(id) = chosen {
        return Route::chosen(addressed_by, id);
    }
    let fallback = |reason: String| {
        let mut route = match &settings.default_chat_id {
            Some(id) if settings.enabled => Route {
                connection_id: Some(id.clone()),
                turn_selection: None,
                blocked: None,
                sender_note: None,
                note: RouteNote {
                    source: RouteSource::RoutingDefault,
                    connection_id: Some(id.clone()),
                    ..Route::default_for(addressed_by).note
                },
            },
            _ => Route::default_for(addressed_by),
        };
        route.note.reason = reason;
        route
    };
    if !settings.wants_case_routing() {
        return if settings.enabled && settings.default_chat_id.is_some() {
            fallback("your routing default".to_owned())
        } else {
            Route::default_for(addressed_by)
        };
    }
    let answer = match use_case(state, user, space, settings, text).await {
        Decision::Answered(a) => a,
        // A failed decision is not a "no match": the person's routing
        // connection could not answer, so nothing answers in its place.
        Decision::Failed(why) => {
            let mut route = Route::default_for(addressed_by);
            let text = |why: DecisionFailure| {
                format!(
                    "Your model routing could not choose a model for this message: {why}. No \
                     model was used; check your System One connection in Settings."
                )
            };
            // In a Space room every member reads this; the detail is
            // returned to the sender alone.
            let shown = if space.is_some() { why.shared() } else { why };
            route.blocked = Some(text(shown));
            route.note.reason = format!("routing failed: {shown}");
            route.sender_note = Some(AddressingNote::RoutingFailed {
                reason: why.reason(),
                message: text(why),
            });
            return route;
        }
    };
    let min = settings.min_route_probability;
    if answer.key == NO_MATCH_KEY {
        return fallback("System One found no matching use case; using your default".to_owned());
    }
    if answer.probability < min {
        return fallback(format!(
            "no use case matched clearly (best {:.2}, need {min:.2}); using your default",
            answer.probability
        ));
    }
    // The key maps to the person's own saved connection; the answer never
    // supplies an id or endpoint.
    let Some(case) = settings.cases.iter().find(|c| c.key == answer.key) else {
        return fallback("System One named no known use case; using your default".to_owned());
    };
    Route {
        connection_id: Some(case.connection_id.clone()),
        turn_selection: None,
        blocked: None,
        sender_note: None,
        note: RouteNote {
            source: RouteSource::Case,
            addressed_by,
            case_key: Some(case.key.clone()),
            case_label: Some(case.label.clone()),
            connection_id: Some(case.connection_id.clone()),
            provider_id: None,
            model_id: None,
            reasoning_effort: None,
            reason: format!(
                "System One matched the use case \"{}\" (p {:.2})",
                case.label, answer.probability
            ),
        },
    }
}

/// The mentions of a scheduled prompt.
pub fn schedule_addressed() -> Addressed {
    Addressed {
        mentions: json!([{"kind": "agent", "addressed_by": "schedule"}]),
        audience: "agent".to_owned(),
        spans: None,
    }
}

// ---- Scheduled prompts ----

/// Dispatch one occurrence of `owner_id`'s scheduled prompt: in their
/// personal Chat (`space` `None`) or in room `space.1` of Space `space.0`.
/// Membership is rechecked now; a failed check stores nothing in the room
/// and returns the reason. The prompt is stored as an agent-directed
/// message authored by the owner, then takes exactly the routing and run
/// path of a person's `@agent` message.
pub async fn submit_scheduled(
    state: &AppState,
    owner_id: &str,
    space: Option<(&str, &str)>,
    prompt: &str,
) -> anyhow::Result<()> {
    let text = prompt.trim().to_owned();
    let max = state.config.limits.max_message_chars;
    if text.is_empty() || text.chars().count() > max {
        anyhow::bail!("the scheduled prompt must be 1..={max} characters");
    }
    let uid = owner_id.to_owned();
    let author = state
        .db
        .call(move |db| {
            let profile = db.profile(&uid)?;
            Ok(Author {
                id: Some(uid),
                name: profile.display_name,
            })
        })
        .await?;
    let settings = preferences(state, owner_id).await?;
    match space {
        None => {
            let (uid, a, t) = (owner_id.to_owned(), author.clone(), text.clone());
            let event = state
                .db
                .call(move |db| {
                    db.append_personal(&uid, kind::MESSAGE, &a, &t, &schedule_addressed())
                })
                .await?;
            let route = route_turn(
                state,
                owner_id,
                None,
                &settings,
                &text,
                AddressedBy::Schedule,
                None,
            )
            .await;
            crate::agent::start_personal_run(state, owner_id, &event, route)
                .await
                .map_err(anyhow::Error::msg)
        }
        Some((space_id, room_id)) => {
            if crate::config::validate_id(space_id).is_err()
                || crate::config::validate_id(room_id).is_err()
            {
                anyhow::bail!("the scheduled prompt's room no longer exists");
            }
            // The membership and room checks and the insert are one
            // transaction: a creator removed a moment ago cannot post. No
            // System One or model call happens before this.
            let (sid, rid, a, t) = (
                space_id.to_owned(),
                room_id.to_owned(),
                author.clone(),
                text.clone(),
            );
            let stored = state
                .db
                .call(move |db| {
                    let Some(event) = db.append_event_as_member(
                        &sid,
                        &rid,
                        kind::MESSAGE,
                        &a,
                        &t,
                        &schedule_addressed(),
                    )?
                    else {
                        return Ok(None);
                    };
                    Ok(db.get_room(&sid, &rid)?.map(|room| (event, room)))
                })
                .await?;
            let Some((event, room)) = stored else {
                anyhow::bail!(
                    "the creator is no longer a member of the Space, or the room no longer exists"
                );
            };
            // A scheduled prompt asks the Space's default agent, on the
            // creator's models for this Space; no System One addressing.
            let (sid, uid) = (space_id.to_owned(), owner_id.to_owned());
            let agent = state
                .db
                .call(move |db| db.default_space_agent(&sid))
                .await?
                .ok_or_else(|| anyhow::anyhow!("the Space has no agent"))?;
            let prefs = crate::space_prefs::load(state, space_id, &uid).await?;
            let targets = vec![crate::agent_dispatch::Target {
                agent,
                pick: Some("Scheduled prompt: default agent".to_owned()),
                addressed_by: AddressedBy::Schedule,
            }];
            let started = crate::agent_dispatch::start_runs(
                state, space_id, &room, &author, &event, targets, None, &settings, &prefs,
            )
            .await;
            if let Some(reason) = started.failures.into_iter().next() {
                anyhow::bail!(reason);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(key: &str) -> RouteCase {
        RouteCase {
            key: key.into(),
            label: "Code".into(),
            description: "Programming questions".into(),
            connection_id: format!("pc_{}", "a".repeat(32)),
        }
    }

    #[test]
    fn defaults_are_off_and_round_trip() {
        let d: RoutingSettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(d, RoutingSettings::default());
        assert!(!d.enabled && !d.infer_addressing);
        assert_eq!(d.min_directedness, 0.35);
        assert_eq!(d.min_route_probability, 0.65);
        assert!(d.check_shape().is_ok());
        assert!(serde_json::from_value::<RoutingSettings>(json!({"surprise": 1})).is_err());
    }

    #[test]
    fn shape_is_bounded() {
        let mut s = RoutingSettings {
            enabled: true,
            classifier_id: Some("x".into()),
            cases: vec![case("code")],
            ..RoutingSettings::default()
        };
        assert!(s.check_shape().is_ok());
        s.cases.push(case("code"));
        assert!(s.check_shape().is_err(), "duplicate keys");
        s.cases = (0..=MAX_CASES).map(|i| case(&format!("c{i}"))).collect();
        assert!(s.check_shape().is_err(), "too many");
        s.cases = vec![case("no_match")];
        assert!(s.check_shape().is_err(), "reserved key");
        s.cases = vec![case("Bad-Key")];
        assert!(s.check_shape().is_err());
        s.cases = vec![case("code")];
        s.min_directedness = f64::NAN;
        assert!(s.check_shape().is_err());
        s.min_directedness = 0.19;
        assert!(s.check_shape().is_err());
        s.min_directedness = 0.2;
        assert!(s.check_shape().is_ok());
        s.min_directedness = 0.9;
        s.min_route_probability = 1.5;
        assert!(s.check_shape().is_err());
        s.min_route_probability = 0.7;
        s.classifier_id = None;
        assert!(s.check_shape().is_err(), "cases need a classifier");
        s.enabled = false;
        assert!(s.check_shape().is_ok(), "off routing may keep cases");
        s.infer_addressing = true;
        assert!(s.check_shape().is_ok(), "the legacy flag is ignored");
    }

    #[test]
    fn directedness_migration_rewrites_only_old_defaults_and_runs_once() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let conn = db.lock();
        conn.execute(
            "DELETE FROM metro_meta WHERE key = 'routing_directedness_v2'",
            [],
        )
        .unwrap();
        for (id, raw) in [
            (
                "old",
                r#"{"min_directedness":0.85,"infer_addressing":true,"min_route_probability":0.8}"#,
            ),
            (
                "custom",
                r#"{"min_directedness":0.9,"infer_addressing":true}"#,
            ),
            ("missing", r#"{"infer_addressing":true}"#),
            ("bad", "not json"),
        ] {
            conn.execute(
                "INSERT INTO model_routing_settings (user_id, settings, updated_at) VALUES (?1, ?2, 42)",
                params![id, raw],
            ).unwrap();
        }
        init(&conn).unwrap();
        let old = load(&conn, "old").unwrap();
        assert_eq!(old.min_directedness, 0.35);
        assert_eq!(old.min_route_probability, 0.8);
        assert!(old.infer_addressing);
        assert_eq!(load(&conn, "custom").unwrap().min_directedness, 0.9);
        assert_eq!(load(&conn, "missing").unwrap().min_directedness, 0.35);
        assert_eq!(load(&conn, "bad").unwrap(), RoutingSettings::default());

        let updated_at: i64 = conn
            .query_row(
                "SELECT updated_at FROM model_routing_settings WHERE user_id = 'old'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(updated_at, 42);
        store(
            &conn,
            "old",
            &RoutingSettings {
                min_directedness: 0.85,
                ..old
            },
        )
        .unwrap();
        init(&conn).unwrap();
        assert_eq!(load(&conn, "old").unwrap().min_directedness, 0.85);
    }

    #[test]
    fn addressing_comes_from_explicit_recipients() {
        use crate::mentions::Mention;
        let bob = Mention::User { id: "bob".into() };
        assert_eq!(Addressing::of(&[]), Addressing::Unaddressed);
        assert_eq!(
            Addressing::of(std::slice::from_ref(&bob)),
            Addressing::PeopleOnly
        );
        assert_eq!(Addressing::of(&[bob, Mention::Agent]), Addressing::Agent);
    }

    #[test]
    fn inherited_routes_keep_the_parent_connection() {
        let parent = Route::chosen(AddressedBy::Explicit, "pc_x");
        let child = Route::inherited(&parent);
        assert_eq!(child.connection_id.as_deref(), Some("pc_x"));
        assert_eq!(child.note.source, RouteSource::Inherited);
        assert_eq!(child.note.addressed_by, AddressedBy::Thread);
        let default = Route::inherited(&Route::default_for(AddressedBy::Explicit));
        assert_eq!(default.connection_id, None);
        assert_eq!(default.note.source, RouteSource::Default);
    }
}
