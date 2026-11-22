//! The provider catalog: which model adapters this build can instantiate.
//!
//! Server-side entries come from Rig's compiled registry
//! ([`ProviderId::all`]), filtered to the adapters Metro can actually drive
//! with a plain API key and its agent tools, plus one custom
//! OpenAI-compatible (Chat Completions) adapter for operator-approved
//! endpoints. Device entries are run by the user's own `metro-connector`;
//! the server never holds their vendor credentials.
//!
//! Building the catalog reads nothing but compiled tables: no network, no
//! environment, no database.

use std::sync::LazyLock;

use rig_core::providers::openai::Route;
use rig_core::providers::openai::wire::{Auth, Dialect, Quirks};
use rig_core::providers::registry::{Format, ProviderId};
use rig_core::providers::{anthropic, gemini, openai};
use serde::Serialize;

/// The id of the custom OpenAI-compatible adapter.
pub const CUSTOM: &str = "custom";

/// Device adapters: run on the owner's machine by `metro-connector`.
pub const DEVICE_OPENAI_OAUTH: &str = "device-openai-oauth";
pub const DEVICE_CLAUDE_CODE: &str = "device-claude-code";
pub const DEVICE_OPENAI_COMPATIBLE: &str = "device-openai-compatible";
/// System One on the owner's machine, relayed with the `decide` operation.
pub const DEVICE_SYSTEMONE: &str = "device-systemone";
/// OpenAI-compatible embeddings on the owner's machine, relayed with `embed`.
pub const DEVICE_EMBEDDINGS: &str = "device-embeddings";
/// A media embedder (EmbeddingGemma 2 or a compatible model service) on the
/// owner's machine: `embed` for text and `media_embed` for documents.
pub const DEVICE_EMBEDDINGGEMMA: &str = "device-embeddinggemma-2";

/// TypeSafe's hosted System One API (`POST /v1/systemone`, `GET /v1/models`),
/// as documented at <https://docs.typesafe.ai/api/system-one>.
pub const SYSTEMONE: &str = "systemone";
/// The System One wire on an operator-approved endpoint.
pub const CUSTOM_SYSTEMONE: &str = "custom-systemone";
/// OpenAI's own `POST /v1/embeddings`.
pub const OPENAI_EMBEDDINGS: &str = "openai-embeddings";
/// OpenAI-compatible `POST /v1/embeddings` on an operator-approved endpoint.
pub const CUSTOM_EMBEDDINGS: &str = "custom-embeddings";
/// A self-hosted media embedder (EmbeddingGemma 2 or a compatible model
/// service) on an operator-approved endpoint: `POST {base}/embeddings` for
/// text and Metro's `POST {base}/media/embeddings` for documents and media.
pub const EMBEDDINGGEMMA: &str = "embeddinggemma-2";

/// The model the media adapters suggest. Any other model the chosen
/// endpoint lists can be picked instead.
pub const EMBEDDINGGEMMA_MODEL: &str = "google/embeddinggemma-2";
/// What the media adapters' model embeds natively. Files are sent only as
/// `metro_model_protocol::MEDIA_MIME_TYPES`; PDF and DOCX are prepared by
/// the model service (pages extracted or rendered) before the model sees
/// them.
pub const MEDIA_MODALITIES: &[&str] = &["text", "image", "audio", "video"];

/// TypeSafe's public API base: requests go to `{base}/systemone` and
/// `{base}/models`. Verified against <https://api.typesafe.ai/openapi.json>
/// (servers `https://api.typesafe.ai`, paths `/v1/systemone`, `/v1/models`).
pub const SYSTEMONE_BASE_URL: &str = "https://api.typesafe.ai/v1";
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// The account-bound subscription sign-in: never shared, only published as a
/// bring-your-own preset.
pub fn is_subscription(id: &str) -> bool {
    id == DEVICE_OPENAI_OAUTH
}

/// Chat Completions with an optional bearer key: no `Authorization` header at
/// all when the connection has no key. Its base URL always comes from
/// `METRO_PROVIDER_ALLOWED_BASE_URLS`, so the dialect names none.
pub(crate) const CUSTOM_DIALECT: Dialect =
    Dialect::gateway("metro-custom-openai-compatible", "", "").with_quirks(custom_quirks());

