//! Local composer inventory and validation. Never queries a vendor: the
//! selectable models are a connection's enabled models
//! ([`super::models`]), intersected with a device's live inventory.

use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Value, json};

use super::models::{self, EffortWire};
use super::{Access, Authorized, Capability, UseError, api, catalog};
use crate::AppState;
use crate::device_relay::ExpectedDevice;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnSelection {
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    target_id: String,
    pub(super) config_rev: i64,
    pub(super) device: Option<ExpectedDevice>,
    /// A self-hosted endpoint's wire for the effort (the model's own
    /// thinking options); `None` for providers with a documented wire.
    pub(super) wire: Option<EffortWire>,
}

impl TurnSelection {
    pub(crate) fn params(&self, provider: &str) -> Option<Value> {
        let effort = self.reasoning_effort.as_deref()?;
        if let Some(wire) = self.wire {
            return wire.params(effort);
        }
        match provider {
            catalog::DEVICE_OPENAI_OAUTH => Some(json!({"reasoning_effort": effort})),
            "openai" => Some(json!({"reasoning": {"effort": effort}})),
            "anthropic" => Some(json!({"output_config": {"effort": effort}})),
            _ => None,
        }
    }

    pub(super) fn recheck(
        &self,
        state: &AppState,
        conn: &Connection,
        a: &Authorized,
    ) -> Result<(), UseError> {
        let now = checked(
            state,
            conn,
            a,
            Some(&self.model_id),
            self.reasoning_effort.as_deref(),
        )?;
        (now == *self).then_some(()).ok_or(UseError::TurnChanged)
    }
}

/// One selectable model as pickers show it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ChatModel {
    pub id: String,
    pub display_name: String,
    pub reasoning_efforts: Vec<String>,
    /// The connection's default model.
    pub is_default: bool,
    /// `false` when the provider's last listing no longer had it.
    pub available: bool,
    #[serde(skip)]
    wire: Option<EffortWire>,
}

pub(super) fn models(
    state: &AppState,
    conn: &Connection,
    a: &Authorized,
) -> Result<Vec<ChatModel>, UseError> {
    candidates(state, conn, a).map(|(models, _)| models)
}

fn internal(e: rusqlite::Error) -> UseError {
    tracing::error!(error = %e, "reading a connection's models");
    UseError::Internal
}

/// A model not (yet) in the connection's list.
fn synthetic(id: &str) -> models::ModelRow {
    models::ModelRow {
        model_id: id.to_owned(),
        display_name: None,
        listed_name: None,
        enabled: true,
        available: true,
        source: models::Source::Listed,
        position: 0,
        thinking: None,
        context_tokens: None,
    }
}

fn candidates(
    state: &AppState,
    conn: &Connection,
    a: &Authorized,
) -> Result<(Vec<ChatModel>, Option<ExpectedDevice>), UseError> {
    let row = &a.row;
    if row.capability != Capability::Chat {
        return Err(UseError::WrongCapability {
            expected: Capability::Chat,
            actual: row.capability,
        });
    }
    let entry = catalog::find(&row.provider_id).ok_or(UseError::Unsupported)?;
    // A preset names one model and runs it on the caller's own connection.
    let preset = matches!(a.access, Access::Preset { .. });
    let listed = models::enabled(conn, &row.id).map_err(internal)?;
    let pick = |ids: Option<&[String]>| -> Vec<models::ModelRow> {
        if preset {
            let own = listed.iter().find(|m| m.model_id == row.model).cloned();
            let m = own.unwrap_or_else(|| synthetic(&row.model));
            return match ids {
                Some(ids) if !ids.contains(&m.model_id) => vec![],
                _ => vec![m],
            };
        }
        listed
            .iter()
            .filter(|m| ids.is_none_or(|ids| ids.contains(&m.model_id)))
            .cloned()
            .collect()
    };
    let (rows, modern, device) = match entry.adapter {
        catalog::Adapter::Device => {
            let (generation, inventory) = state
                .device
                .chat_inventory(&row.id)
                .ok_or(UseError::DeviceOffline)?;
            let modern = inventory.is_some();
            let rows = match &inventory {
                Some(ids) if preset => pick(Some(ids)),
                // The device's live inventory is its listing: every model it
                // approves is selectable unless the owner disabled it here
                // (opt-out), with the owner's names.
                Some(ids) => {
                    let all = models::list(conn, &row.id).map_err(internal)?;
                    ids.iter()
                        .filter_map(|id| match all.iter().find(|m| &m.model_id == id) {
                            Some(m) if m.enabled => Some(m.clone()),
                            Some(_) => None,
                            None => Some(synthetic(id)),
                        })
                        .collect()
                }
                // A legacy connector without an inventory: the connection's
                // model only, as before model lists.
                None => pick(None)
                    .into_iter()
                    .filter(|m| m.model_id == row.model)
                    .collect(),
            };
            (
                rows,
                modern,
                Some(ExpectedDevice {
                    generation,
                    advertised: modern,
                }),
            )
        }
        catalog::Adapter::Registered(_) | catalog::Adapter::Custom => {
            // Verify the saved key and endpoint locally without a model list call.
            state.providers.server_config(entry, row)?;
            (pick(None), true, None)
        }
        _ => return Err(UseError::Unsupported),
    };
    let out = rows
        .into_iter()
        .filter(|m| !m.model_id.is_empty())
        .map(|m| {
            let (reasoning_efforts, wire) = models::efforts_of(row, entry, &m, modern);
            ChatModel {
                display_name: m.name().to_owned(),
                is_default: m.model_id == row.model,
                available: m.available,
                id: m.model_id,
                reasoning_efforts,
                wire,
            }
        })
        .collect();
    Ok((out, device))
}

