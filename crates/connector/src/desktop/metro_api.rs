//! Calls the desktop bridge makes to the signed-in person's Metro server
//! with their session token: who they are, which connections are theirs, and
//! issuing or revoking a connection's device token. No redirects, bounded
//! bodies, fixed error text.

use std::time::Duration;

use serde_json::Value;

use crate::Secret;
use crate::http::read_capped;
use crate::profile::validate_device_token;

const MAX_BODY: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// 401/403: the session token is not valid (or no longer).
    Rejected,
    /// 404 on a connection: not this person's connection.
    NotFound,
    Unreachable,
    /// Anything else, with a short safe description.
    Server(String),
}

pub struct MetroApi {
    http: reqwest::Client,
    base: String,
    token: Secret,
}

impl MetroApi {
    /// `base` must already be validated (`netpolicy::validate_server_url`).
    pub fn new(base: &str, token: Secret) -> Result<Self, ApiError> {
        Ok(MetroApi {
            http: crate::http::client_for_server(TIMEOUT, base)
                .map_err(|_| ApiError::Server("cannot start HTTP".into()))?,
            base: base.trim_end_matches('/').to_string(),
            token,
        })
    }

    async fn call(&self, method: reqwest::Method, path: &str) -> Result<(u16, Value), ApiError> {
        let mut rb = self
            .http
            .request(method.clone(), format!("{}{}", self.base, path))
            .bearer_auth(self.token.expose())
            .header("Accept", "application/json");
        if method == reqwest::Method::POST {
            rb = rb.header("Content-Type", "application/json").body("{}");
        }
        let resp = rb.send().await.map_err(|_| ApiError::Unreachable)?;
        let status = resp.status().as_u16();
        match status {
            200..=299 => {}
            300..=399 => {
                return Err(ApiError::Server(
                    "the Metro server answered with a redirect; check its URL".into(),
                ));
            }
            401 | 403 => return Err(ApiError::Rejected),
            404 => return Err(ApiError::NotFound),
            s => {
                return Err(ApiError::Server(format!(
                    "the Metro server answered HTTP {s}"
                )));
            }
        }
        let body = read_capped(resp, MAX_BODY)
            .await
            .map_err(|_| ApiError::Server("the Metro server's answer is too large".into()))?;
        if body.is_empty() {
            return Ok((status, Value::Null));
        }
        let v = serde_json::from_slice(&body)
            .map_err(|_| ApiError::Server("the Metro server's answer is not JSON".into()))?;
        Ok((status, v))
    }

    /// The signed-in account's stable id (`GET /v1/me`).
    pub async fn me(&self) -> Result<String, ApiError> {
        let (_, v) = self.call(reqwest::Method::GET, "/v1/me").await?;
        match v.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control) => {
                Ok(id.to_string())
            }
            _ => Err(ApiError::Server(
                "GET /v1/me did not return an account id".into(),
            )),
        }
    }

    /// The person's own connections: (id, provider_id).
    pub async fn connections(&self) -> Result<Vec<(String, String)>, ApiError> {
        let (_, v) = self.call(reqwest::Method::GET, "/v1/providers").await?;
        let list = v
            .get("connections")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ApiError::Server("GET /v1/providers did not return connections".into())
            })?;
        Ok(list
            .iter()
            .filter_map(|c| {
                Some((
                    c.get("id")?.as_str()?.to_string(),
                    c.get("provider_id")?.as_str()?.to_string(),
                ))
            })
            .collect())
    }

    /// Issue (or rotate) a connection's device token. The token goes only
    /// to this device's profile, never back to the UI.
    pub async fn issue_device_token(&self, id: &str) -> Result<Secret, ApiError> {
        let (_, v) = self
            .call(
                reqwest::Method::POST,
                &format!("/v1/providers/{id}/device-token"),
            )
            .await?;
        let t = Secret::new(v.get("token").and_then(Value::as_str).unwrap_or_default());
        validate_device_token(&t).map_err(|_| {
            ApiError::Server("the Metro server returned no valid device token".into())
        })?;
        Ok(t)
    }

    pub async fn revoke_device_token(&self, id: &str) -> Result<(), ApiError> {
        self.call(
            reqwest::Method::DELETE,
            &format!("/v1/providers/{id}/device-token"),
        )
        .await
        .map(|_| ())
    }
}