const fn custom_quirks() -> Quirks {
    let mut quirks = Quirks::openai();
    quirks.auth = Auth::OptionalBearer;
    quirks
}

/// Registered selections Metro does not offer, and why. Everything else in
/// [`ProviderId::all`] is offered.
const EXCLUDED: &[(&str, &str)] = &[
    // A ChatGPT subscription token plus account id, not an API key.
    ("chatgpt", "needs a ChatGPT sign-in, not an API key"),
    // A short-lived session token exchanged from a GitHub OAuth token.
    ("copilot", "needs an exchanged short-lived session token"),
];

/// Where an adapter runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Execution {
    /// The server calls the provider with a key it stores encrypted.
    Server,
    /// The owner's `metro-connector` calls the provider; the server relays.
    Device,
}

/// What a connection is for. Fixed by its adapter when it is created; a
/// connection is never used for another capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Answers and tool turns for agent runs.
    Chat,
    /// Typed System One decisions (`noul`, `choice`, `score`).
    SystemOne,
    /// Vectors for semantic search.
    Embeddings,
}

impl Capability {
    pub const ALL: [Capability; 3] = [
        Capability::Chat,
        Capability::SystemOne,
        Capability::Embeddings,
    ];

    /// The stored and wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Chat => "chat",
            Capability::SystemOne => "system_one",
            Capability::Embeddings => "embeddings",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// How an entry is instantiated.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug)]
pub enum Adapter {
    /// A Rig registry selection, built with [`ProviderId::config`].
    Registered(ProviderId),
    /// [`CUSTOM_DIALECT`] on an approved base URL.
    Custom,
    /// The System One wire (`POST {base}/systemone`).
    SystemOne,
    /// OpenAI-compatible embeddings (`POST {base}/embeddings`).
    Embeddings,
    /// Relayed to the owner's device.
    Device,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub adapter: Adapter,
    pub execution: Execution,
    /// The one capability this adapter serves.
    pub capability: Capability,
    /// The wire protocol family: `openai`, `anthropic`, `gemini`,
    /// `systemone`, `openai-embeddings` or `device`.
    pub format: &'static str,
    /// The endpoint the adapter speaks.
    pub api: &'static str,
    pub requires_key: bool,
    /// The dialect's own HTTPS base URL. Plain-HTTP defaults (local servers)
    /// are not offered: such endpoints must be approved by the operator.
    pub default_base_url: Option<String>,
    /// Whether the adapter implements a model listing.
    pub lists_models: bool,
    /// Whether the adapter embeds documents and media (`media_embed`), not
    /// only text. Only the EmbeddingGemma 2 adapters.
    pub media_inputs: bool,
    /// What it embeds natively (empty for text-only adapters).
    pub modalities: &'static [&'static str],
    /// A model to start with; the endpoint's own list decides what exists.
    pub default_model: Option<&'static str>,
}

impl Entry {
    pub fn is_device(&self) -> bool {
        self.execution == Execution::Device
    }
}

/// What `GET /v1/provider-catalog` shows for one entry.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct EntryView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub execution: Execution,
    pub capability: Capability,
    pub format: &'static str,
    pub api: &'static str,
    pub requires_key: bool,
    /// Whether the user may pick one of `allowed_base_urls` for it.
    pub allow_custom_url: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_base_url: Option<String>,
    pub lists_models: bool,
    /// Whether connections of this adapter accept documents and media.
    pub media_inputs: bool,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub modalities: &'static [&'static str],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<&'static str>,
}