pub(crate) async fn check(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    connection: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<TurnSelection, UseError> {
    if !api::valid_connection_id(connection) {
        return Err(UseError::NotFound);
    }
    let (st, c, s, i) = (
        state.clone(),
        caller.to_owned(),
        space.map(str::to_owned),
        connection.to_owned(),
    );
    let (m, e) = (model.map(str::to_owned), effort.map(str::to_owned));
    state
        .db
        .call(move |db| {
            let conn = db.lock();
            Ok(super::sharing::authorize(&conn, &c, s.as_deref(), &i)
                .and_then(|a| checked(&st, &conn, &a, m.as_deref(), e.as_deref())))
        })
        .await
        .map_err(|e| {
            tracing::error!(error = %format!("{e:#}"), "checking a turn's model");
            UseError::Internal
        })?
}

pub(super) fn checked(
    state: &AppState,
    conn: &Connection,
    authorized: &Authorized,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<TurnSelection, UseError> {
    let model = model.unwrap_or(&authorized.row.model);
    // Do not normalize turn IDs or accept whitespace, empty or long strings.
    if model.is_empty() || !api::check_model(model).is_ok_and(|s| s == model) {
        return Err(UseError::TurnModelNotAllowed);
    }
    let (models, device) = candidates(state, conn, authorized)?;
    let candidate = models
        .into_iter()
        .find(|m| m.id == model)
        .ok_or(UseError::TurnModelNotAllowed)?;
    if let Some(effort) = effort
        && !candidate.reasoning_efforts.iter().any(|e| e == effort)
    {
        return Err(UseError::TurnEffortNotAllowed);
    }
    Ok(TurnSelection {
        model_id: candidate.id,
        reasoning_effort: effort.map(str::to_owned),
        target_id: authorized.row.id.clone(),
        config_rev: authorized.row.config_rev,
        device,
        wire: candidate.wire,
    })
}

#[cfg(test)]
mod tests {
    use rig_core::catalog::Catalog;
    use rig_core::completion::CompletionRequest;
    use rig_core::providers::anthropic::AnthropicConfig;
    use rig_core::providers::openai::OpenAIConfig;
    use rig_core::providers::openai::responses_api::wire::Responses;
    use rig_core::providers::registry::{Format, ProviderId};
    use rig_core::wire::{Body, Mode, Wire};

    use super::super::models::{anthropic_efforts, chatgpt_efforts, openai_efforts};
    use super::*;

    fn selection(model: &str, effort: Option<&str>, wire: Option<EffortWire>) -> TurnSelection {
        TurnSelection {
            model_id: model.into(),
            reasoning_effort: effort.map(str::to_owned),
            target_id: "test".into(),
            config_rev: 0,
            device: None,
            wire,
        }
    }

    #[test]
    fn official_openai_effort_uses_the_responses_wire_only_when_selected() {
        let mut selection = selection("gpt-5.5", Some("xhigh"), None);
        let wire = Responses::new(OpenAIConfig::new("offline-fixture"), &selection.model_id);
        for expected in [json!("xhigh"), Value::Null] {
            let request =
                CompletionRequest::new("hello").additional_params(selection.params("openai"));
            let encoded = wire.encode(request, Mode::Streaming).unwrap();
            assert_eq!(encoded.request.uri().path(), "/v1/responses");
            let Body::Bytes(bytes) = encoded.request.body() else {
                panic!("expected JSON")
            };
            let json: Value = serde_json::from_slice(bytes.as_slice()).unwrap();
            assert_eq!(json["model"], "gpt-5.5");
            assert_eq!(json["reasoning"]["effort"], expected);
            assert!(json.get("reasoning_effort").is_none());
            selection.reasoning_effort = None;
        }
        assert!(openai_efforts("gpt-5.5-unknown").is_empty());
        assert!(!chatgpt_efforts("gpt-5.5").contains(&"none"));
        assert!(openai_efforts("gpt-5.5").contains(&"none"));
    }

    #[test]
    fn custom_endpoint_effort_uses_the_models_own_wire() {
        let s = selection("local", Some("high"), Some(EffortWire::ReasoningEffort));
        assert_eq!(
            s.params(catalog::CUSTOM),
            Some(json!({"reasoning_effort": "high"}))
        );
        let s = selection("local", Some("high"), Some(EffortWire::Reasoning));
        assert_eq!(
            s.params(catalog::CUSTOM),
            Some(json!({"reasoning": {"effort": "high"}}))
        );
        let s = selection("local", Some("high"), Some(EffortWire::None));
        assert_eq!(s.params(catalog::CUSTOM), None);
        let s = selection("local", None, Some(EffortWire::Reasoning));
        assert_eq!(s.params(catalog::CUSTOM), None);
        // Without a custom wire, an unknown provider still sends nothing.
        let s = selection("local", Some("high"), None);
        assert_eq!(s.params(catalog::CUSTOM), None);
    }

    #[test]
    fn canonical_anthropic_effort_encodes_in_messages_and_keeps_max_tokens() {
        let provider = ProviderId::catalog("anthropic").unwrap();
        let entry = catalog::find("anthropic").unwrap();
        assert!(matches!(entry.adapter, catalog::Adapter::Registered(id) if id == provider));
        assert_eq!(provider.format(), Some(Format::Anthropic));
        for model in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"] {
            let facts = Catalog::builtin().facts(provider, model);
            // Match the existing agent output cap for unknown models.
            let max_tokens = facts
                .spec()
                .is_none()
                .then_some(super::super::UNCATALOGUED_MAX_TOKENS);
            let wire = AnthropicConfig::new("offline-fixture")
                .completion(model)
                .with_facts(facts);
            let cap = max_tokens.or(wire.default_max_tokens).unwrap();
            assert_eq!(
                anthropic_efforts(model),
                ["low", "medium", "high", "xhigh", "max"]
            );
            for effort in anthropic_efforts(model)
                .iter()
                .copied()
                .map(Some)
                .chain([None])
            {
                let selection = selection(model, effort, None);
                let request = CompletionRequest::new("hello")
                    .max_tokens(max_tokens)
                    .additional_params(selection.params("anthropic"));
                let encoded = wire.encode(request, Mode::Streaming).unwrap();
                assert_eq!(encoded.request.uri().host(), Some("api.anthropic.com"));
                assert_eq!(encoded.request.uri().path(), "/v1/messages");
                assert!(encoded.request.headers().contains_key("anthropic-version"));
                let Body::Bytes(bytes) = encoded.request.body() else {
                    panic!("expected JSON")
                };
                let body: Value = serde_json::from_slice(bytes.as_slice()).unwrap();
                assert_eq!(body["model"], model);
                assert_eq!(body["max_tokens"], cap);
                if let Some(effort) = effort {
                    assert_eq!(body["output_config"], json!({"effort": effort}));
                } else {
                    assert!(body.get("output_config").is_none());
                }
            }
        }
        assert!(anthropic_efforts("claude-opus-5-5-other").is_empty());
    }
}
