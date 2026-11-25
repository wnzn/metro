use std::{fmt, net::IpAddr, time::Duration};

use reqwest::{
    Client, Method, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Host;

/// Check the `--server` origin: `https://host[:port]`, or plain
/// `http://host[:port]` to any host (see `plain_http_warning`). IP
/// addresses must be written in their usual form, so `127.1` or
/// `0x7f000001` can't pass for something else.
pub fn origin(input: &str) -> Result<Url, &'static str> {
    const INVALID: &str =
        "Use an HTTPS or HTTP origin, without credentials, a path, query, or fragment.";
    let (scheme, rest) = input.split_once("://").ok_or(INVALID)?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty()
        || authority.contains(['/', '\\', '?', '#', '@', '%'])
        || authority
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(INVALID);
    }
    let url = Url::parse(input).map_err(|_| INVALID)?;
    match (scheme, url.host()) {
        ("https", Some(_)) => Ok(url),
        ("http", Some(Host::Ipv4(ip))) => {
            // WHATWG accepts spellings such as 127.1 and 0x7f000001.
            let literal = authority.split(':').next().unwrap_or_default();
            (!ip.is_unspecified() && literal.parse() == Ok(ip))
                .then_some(url)
                .ok_or(INVALID)
        }
        ("http", Some(Host::Ipv6(ip))) => {
            let literal = authority.strip_prefix('[').and_then(|s| s.split_once(']'));
            (!ip.is_unspecified()
                && literal.and_then(|(s, _)| s.parse().ok()) == Some(IpAddr::V6(ip)))
            .then_some(url)
            .ok_or(INVALID)
        }
        ("http", Some(Host::Domain(_))) => Ok(url),
        _ => Err(INVALID),
    }
}

/// A warning to show before the token is sent when `origin` is plain HTTP
/// to a host that is neither loopback nor Tailscale; `None` otherwise.
pub fn plain_http_warning(origin: &Url) -> Option<String> {
    metro_url_policy::plain_http_warning(origin.scheme(), origin.host_str()?, "Metro token")
}

#[derive(Clone)]
pub struct Api {
    root: Url,
    client: Client,
    auth: HeaderValue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Network,
    Http(u16),
    Invalid,
    Reselect,
    AccessCheck,
    CredentialInDraft,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network => f.write_str("Connection failed or timed out."),
            Self::Http(401) => f.write_str("Not signed in (HTTP 401). Check your Metro token."),
            Self::Http(403 | 404) => f.write_str("Access denied or no longer available."),
            Self::Http(code) => write!(f, "Metro returned HTTP {code}."),
            Self::Invalid => f.write_str("Metro returned an invalid or oversized response."),
            Self::Reselect => {
                f.write_str("Model or thinking level unavailable. Use F4 to choose again.")
            }
            Self::AccessCheck => {
                f.write_str("Could not verify recipients or model availability. Try F5.")
            }
            Self::CredentialInDraft => f.write_str("Remove the sign-in token from the draft."),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    Chat,
    Room { space: String, room: String },
}

impl Place {
    pub fn space(&self) -> Option<&str> {
        match self {
            Self::Chat => None,
            Self::Room { space, .. } => Some(space),
        }
    }

    fn endpoint<'a>(&'a self, leaf: &'a str) -> Vec<&'a str> {
        match self {
            Self::Chat => vec!["chat", leaf],
            Self::Room { space, room } => vec!["spaces", space, "rooms", room, leaf],
        }
    }
}

#[derive(Deserialize)]
pub struct Identity {
    pub id: String,
}