fn display_name(vendor: &str) -> &str {
    match vendor {
        "openai" => "OpenAI",
        "azure.openai" => "Azure OpenAI",
        "deepseek" => "DeepSeek",
        "groq" => "Groq",
        "together" => "Together AI",
        "huggingface" => "Hugging Face",
        "llamacpp" => "llama.cpp server",
        "mistral" => "Mistral",
        "openrouter" => "OpenRouter",
        "venice" => "Venice",
        "doubleword" => "Doubleword",
        "zai" => "Z.ai",
        "minimax" => "MiniMax",
        "moonshot" => "Moonshot AI",
        "xiaomimimo" => "Xiaomi MiMo",
        "cohere" => "Cohere",
        "ollama" => "Ollama",
        "opencode" => "OpenCode Zen",
        "opencode-go" => "OpenCode Go",
        "xai" => "xAI",
        "anthropic" => "Anthropic",
        "gcp.gemini" => "Google Gemini",
        other => other,
    }
}

fn api_name(format: Format, route: Option<Route>) -> &'static str {
    match (format, route) {
        (Format::OpenAi, Some(Route::Responses)) => "OpenAI Responses",
        (Format::OpenAi, _) => "OpenAI Chat Completions",
        (Format::Anthropic, _) => "Anthropic Messages",
        (Format::Gemini, _) => "Gemini GenerateContent",
    }
}

fn https(url: &str) -> Option<String> {
    url.starts_with("https://")
        .then(|| url.trim_end_matches('/').to_owned())
}

/// One registered selection as an entry, or `None` when Metro cannot drive
/// it (see [`EXCLUDED`], and adapters without tool calling: every Metro agent
/// run offers tools).
fn registered(id: ProviderId) -> Option<Entry> {
    let format = id.format()?;
    let vendor = id.vendor();
    if EXCLUDED.iter().any(|(v, _)| *v == vendor) || id.short_lived_key() {
        return None;
    }
    let (default_base_url, route, lists_models) = match format {
        Format::OpenAi => {
            let dialect = openai::wire::by_name(vendor)?;
            if !dialect.quirks.supports_tools {
                return None;
            }
            (
                https(dialect.base_url),
                Some(dialect.quirks.completion_route),
                !dialect.quirks.models_path.is_empty(),
            )
        }
        Format::Anthropic => {
            let dialect = anthropic::wire::Dialect::by_name(vendor)?;
            (https(dialect.base_url), None, true)
        }
        Format::Gemini => (https(gemini::BASE_URL), None, true),
    };
    // A vendor registered in two families: its primary family keeps the bare
    // vendor id, the other is `vendor/format`.
    let several = ProviderId::vendor_selections(vendor).count() > 1;
    let primary = ProviderId::catalog(vendor).is_some_and(|p| p == id);
    let api = api_name(format, route);
    let (key, name) = if several && !primary {
        (
            format!("{vendor}/{}", format.as_str()),
            format!("{} ({api})", display_name(vendor)),
        )
    } else if several {
        (
            vendor.to_owned(),
            format!("{} ({api})", display_name(vendor)),
        )
    } else {
        (vendor.to_owned(), display_name(vendor).to_owned())
    };
    let requires_key = id.requires_credential();
    let description = match (&default_base_url, requires_key) {
        (Some(_), true) => format!("{api} with your own API key, called by the Metro server."),
        (Some(_), false) => format!("{api}, called by the Metro server; an API key is optional."),
        (None, true) => {
            format!("{api} with your own API key, on an endpoint your operator approved.")
        }
        (None, false) => format!(
            "{api} on a self-hosted endpoint your operator approved; an API key is optional."
        ),
    };
    Some(Entry {
        id: key,
        name,
        description,
        adapter: Adapter::Registered(id),
        execution: Execution::Server,
        capability: Capability::Chat,
        format: format.as_str(),
        api,
        requires_key,
        default_base_url,
        lists_models,
        media_inputs: false,
        modalities: &[],
        default_model: None,
    })
}

fn device(id: &str, name: &str, description: &str) -> Entry {
    device_for(id, name, description, Capability::Chat)
}

fn device_for(id: &str, name: &str, description: &str, capability: Capability) -> Entry {
    Entry {
        id: id.to_owned(),
        name: name.to_owned(),
        description: description.to_owned(),
        adapter: Adapter::Device,
        execution: Execution::Device,
        capability,
        format: "device",
        api: "metro-connector",
        requires_key: false,
        default_base_url: None,
        lists_models: true,
        media_inputs: false,
        modalities: &[],
        default_model: None,
    }
}

