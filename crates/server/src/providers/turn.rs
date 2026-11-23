//! Local composer inventory and validation. Never queries a vendor.

use rig_core::providers::registry::Format;
use serde::Serialize;
use serde_json::{Value, json};

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
}

impl TurnSelection {
    pub(crate) fn params(&self, provider: &str) -> Option<Value> {
        let effort = self.reasoning_effort.as_deref()?;
        match provider {
            catalog::DEVICE_OPENAI_OAUTH => Some(json!({"reasoning_effort": effort})),
            "openai" => Some(json!({"reasoning": {"effort": effort}})),
            "anthropic" => Some(json!({"output_config": {"effort": effort}})),
            _ => None,
        }
    }

    pub(super) fn recheck(&self, state: &AppState, a: &Authorized) -> Result<(), UseError> {
        let now = checked(
            state,
            a,
            Some(&self.model_id),
            self.reasoning_effort.as_deref(),
        )?;
        (now == *self).then_some(()).ok_or(UseError::TurnChanged)
    }
}

#[derive(Debug, Serialize)]
pub struct ChatModel {
    pub id: String,
    pub reasoning_efforts: &'static [&'static str],
}

// Exact model IDs and supported levels from developers.openai.com/api/docs/models
// (not name prefixes, third-party gateways or arbitrary compatible endpoints).
fn openai_efforts(id: &str) -> &'static [&'static str] {
    match id {
        "gpt-6.2-astra" | "gpt-6.2-sol" | "gpt-6.1-sol" => {
            &["low", "medium", "high", "xhigh", "max"]
        }
        "gpt-5.6" | "gpt-5.6-sol" | "gpt-5.6-terra" => {
            &["none", "low", "medium", "high", "xhigh", "max"]
        }
        "gpt-5.5"
        | "gpt-5.5-2026-04-23"
        | "gpt-5.4"
        | "gpt-5.4-2026-03-05"
        | "gpt-5.4-mini"
        | "gpt-5.4-mini-2026-03-17"
        | "gpt-5.4-nano"
        | "gpt-5.4-nano-2026-03-17"
        | "gpt-5.2"
        | "gpt-5.2-2025-12-11" => &["none", "low", "medium", "high", "xhigh"],
        "gpt-5.3-codex" => &["low", "medium", "high", "xhigh"],
        "gpt-5.1" | "gpt-5.1-2025-11-13" => &["none", "low", "medium", "high"],
        "gpt-5.5-pro" => &["medium", "high", "xhigh"],
        _ => &[],
    }
}

// ChatGPT sign-in follows the Codex catalog. Intersect with the levels the
// connector's wire accepts, not the public API-key model's extra levels.
// https://github.com/openai/codex/blob/6c0cee502d215e5df2dd9a303cf2e252229c9e12/codex-rs/models-manager/models.json
fn chatgpt_efforts(id: &str) -> &'static [&'static str] {
    match id {
        "gpt-6.2-astra" | "gpt-6.2-sol" | "gpt-6-astra" | "gpt-6.1-sol" | "gpt-6.1-terra"
        | "gpt-6-sol" | "gpt-6-luna" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" => {
            &["low", "medium", "high", "xhigh", "max"]
        }
        "gpt-5.5" => &["low", "medium", "high", "xhigh"],
        _ => &[],
    }
}

// https://platform.claude.com/docs/en/build-with-claude/effort
fn anthropic_efforts(id: &str) -> &'static [&'static str] {
    match id {
        "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-haiku-5-5" => {
            &["low", "medium", "high", "xhigh", "max"]
        }
        _ => &[],
    }
}

pub(super) fn models(state: &AppState, a: &Authorized) -> Result<Vec<ChatModel>, UseError> {
    candidates(state, a).map(|(models, _)| models)
}

