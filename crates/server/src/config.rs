//! Server configuration, read once from the environment at startup.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};

/// An OpenAI-compatible chat completions endpoint for the agent.
#[derive(Clone)]
pub struct ModelConfig {
    pub base_url: String,
    pub model: String,
    /// Empty when the endpoint needs no key.
    pub api_key: String,
}

impl std::fmt::Debug for ModelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field(
                "api_key",
                &if self.api_key.is_empty() {
                    "<none>"
                } else {
                    "<redacted>"
                },
            )
            .finish()
    }
}

/// Bounds on the queue, the agent and request inputs.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Jobs waiting for one Space's runner at once; more are refused.
    pub queue_capacity: usize,
    /// How long a job may wait to be claimed before it fails.
    pub claim_timeout: Duration,
    /// How long a claimed job may run before it fails.
    pub run_timeout: Duration,
    /// Agent runs at once across the server (Spaces and personal Chat).
    pub max_agent_runs: usize,
    /// Agent runs at once in one Space, threads included.
    pub max_space_runs: usize,
    /// Demo runs at once.
    pub max_demo_runs: usize,
    /// Model calls in one agent run, tool round trips included.
    pub max_agent_turns: usize,
    /// Wall clock for one agent run.
    pub agent_timeout: Duration,
    /// Earlier room events given to the model as history.
    pub agent_history: usize,
    /// Longest message text, in characters.
    pub max_message_chars: usize,
    /// Longest room or Space name, in characters.
    pub max_room_name_chars: usize,
    /// Most rooms (threads included) in one Space.
    pub max_rooms: usize,
    /// Most threads under one top-level room.
    pub max_threads_per_room: usize,
    /// Most reply threads in one Space; they count toward neither of the above.
    pub max_reply_threads: usize,
    /// Most Spaces one user may own.
    pub max_spaces_per_owner: usize,
    /// Most members (owner included) in one Space.
    pub max_members: usize,
    /// Longest thread task or message from a parent agent, in characters.
    pub max_thread_text_chars: usize,
    /// Longest runner result output, in bytes.
    pub max_result_bytes: usize,
    /// Longest tool output passed back to the model, in bytes.
    pub max_tool_output_to_model: usize,
    /// Longest text stored in one tool or agent event, in bytes.
    pub max_event_text: usize,
    /// Events returned by one `GET .../events` page.
    pub events_page: usize,
    /// Most personal registered runners one user may own.
    pub max_personal_runners: usize,
    /// Most organization runners on the server.
    pub max_org_runners: usize,
    /// Monitoring events kept per Space, and for the instance.
    pub monitoring_events_per_scope: usize,
    /// How long monitoring events are kept.
    pub monitoring_max_age: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            queue_capacity: 32,
            claim_timeout: Duration::from_secs(30),
            run_timeout: Duration::from_secs(120),
            max_agent_runs: 8,
            max_space_runs: 4,
            max_demo_runs: 4,
            max_agent_turns: 12,
            agent_timeout: Duration::from_secs(600),
            agent_history: 40,
            max_message_chars: 8_000,
            max_room_name_chars: 64,
            max_rooms: 200,
            max_threads_per_room: 16,
            max_reply_threads: 5_000,
            max_spaces_per_owner: 50,
            max_members: 100,
            max_thread_text_chars: 8_000,
            max_result_bytes: 1024 * 1024,
            max_tool_output_to_model: 64 * 1024,
            max_event_text: 16 * 1024,
            events_page: 500,
            max_personal_runners: 20,
            max_org_runners: 100,
            monitoring_events_per_scope: 500,
            monitoring_max_age: Duration::from_secs(30 * 24 * 3600),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub db_path: PathBuf,
    /// `None` when `METRO_MODEL` is unset: Ask agent is unavailable.
    pub model: Option<ModelConfig>,
    pub limits: Limits,
    /// Stable user ids (`sub`, or the dev username) of the organization's
    /// admins, from `METRO_ORG_ADMINS`. Empty means nobody can create or
    /// manage organization runners.
    pub org_admins: Vec<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = env("METRO_BIND")
            .unwrap_or_else(|| "127.0.0.1:8787".to_owned())
            .parse::<SocketAddr>()
            .context("METRO_BIND must be host:port, such as 127.0.0.1:8787")?;
        let db_path = PathBuf::from(env("METRO_DB").unwrap_or_else(|| "metro.sqlite3".to_owned()));
        for retired in ["METRO_RUNNER_TOKEN", "METRO_RUNNER_ROOM"] {
            if env(retired).is_some() {
                tracing::warn!(
                    "{retired} is ignored by the server: runner tokens are issued per Space \
                     with POST /v1/spaces/{{sid}}/runner-token"
                );
            }
        }

        let model = env("METRO_MODEL").map(|model| ModelConfig {
            base_url: env("METRO_MODEL_BASE_URL")
                .unwrap_or_else(|| "https://api.openai.com/v1".to_owned())
                .trim_end_matches('/')
                .to_owned(),
            model,
            api_key: env("METRO_MODEL_API_KEY").unwrap_or_default(),
        });
        if let Some(model) = &model
            && !(model.base_url.starts_with("http://") || model.base_url.starts_with("https://"))
        {
            bail!("METRO_MODEL_BASE_URL must start with http:// or https://");
        }

        let org_admins = parse_org_admins(env("METRO_ORG_ADMINS").as_deref())?;

        Ok(Self {
            bind,
            db_path,
            model,
            limits: Limits::default(),
            org_admins,
        })
    }

    /// Whether `user_id` is an organization admin.
    pub fn is_org_admin(&self, user_id: &str) -> bool {
        self.org_admins.iter().any(|a| a == user_id)
    }
}

/// `METRO_ORG_ADMINS`: a JSON array of stable user ids, such as
/// `["alice","00u1abcd"]`. Unset or blank means no admins.
pub fn parse_org_admins(raw: Option<&str>) -> anyhow::Result<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let ids: Vec<String> = serde_json::from_str(raw)
        .context("METRO_ORG_ADMINS must be a JSON array of stable user ids, such as [\"alice\"]")?;
    let mut out: Vec<String> = Vec::with_capacity(ids.len());
    for id in ids {
        if !crate::auth::valid_id(&id) {
            bail!(
                "METRO_ORG_ADMINS entries must be 1..=256 printable characters without surrounding spaces"
            );
        }
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// A set, non-blank environment variable.
fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Space and room ids are short ASCII slugs: `[a-z0-9_-]{1,64}`.
pub fn validate_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        bail!("id must match [a-z0-9_-]{{1,64}}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_admins_parse() {
        assert!(parse_org_admins(None).unwrap().is_empty());
        assert!(parse_org_admins(Some("[]")).unwrap().is_empty());
        assert_eq!(
            parse_org_admins(Some(r#"["alice","bob","alice"]"#)).unwrap(),
            vec!["alice".to_owned(), "bob".to_owned()]
        );
        assert!(parse_org_admins(Some("alice")).is_err());
        assert!(parse_org_admins(Some(r#"[" alice"]"#)).is_err());
        assert!(parse_org_admins(Some(r#"[""]"#)).is_err());
        assert!(parse_org_admins(Some(r#"[1]"#)).is_err());
    }
}