static ENTRIES: LazyLock<Vec<Entry>> = LazyLock::new(|| {
    let mut entries: Vec<Entry> = ProviderId::all().filter_map(registered).collect();
    entries.push(Entry {
        id: CUSTOM.to_owned(),
        name: "Custom OpenAI-compatible endpoint".to_owned(),
        description: "OpenAI Chat Completions on an endpoint your operator approved, such as a \
                      self-hosted or private gateway. An API key is optional; without one no \
                      Authorization header is sent."
            .to_owned(),
        adapter: Adapter::Custom,
        execution: Execution::Server,
        capability: Capability::Chat,
        format: "openai",
        api: "OpenAI Chat Completions",
        requires_key: false,
        default_base_url: None,
        lists_models: true,
        media_inputs: false,
        modalities: &[],
        default_model: None,
    });
    entries.push(device(
        DEVICE_OPENAI_OAUTH,
        "Sign in with ChatGPT (on your device)",
        "Runs in metro-connector on your own machine, signed in with your own ChatGPT account. \
         Sign in with ChatGPT is a preview for eligible locally run open-source integrations; \
         paid or hosted apps may need OpenAI's approval, and availability depends on your plan \
         and workspace. Metro relays requests and never receives your OpenAI credentials. To \
         use an OpenAI API key instead, add an OpenAI connection.",
    ));
    entries.push(device(
        DEVICE_CLAUDE_CODE,
        "Claude (local CLI)",
        "Runs through your own installed Claude Code on your machine, with your own Anthropic \
         Console API key kept on that device (not a Claude Pro or Max sign-in). Metro relays \
         requests and never receives the key.",
    ));
    entries.push(device(
        DEVICE_OPENAI_COMPATIBLE,
        "OpenAI-compatible endpoint (on your device)",
        "An OpenAI-compatible server reached from your own machine by metro-connector, such as a \
         local model server, with any key kept on that device. Metro never receives it.",
    ));
    entries.push(typed(
        SYSTEMONE,
        "TypeSafe System One",
        "TypeSafe's System One API (POST /v1/systemone) with your own TypeSafe API key, called by \
         the Metro server. Answers typed Noul, Choice and Score questions; it does not chat.",
        Adapter::SystemOne,
        Capability::SystemOne,
        true,
        Some(SYSTEMONE_BASE_URL),
    ));
    entries.push(typed(
        CUSTOM_SYSTEMONE,
        "System One on an approved endpoint",
        "The System One wire (POST /systemone, GET /models) on an endpoint your operator \
         approved, such as a self-hosted deployment. An API key is optional.",
        Adapter::SystemOne,
        Capability::SystemOne,
        false,
        None,
    ));
    entries.push(typed(
        OPENAI_EMBEDDINGS,
        "OpenAI embeddings",
        "OpenAI's embeddings API (POST /v1/embeddings) with your own OpenAI API key, called by \
         the Metro server. Used for semantic search, not for chat.",
        Adapter::Embeddings,
        Capability::Embeddings,
        true,
        Some(OPENAI_BASE_URL),
    ));
    entries.push(typed(
        CUSTOM_EMBEDDINGS,
        "OpenAI-compatible embeddings endpoint",
        "An OpenAI-compatible POST /embeddings endpoint your operator approved, such as a \
         self-hosted embedding server. An API key is optional.",
        Adapter::Embeddings,
        Capability::Embeddings,
        false,
        None,
    ));
    entries.push(device_for(
        DEVICE_SYSTEMONE,
        "System One (on your device)",
        "A System One endpoint reached from your own machine by metro-connector, with any key \
         kept on that device. Metro relays typed decisions and never receives the key.",
        Capability::SystemOne,
    ));
    entries.push(device_for(
        DEVICE_EMBEDDINGS,
        "Embeddings endpoint (on your device)",
        "An OpenAI-compatible embeddings endpoint reached from your own machine by \
         metro-connector, with any key kept on that device. Metro never receives it.",
        Capability::Embeddings,
    ));
    entries.push(Entry {
        id: EMBEDDINGGEMMA.to_owned(),
        name: "EmbeddingGemma 2 on an approved endpoint".to_owned(),
        description: "A self-hosted media embedding service your operator approved, running \
                      google/embeddinggemma-2 or another model it lists. Embeds TXT, Markdown, \
                      PDF, DOCX, PNG, JPEG, WAV, MP3, MP4 and WebM files for Documents & media \
                      (the service extracts or renders PDF and DOCX pages first), and text for \
                      message search. Whoever runs the endpoint sees what is \
                      sent. An API key is optional."
            .to_owned(),
        adapter: Adapter::Embeddings,
        execution: Execution::Server,
        capability: Capability::Embeddings,
        format: "metro-media-embeddings",
        api: "Metro media embeddings",
        requires_key: false,
        default_base_url: None,
        lists_models: true,
        media_inputs: true,
        modalities: MEDIA_MODALITIES,
        default_model: Some(EMBEDDINGGEMMA_MODEL),
    });
    entries.push(Entry {
        media_inputs: true,
        modalities: MEDIA_MODALITIES,
        default_model: Some(EMBEDDINGGEMMA_MODEL),
        ..device_for(
            DEVICE_EMBEDDINGGEMMA,
            "EmbeddingGemma 2 (on your device)",
            "A media embedding service reached from your own machine by metro-connector, running \
             google/embeddinggemma-2 or another model you approve there, with any key kept on \
             that device. Embeds TXT, Markdown, PDF, DOCX, PNG, JPEG, WAV, MP3, MP4 and WebM \
             files (the service extracts or renders PDF and DOCX pages first). You see the files and text sent to it, including other people's \
             when you share it.",
            Capability::Embeddings,
        )
    });
    entries
});