#[derive(Clone, Deserialize)]
pub struct Named {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Deserialize)]
pub struct Entry {
    pub id: i64,
    pub kind: String,
    pub actor: String,
    pub actor_id: Option<String>,
    pub text: String,
    pub mentions: Vec<Recipient>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Recipient {
    Agent,
    User {
        id: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Deserialize)]
pub struct CatalogRow {
    pub connection_id: String,
    pub name: String,
    pub model: Option<String>,
    pub provider_id: String,
    pub capability: String,
    pub owned: bool,
    pub visibility: String,
    pub runs_on: String,
    pub ready: Option<bool>,
    pub chat_models: Option<Vec<Model>>,
}

#[derive(Clone, Deserialize)]
pub struct Model {
    pub id: String,
    pub reasoning_efforts: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Choice {
    pub provider_connection_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

#[derive(Serialize)]
pub struct Post<'a> {
    pub text: &'a str,
    pub invoke_agent: bool,
    #[serde(flatten)]
    pub choice: Option<&'a Choice>,
}

impl Api {
    pub fn new(root: Url, token: &str) -> Result<Self, &'static str> {
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("Invalid Metro token.");
        }
        let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| "Invalid Metro token.")?;
        auth.set_sensitive(true);
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(6))
            .build()
            .map_err(|_| "Could not initialize the HTTP client.")?;
        Ok(Self { root, client, auth })
    }

    fn url(&self, path: &[&str], query: &[(&str, &str)]) -> Result<Url, Failure> {
        let mut url = self.root.clone();
        if path
            .iter()
            .any(|s| s.is_empty() || matches!(*s, "." | ".."))
        {
            return Err(Failure::Invalid);
        }
        url.path_segments_mut()
            .map_err(|_| Failure::Invalid)?
            .clear()
            .push("v1")
            .extend(path);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    async fn request(
        &self,
        method: Method,
        path: &[&str],
        query: &[(&str, &str)],
        post: Option<&Post<'_>>,
    ) -> Result<reqwest::Response, Failure> {
        let mut request = self
            .client
            .request(method, self.url(path, query)?)
            .header(AUTHORIZATION, self.auth.clone());
        if let Some(body) = post {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|_| Failure::Network)?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(Failure::Http(response.status().as_u16()))
        }
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &[&str],
        query: &[(&str, &str)],
    ) -> Result<T, Failure> {
        const MAX_BODY: usize = 16 * 1024 * 1024;
        let mut response = self.request(Method::GET, path, query, None).await?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Network)? {
            if body.len() + chunk.len() > MAX_BODY {
                return Err(Failure::Invalid);
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| Failure::Invalid)
    }

    pub async fn me(&self) -> Result<Identity, Failure> {
        self.get(&["me"], &[]).await
    }

    pub async fn spaces(&self) -> Result<Vec<Named>, Failure> {
        #[derive(Deserialize)]
        struct Response {
            spaces: Vec<Named>,
        }
        Ok(self.get::<Response>(&["spaces"], &[]).await?.spaces)
    }

    pub async fn rooms(&self, space: &str) -> Result<Vec<Named>, Failure> {
        #[derive(Deserialize)]
        struct Response {
            rooms: Vec<Named>,
        }
        Ok(self
            .get::<Response>(&["spaces", space, "rooms"], &[])
            .await?
            .rooms)
    }

    pub async fn members(&self, space: &str) -> Result<Vec<String>, Failure> {
        #[derive(Deserialize)]
        struct Member {
            user_id: String,
        }
        #[derive(Deserialize)]
        struct Response {
            members: Vec<Member>,
        }
        let response: Response = self.get(&["spaces", space, "members"], &[]).await?;
        Ok(response.members.into_iter().map(|m| m.user_id).collect())
    }

    pub async fn events(&self, place: &Place, after: i64) -> Result<Vec<Entry>, Failure> {
        #[derive(Deserialize)]
        struct Response {
            events: Vec<Entry>,
        }
        Ok(self
            .get::<Response>(&place.endpoint("events"), &[("after", &after.to_string())])
            .await?
            .events)
    }

    pub async fn catalog(&self, place: &Place) -> Result<Vec<CatalogRow>, Failure> {
        #[derive(Deserialize)]
        struct Response {
            models: Vec<CatalogRow>,
        }
        let scope = place.space().map(|id| [("space_id", id)]);
        Ok(self
            .get::<Response>(
                &["models", "available"],
                scope.as_ref().map_or(&[], |q| &q[..]),
            )
            .await?
            .models)
    }

    pub async fn providers(&self) -> Result<Vec<Named>, Failure> {
        #[derive(Deserialize)]
        struct Response {
            providers: Vec<Named>,
        }
        Ok(self
            .get::<Response>(&["provider-catalog"], &[])
            .await?
            .providers)
    }

    pub async fn post(&self, place: &Place, body: &Post<'_>) -> Result<(), Failure> {
        let token = self
            .auth
            .to_str()
            .ok()
            .and_then(|header| header.strip_prefix("Bearer "))
            .ok_or(Failure::Invalid)?;
        if body.text.contains(token) {
            return Err(Failure::CredentialInDraft);
        }
        self.request(Method::POST, &place.endpoint("messages"), &[], Some(body))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_credential_safe_origins() {
        for good in [
            "https://example.org",
            "https://example.org:8443/",
            "http://127.0.0.1:8080",
            "http://127.7.8.9",
            "http://[::1]:8080",
            "http://localhost",
            "http://[::ffff:127.0.0.1]",
            "http://100.101.102.103:8787",
            "http://[fd7a:115c:a1e0::1]:8787",
            "http://metro.tail1234.ts.net",
            "http://192.168.1.5:8787",
            "http://metro.example.org/",
        ] {
            assert!(origin(good).is_ok(), "{good}");
        }
        for bad in [
            "localhost",
            "http://100.64.0.1.",
            "http://127.0.0.1.",
            "http://127.1",
            "http://0x7f000001",
            "http://127.000.0.1",
            "http://192.168.1.005",
            "http://0.0.0.0",
            "http://[::]",
            "http://host/api",
            "https://host/api",
            "https://host//",
            "https://host/.",
            "https://host?",
            "https://host/#",
            "https://@host",
            "https://user:pass@host",
            "http://user:pass@192.168.1.5",
            "https://host\\other",
            "https://host\n",
            "https:///host",
            "https://%68ost",
            "file:///",
        ] {
            assert!(origin(bad).is_err(), "{bad}");
        }
        for quiet in [
            "https://192.168.1.5",
            "http://127.0.0.1:8080",
            "http://localhost",
            "http://[::1]:8080",
            "http://100.64.0.9",
            "http://metro.tail1234.ts.net",
        ] {
            assert_eq!(plain_http_warning(&origin(quiet).unwrap()), None, "{quiet}");
        }
        for loud in [
            "http://192.168.1.5:8787",
            "http://metro.example.org",
            "http://100.128.0.1",
            "http://localhost.",
        ] {
            let w = plain_http_warning(&origin(loud).unwrap()).unwrap();
            assert!(
                w.contains("isn't encrypted") && w.contains("Metro token"),
                "{w}"
            );
        }
        let api = Api::new(origin("https://example.org").unwrap(), "test-token").unwrap();
        let url = api
            .url(
                &["spaces", "//other?secret", "rooms"],
                &[("space_id", "a&b")],
            )
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://example.org/v1/spaces/%2F%2Fother%3Fsecret/rooms?space_id=a%26b"
        );
        assert!(api.url(&["spaces", ".."], &[]).is_err());
    }
}