fn candidates(
    state: &AppState,
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
    let (ids, efforts, device) = match entry.adapter {
        catalog::Adapter::Device => {
            let (generation, inventory) = state
                .device
                .chat_inventory(&row.id)
                .ok_or(UseError::DeviceOffline)?;
            let modern = inventory.is_some();
            let ids = match inventory {
                Some(ids) if a.access == Access::Own => ids,
                Some(ids) if ids.contains(&row.model) => vec![row.model.clone()],
                Some(_) => vec![],
                None => vec![row.model.clone()],
            };
            let efforts: fn(&str) -> &'static [&'static str] =
                if modern && row.provider_id == catalog::DEVICE_OPENAI_OAUTH {
                    chatgpt_efforts
                } else {
                    |_| &[]
                };
            (
                ids,
                efforts,
                Some(ExpectedDevice {
                    generation,
                    advertised: modern,
                }),
            )
        }
        catalog::Adapter::Registered(_) | catalog::Adapter::Custom => {
            // Verify the saved key and endpoint locally without a model list call.
            state.providers.server_config(entry, row)?;
            let efforts: fn(&str) -> &'static [&'static str] = match entry.adapter {
                catalog::Adapter::Registered(id)
                    if row.base_url.is_none()
                        && entry.id == "openai"
                        && id.format() == Some(Format::OpenAi) =>
                {
                    openai_efforts
                }
                catalog::Adapter::Registered(id)
                    if row.base_url.is_none()
                        && entry.id == "anthropic"
                        && id.format() == Some(Format::Anthropic) =>
                {
                    anthropic_efforts
                }
                _ => |_| &[],
            };
            let ids = if a.access == Access::Own {
                state.providers.listed_chat_models(row)
            } else {
                vec![row.model.clone()]
            };
            (ids, efforts, None)
        }
        _ => return Err(UseError::Unsupported),
    };
    let models = ids
        .into_iter()
        .filter(|id| !id.is_empty())
        .map(|id| ChatModel {
            reasoning_efforts: efforts(&id),
            id,
        })
        .collect();
    Ok((models, device))
}

pub(crate) async fn check(
    state: &AppState,
    caller: &str,
    space: Option<&str>,
    connection: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<TurnSelection, UseError> {
    let authorized = super::resolve(state, caller, space, connection).await?;
    checked(state, &authorized, model, effort)
}

pub(super) fn checked(
    state: &AppState,
    authorized: &Authorized,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<TurnSelection, UseError> {
    let model = model.unwrap_or(&authorized.row.model);
    // Do not normalize turn IDs or accept whitespace, empty or long strings.
    if model.is_empty() || !api::check_model(model).is_ok_and(|s| s == model) {
        return Err(UseError::TurnModelNotAllowed);
    }
    let (models, device) = candidates(state, authorized)?;
    let candidate = models
        .into_iter()
        .find(|m| m.id == model)
        .ok_or(UseError::TurnModelNotAllowed)?;
    if let Some(effort) = effort
        && !candidate.reasoning_efforts.contains(&effort)
    {
        return Err(UseError::TurnEffortNotAllowed);
    }
    Ok(TurnSelection {
        model_id: candidate.id,
        reasoning_effort: effort.map(str::to_owned),
        target_id: authorized.row.id.clone(),
        config_rev: authorized.row.config_rev,
        device,
    })
}

#[cfg(test)]
mod tests {
    use rig_core::catalog::Catalog;
    use rig_core::completion::CompletionRequest;
    use rig_core::providers::anthropic::AnthropicConfig;
    use rig_core::providers::openai::OpenAIConfig;
    use rig_core::providers::openai::responses_api::wire::Responses;
    use rig_core::providers::registry::ProviderId;
    use rig_core::wire::{Body, Mode, Wire};

    use super::*;

    #[test]
    fn official_openai_effort_uses_the_responses_wire_only_when_selected() {
        let mut selection = TurnSelection {
            model_id: "gpt-5.5".into(),
            reasoning_effort: Some("xhigh".into()),
            target_id: "test".into(),
            config_rev: 0,
            device: None,
        };
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
                let selection = TurnSelection {
                    model_id: model.to_owned(),
                    reasoning_effort: effort.map(str::to_owned),
                    target_id: "test".into(),
                    config_rev: 0,
                    device: None,
                };
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