#[allow(clippy::too_many_arguments)]
fn typed(
    id: &str,
    name: &str,
    description: &str,
    adapter: Adapter,
    capability: Capability,
    requires_key: bool,
    default_base_url: Option<&str>,
) -> Entry {
    let (format, api) = match adapter {
        Adapter::SystemOne => ("systemone", "TypeSafe System One"),
        _ => ("openai-embeddings", "OpenAI Embeddings"),
    };
    Entry {
        id: id.to_owned(),
        name: name.to_owned(),
        description: description.to_owned(),
        adapter,
        execution: Execution::Server,
        capability,
        format,
        api,
        requires_key,
        default_base_url: default_base_url.map(str::to_owned),
        lists_models: true,
        media_inputs: false,
        modalities: &[],
        default_model: None,
    }
}

/// Every entry this build offers, before applying the server's allowlist.
pub fn entries() -> &'static [Entry] {
    &ENTRIES
}

pub fn find(id: &str) -> Option<&'static Entry> {
    entries().iter().find(|e| e.id == id)
}

/// Whether `id` names a device adapter.
pub fn is_device(id: &str) -> bool {
    find(id).is_some_and(Entry::is_device)
}

/// Whether an entry can be used on this server: a device entry always; a
/// server entry when it has a default HTTPS endpoint or the operator
/// approved at least one base URL.
pub fn usable(entry: &Entry, allowed: &[String]) -> bool {
    entry.is_device() || entry.default_base_url.is_some() || !allowed.is_empty()
}

/// The catalog as users see it on this server.
pub fn view(allowed: &[String]) -> Vec<EntryView> {
    entries()
        .iter()
        .filter(|e| usable(e, allowed))
        .map(|e| EntryView {
            id: e.id.clone(),
            name: e.name.clone(),
            description: e.description.clone(),
            execution: e.execution,
            capability: e.capability,
            format: e.format,
            api: e.api,
            requires_key: e.requires_key,
            allow_custom_url: !e.is_device() && !allowed.is_empty(),
            default_base_url: e.default_base_url.clone(),
            lists_models: e.lists_models,
            media_inputs: e.media_inputs,
            modalities: e.modalities,
            default_model: e.default_model,
        })
        .collect()
}
