//! Metro device connector.
//!
//! A person runs `metro-connector` on their own machine to serve their own
//! model connection to Metro. The connector:
//!
//! - holds an opaque `metro_pc_` device credential that Metro issued for one
//!   personal provider connection;
//! - keeps the person's vendor sign-in, CLI login or API key on this machine,
//!   in a per-connection profile directory (mode 700, files mode 600), and
//!   never sends any of it to Metro;
//! - only makes outbound requests: it claims jobs from Metro
//!   (`POST /v1/device/claim`), runs them against the locally configured
//!   provider and posts the result (`POST /v1/device/jobs/{id}/result`);
//! - accepts only `models` plus the one operation its profile's kind serves:
//!   `complete` for chat kinds, `decide` (System One) for `device-systemone`,
//!   `embed` for `device-embeddings`; and enforces the locally approved model
//!   allowlist. Chat requests are rebuilt from an allowlist of Chat
//!   Completions fields (text messages and function tools only), and typed
//!   requests are checked in full by `metro-model-protocol` (no images, no
//!   extra fields), so a job can't choose a URL, path, command, credential,
//!   hosted tool or remote resource on this machine.
//!
//! Vendor adapters (`openai_oauth`, `claude_code`) implement the same small
//! interface: `login`, `logout`, `models` and `complete`, each taking the
//! adapter's private directory inside the profile. `openai_compat` is the
//! bring-your-own-key adapter for any OpenAI-compatible endpoint; the
//! System One and embeddings kinds use the same saved base URL and bound key
//! with the shared `metro-model-protocol` clients.

pub mod cli;
pub mod desktop;
pub mod desktop_local;
pub mod desktop_tools;
pub mod fsutil;
pub mod http;
pub mod metro;
pub mod netpolicy;
pub mod openai_compat;
pub mod opener;
pub mod policy;
pub mod profile;
pub mod provider;
pub mod run;
pub mod secret;

// Vendor adapters. Owned by their own authors; this crate only registers them
// and calls the shared interface (see `provider.rs`).
pub mod claude_code;
pub mod openai_oauth;

pub use secret::{SafeError, Secret};

/// User agent for every outbound request.
pub const USER_AGENT: &str = concat!("metro-connector/", env!("CARGO_PKG_VERSION"));
