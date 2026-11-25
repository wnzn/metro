//! On-device connections for the Metro desktop app, linked into the
//! `metro-desktop` launcher (so nobody installs or runs `metro-connector`).
//!
//! The launcher starts the packaged UI with a loopback [`bridge`] URL and
//! bearer in its environment; the UI drives everything through it:
//!
//! | Method + path | Body | Answer |
//! |---|---|---|
//! | `GET /v1/status` | | bridge, version, instance, kinds, session (including generation), local, connections |
//! | `GET /v1/monitoring/events` | | `{events: [...]}`; recent device-only operational errors |
//! | `POST /v1/session` | `{server_url, session_token}` | status; verifies `GET /v1/me`, starts confirmed workers |
//! | `DELETE /v1/session` | | status; stops every worker and sign-in |
//! | `POST /v1/connections/{id}/pair` | `{kind, base_url?, models?}` | connection; mints the device token itself |
//! | `POST /v1/connections/{id}/rotate` | `{}` | connection |
//! | `POST /v1/connections/{id}/credentials` | `{api_key}` / `{no_api_key:true}` / `{open_browser?}` | connection |
//! | `DELETE /v1/connections/{id}/credentials` | | connection |
//! | `GET /v1/connections/{id}/available-models` | | `{models}` |
//! | `PUT /v1/connections/{id}/models` | `{models}` | connection |
//! | `PUT /v1/connections/{id}/endpoint` | `{base_url}` | connection |
//! | `POST /v1/connections/{id}/forget` | `{revoke}` | `{forgotten, revoked}` |
//! | `GET /v1/web-search` | | web search on this computer: switch, notice, device, providers |
//! | `PUT /v1/web-search` | `{enabled?, notice_acknowledged?, providers?: [{id, enabled}]}` | the same |
//! | `PUT /v1/web-search/providers/{id}/key` | `{api_key}` | provider row (`key_saved`, never the key) |
//! | `DELETE /v1/web-search/providers/{id}/key` | | provider row (back to free mode) |
//! | `POST /v1/web-search/keys/reset` | `{}` | the web search state; every saved key deleted |
//! | `POST /v1/web-search/providers/{id}/test` | `{session_generation}` | provider row with `test.outcome`; cancelled if that binding ends |
//! | `POST /v1/web-search/providers/{id}/clear-cooldown` | `{}` | provider row |
//! | `POST /v1/local-workspace/open` | `{}` | `{server_url, token}` |
//! | `GET/POST /v1/local-tools…` | | the desktop tools lane |
//!
//! Errors are `{error: {code, message}}`.

pub mod bridge;
pub mod core;
pub mod local;
pub mod metro_api;
pub mod monitoring;
pub mod store;
mod tools_glue;
pub mod update;
pub mod web;
