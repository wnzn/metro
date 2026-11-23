//! Sign-in, refresh, sign-out, the model catalog, and inference.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::diagnostic::{Detail, Phase, mark};
use super::http::{self, Category, Failure, MAX_JSON, form, pct, tagged};
use super::loopback::Loopback;
use super::oidc::{self, Discovery};
use super::sse;
use super::store::{Account, Profile, Tokens, now};
use super::translate;
use super::{
    AGENT_NAME, DYNAMIC_CLIENT_ID, Endpoints, LoginOptions, ModelInfo, PLAN_SCOPE, SCOPES,
    SignInStage, Status, USAGE_URL,
};
use crate::SafeError;

/// Refresh this many seconds before `exp`.
const REFRESH_SKEW: u64 = 120;

#[derive(Clone, Debug, Serialize)]
pub struct LogoutOutcome {
    /// Local tokens existed and are now cleared.
    pub signed_out: bool,
    /// OpenAI answered the revocation with HTTP 200.
    pub remote_revoked: bool,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    earliest_refresh_at: Option<Value>,
}

/// Opens the private start link without writing it to the terminal.
pub(crate) fn open_system_browser(url: &str) -> Result<()> {
    match crate::opener::open_start_link(url) {
        Ok(()) => eprintln!("Continue with ChatGPT in your browser."),
        Err(why) => {
            eprintln!("{why} Press Ctrl+C, then run the sign-in again after fixing the browser.");
        }
    }
    Ok(())
}

/// Marks `e` as a failure at `stage`. Unless it already carries a
/// [`SafeError`] of its own, `safe` is what a person is told; the rest of the
/// chain (which may quote a provider or a path) stays out of the UI.
fn at(stage: SignInStage, safe: &'static str) -> impl FnOnce(anyhow::Error) -> anyhow::Error {
    move |e| {
        let has_safe = e.downcast_ref::<SafeError>().is_some()
            || e.chain().any(|c| c.downcast_ref::<SafeError>().is_some());
        let e = if has_safe {
            e
        } else {
            e.context(SafeError::new(safe))
        };
        e.context(stage)
    }
}

/// A Metro-authored message a person can act on, as the error itself.
fn safe(stage: SignInStage, msg: impl AsRef<str>) -> anyhow::Error {
    anyhow::Error::new(SafeError::new(msg)).context(stage)
}

const STORAGE_FAILED: &str = "Metro couldn't prepare this connection's private sign-in storage on this computer, so no browser was opened. Try again; if it keeps failing, use Forget this computer and set the connection up again.";
const DISCOVERY_FAILED: &str = "Metro couldn't reach ChatGPT's sign-in service from this computer, so no browser was opened. Check the internet connection, proxy or firewall, then try again.";
const CALLBACK_FAILED: &str = "Metro couldn't listen on 127.0.0.1 for the sign-in to come back, so no browser was opened. Security software may be blocking local connections; allow Metro, then try again.";
const EXCHANGE_FAILED: &str = "Metro received the browser sign-in but couldn't complete it with ChatGPT. Start the sign-in again.";

pub(crate) async fn login(dir: &Path, opts: LoginOptions) -> Result<()> {
    let ep = opts.endpoints.clone();
    http::check_endpoints(&ep).map_err(at(SignInStage::Discovery, DISCOVERY_FAILED))?;

    // Before any browser: the private store, the host ID, the issuer, the port.
    let (profile, host, account, retained) = async {
        let profile = Profile::open(dir, true)?.expect("created");
        let host = {
            let lock = profile.lock().await?;
            profile.host_or_create(&lock)?
        };
        let account = profile.account()?;
        let retained = profile.tokens()?;
        anyhow::Ok((profile, host, account, retained))
    }
    .await
    .map_err(at(SignInStage::Storage, STORAGE_FAILED))?;
    if let Some(a) = &account
        && a.endpoints != ep
    {
        return Err(safe(
            SignInStage::Storage,
            "This connection's sign-in on this computer belongs to a different ChatGPT service. Use Forget this computer, then set it up again.",
        ));
    }
    let client = http::client_for(&ep).map_err(at(SignInStage::Discovery, DISCOVERY_FAILED))?;
    let disc = oidc::discover(&client, &ep.issuer)
        .await
        .map_err(at(SignInStage::Discovery, DISCOVERY_FAILED))?;

    let lb = Loopback::bind(opts.preferred_port)
        .await
        .map_err(at(SignInStage::Callback, CALLBACK_FAILED))?;
    let redirect_uri = lb.redirect_uri();
    let state = oidc::random_token();
    let nonce = oidc::random_token();
    let pkce = oidc::pkce();
    let start_key = oidc::random_token();

    let saved_client = account.as_ref().map(|a| a.client_id.clone());
    let mut q: Vec<(&str, String)> = vec![
        (
            "client_id",
            saved_client
                .clone()
                .unwrap_or_else(|| DYNAMIC_CLIENT_ID.into()),
        ),
        ("ext_agent_host_id", host.ext_agent_host_id.clone()),
        ("response_type", "code".into()),
        ("redirect_uri", redirect_uri.clone()),
        ("scope", SCOPES.into()),
        ("resource", ep.resource.clone()),
        ("state", state.clone()),
        ("nonce", nonce.clone()),
        ("code_challenge_method", "S256".into()),
        ("code_challenge", pkce.challenge.clone()),
    ];
    match &account {
        None => q.push(("agent_name_hint", AGENT_NAME.into())),
        Some(a) => {
            if let Some(t) = &retained {
                q.push(("id_token_hint", t.id_token.clone()));
            }
            if let Some(e) = &a.email {
                q.push(("login_hint", e.clone()));
            }
            // A sign-in that lacks plan permission asks again with the full
            // scope set. Ordinary sign-ins never force consent.
            if a.subject.is_some() && !a.plan_usage_enabled {
                q.push(("prompt", "consent".into()));
            }
        }
    }
    let authorize_url = format!(
        "{}?{}",
        disc.authorization_endpoint,
        q.iter()
            .map(|(k, v)| format!("{k}={}", pct(v)))
            .collect::<Vec<_>>()
            .join("&")
    );

    // The start page keeps working (and can be opened again) whatever the
    // opener says: an opener failure is the caller's to report, never fatal.
    let _ = (opts.open_browser)(&lb.start_url(&start_key));
    let minutes = opts.timeout.as_secs().div_ceil(60).max(1);
    let cb = lb
        .wait(&start_key, &authorize_url, &state, opts.timeout)
        .await
        .map_err(|e| {
            e.context(SafeError::new(format!(
                "The browser sign-in wasn't finished within {minutes} minute{}. Start it again.",
                if minutes == 1 { "" } else { "s" }
            )))
            .context(SignInStage::Waiting)
        })?;
    drop(authorize_url);

    if let Some(err) = cb.error.as_deref() {
        if err == "access_denied" {
            return Err(safe(
                SignInStage::Response,
                "Sign in with ChatGPT was declined, so ChatGPT plan usage is not enabled for Metro. \
                 Sign in again to approve it, or use another model connection.",
            ));
        }
        return Err(safe(
            SignInStage::Response,
            "ChatGPT's sign-in page returned an error. Start the sign-in again.",
        ));
    }
    let code = cb.code.filter(|c| !c.is_empty()).ok_or_else(|| {
        safe(
            SignInStage::Response,
            "The browser came back without a sign-in code. Start the sign-in again.",
        )
    })?;
    finish(
        &profile,
        &ep,
        &client,
        &disc,
        saved_client,
        cb.client_id,
        code,
        &pkce.verifier,
        &redirect_uri,
        &nonce,
    )
    .await
    .map_err(at(SignInStage::Exchange, EXCHANGE_FAILED))
}

/// After the browser: the client ID, the code exchange, the ID token, and
/// saving the account and tokens.
#[allow(clippy::too_many_arguments)]
async fn finish(
    profile: &Profile,
    ep: &Endpoints,
    client: &reqwest::Client,
    disc: &Discovery,
    saved_client: Option<String>,
    cb_client_id: Option<String>,
    code: String,
    verifier: &str,
    redirect_uri: &str,
    nonce: &str,
) -> Result<()> {
    // Work out the client ID for the exchange.
    let client_id = match (&saved_client, cb_client_id.as_deref()) {
        (Some(saved), Some(got)) if got != saved => {
            bail!(
                "sign-in callback named a different client than this connection's registration; nothing was changed"
            )
        }
        (Some(saved), _) => saved.clone(),
        (None, Some(got)) => {
            if got == DYNAMIC_CLIENT_ID || !valid_client_id(got) {
                bail!("registration incomplete: the callback carried no valid issued client_id");
            }
            // Keep the issued client right away, so a failed exchange retries
            // with it instead of registering again.
            let lock = profile.lock().await?;
            if profile.account()?.is_some() {
                bail!(
                    "this connection profile was registered by another sign-in in the meantime; try again"
                );
            }
            profile.write_account(
                &lock,
                &Account {
                    endpoints: ep.clone(),
                    client_id: got.to_owned(),
                    subject: None,
                    email: None,
                    plan_usage_enabled: false,
                },
            )?;
            got.to_owned()
        }
        (None, None) => bail!("registration incomplete: the callback carried no issued client_id"),
    };

    let body = form(&[
        ("grant_type", "authorization_code"),
        ("client_id", &client_id),
        ("code", &code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
        ("resource", &ep.resource),
    ]);
    let tr = token_request(
        client,
        &disc.token_endpoint,
        body,
        "authorization code exchange",
    )
    .await
    .map_err(|e| {
        if e.to_string().contains("invalid_grant") {
            anyhow!("{e}. Start a new sign-in; the code cannot be reused")
        } else {
            e
        }
    })?;

    let id_token = tr
        .id_token
        .clone()
        .ok_or_else(|| anyhow!("token response carried no ID token"))?;
    let keys = oidc::fetch_jwks(client, &disc.jwks_uri).await?;
    let claims = oidc::verify_id_token(&id_token, &keys, &disc.issuer, &client_id, Some(nonce))?;
    let access_scope =
        oidc::check_access_token(&tr.access_token, &disc.issuer, &ep.resource, &client_id)?;
    let tokens = build_tokens(tr, id_token, access_scope, None)?;
    let plan = tokens.scopes.iter().any(|s| s == PLAN_SCOPE);

    let lock = profile.lock().await?;
    let Some(current) = profile.account()?.filter(|a| a.client_id == client_id) else {
        bail!("the connection's registration changed during sign-in; try again");
    };
    if let Some(sub) = &current.subject
        && *sub != claims.sub
    {
        return Err(anyhow::Error::new(SafeError::new(
            "You signed in to a different ChatGPT account than this connection belongs to, so nothing was changed. Add a new connection for the other account.",
        )));
    }
    profile.write_account(
        &lock,
        &Account {
            endpoints: ep.clone(),
            client_id: client_id.clone(),
            subject: Some(claims.sub),
            email: claims.email.or(current.email),
            plan_usage_enabled: plan,
        },
    )?;
    profile.write_tokens(&lock, &tokens)?;
    drop(lock);

    if !plan {
        return Err(anyhow!(
            "signed in, but ChatGPT plan usage was not granted (no {PLAN_SCOPE} scope)"
        )
        .context(SafeError::new(
            "Signed in, but ChatGPT plan usage wasn't granted, so Metro won't send requests with this connection. Sign in again and approve it.",
        )));
    }
    Ok(())
}

fn valid_client_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn build_tokens(
    tr: TokenResponse,
    id_token: String,
    access_scope: Option<String>,
    previous: Option<&Tokens>,
) -> Result<Tokens> {
    let token_type = tr.token_type.clone().unwrap_or_else(|| "Bearer".into());
    if !token_type.eq_ignore_ascii_case("bearer") {
        bail!("unexpected token type from the token endpoint");
    }
    let t = now();
    let scope = tr.scope.or(access_scope);
    let scopes: Vec<String> = match (scope, previous) {
        (Some(s), _) => {
            let mut v: Vec<String> = s.split_whitespace().map(str::to_owned).collect();
            v.sort();
            v.dedup();
            v
        }
        // A refresh that omits `scope` keeps the original grant.
        (None, Some(p)) => p.scopes.clone(),
        (None, None) => Vec::new(),
    };
    let refresh_token = match (tr.refresh_token, previous) {
        (Some(r), _) if !r.is_empty() => r,
        (_, Some(p)) => p.refresh_token.clone(),
        _ => bail!(
            "token response carried no refresh token (offline_access not granted); sign in again"
        ),
    };
    let earliest = match tr.earliest_refresh_at {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    };
    Ok(Tokens {
        id_token,
        access_token: tr.access_token,
        refresh_token,
        token_type: "Bearer".into(),
        expires_at: t + tr.expires_in.unwrap_or(3600).min(86_400),
        earliest_refresh_at: earliest,
        scopes,
        saved_at: t,
    })
}

async fn token_request(
    client: &reqwest::Client,
    endpoint: &str,
    body: String,
    what: &str,
) -> Result<TokenResponse> {
    let resp = client
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(http::net)?;
    let status = resp.status();
    if !status.is_success() {
        let bytes = http::read_bounded(resp, MAX_JSON).await.unwrap_or_default();
        let f = Failure::parse(status.as_u16(), &bytes);
        let description = f.describe(what);
        let detail = f.detail(Phase::Http);
        return Err(TokenError(f)).context(description).context(detail);
    }
    let bytes = http::read_bounded(resp, MAX_JSON).await?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("{what}: token response is malformed"))
}

/// Carries the parsed failure so refresh can branch on its code.
struct TokenError(Failure);
impl std::fmt::Debug for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "token endpoint HTTP {}", self.0.status)
    }
}
impl std::error::Error for TokenError {}

const UNUSABLE_REFRESH: &[&str] = &[
    "invalid_grant",
    "invalid_refresh_token",
    "token_expired",
    "refresh_token_expired",
    "refresh_token_invalidated",
    "refresh_token_reused",
];

/// A usable access token, refreshed under the profile lock when it's near
/// expiry. Every failure here is the local session's, whatever HTTP status
/// a refresh met.
async fn access(dir: &Path, ep: &Endpoints) -> Result<(Account, Tokens)> {
    access_inner(dir, ep).await.map_err(|e| {
        let inner = e.downcast_ref::<Detail>().copied();
        let d = Detail {
            phase: Phase::Session,
            ..inner.unwrap_or(Detail::phase(Phase::Session))
        };
        e.context(d)
    })
}

async fn access_inner(dir: &Path, ep: &Endpoints) -> Result<(Account, Tokens)> {
    http::check_endpoints(ep)?;
    let profile = Profile::open(dir, false)
        .context(Category::Storage)?
        .ok_or_else(not_signed_in)?;
    let account = profile
        .account()
        .context(Category::Storage)?
        .ok_or_else(not_signed_in)?;
    if account.endpoints != *ep {
        return Err(
            anyhow!("this connection profile is bound to a different OpenAI issuer/API")
                .context(Category::Registration),
        );
    }
    let tokens = profile
        .tokens()
        .context(Category::Storage)?
        .ok_or_else(not_signed_in)?;
    check_plan(&tokens)?;
    if fresh(&tokens) {
        return Ok((account, tokens));
    }
    let lock = profile.lock().await.context(Category::Storage)?;
    // Another process may have refreshed while this one waited for the lock.
    let tokens = profile
        .tokens()
        .context(Category::Storage)?
        .ok_or_else(not_signed_in)?;
    check_plan(&tokens)?;
    if fresh(&tokens) {
        return Ok((account, tokens));
    }
    let client = http::client_for(ep)?;
    let disc = oidc::discover(&client, &ep.issuer)
        .await
        .map_err(refresh_error)?;
    let body = form(&[
        ("grant_type", "refresh_token"),
        ("client_id", &account.client_id),
        ("refresh_token", &tokens.refresh_token),
        ("resource", &ep.resource),
    ]);
    let tr = match token_request(&client, &disc.token_endpoint, body, "token refresh").await {
        Ok(t) => t,
        Err(e) => {
            if let Some(TokenError(f)) = e.downcast_ref::<TokenError>() {
                let code = f.code.as_deref().unwrap_or("");
                if UNUSABLE_REFRESH.contains(&code) {
                    profile.clear_tokens(&lock).context(Category::Storage)?;
                    return Err(e.context(Category::SignIn));
                }
                // A transient failure leaves the credentials as they are.
                let category = match f.category() {
                    Some(
                        c @ (Category::Registration
                        | Category::SignIn
                        | Category::Unavailable
                        | Category::PlanNotAvailable
                        | Category::UsageLimit),
                    ) => c,
                    _ => Category::Refresh,
                };
                return Err(e.context(category));
            }
            return Err(refresh_error(e));
        }
    };
    let id_token = match &tr.id_token {
        Some(t) => {
            let keys = oidc::fetch_jwks(&client, &disc.jwks_uri)
                .await
                .map_err(refresh_error)?;
            let c = oidc::verify_id_token(t, &keys, &disc.issuer, &account.client_id, None)
                .context(Category::Refresh)?;
            if account.subject.as_deref() != Some(c.sub.as_str()) {
                return Err(anyhow!(
                    "refresh returned an identity for a different account; credentials left unchanged"
                ).context(Category::Refresh));
            }
            t.clone()
        }
        None => tokens.id_token.clone(),
    };
    let access_scope = oidc::check_access_token(
        &tr.access_token,
        &disc.issuer,
        &ep.resource,
        &account.client_id,
    )
    .context(Category::Refresh)?;
    let new = build_tokens(tr, id_token, access_scope, Some(&tokens)).context(Category::Refresh)?;
    profile
        .write_tokens(&lock, &new)
        .context(Category::Storage)?;
    check_plan(&new)?;
    Ok((account, new))
}

fn refresh_error(e: anyhow::Error) -> anyhow::Error {
    if e.downcast_ref::<Category>().is_some() {
        e
    } else {
        e.context(Category::Refresh)
    }
}

fn check_plan(tokens: &Tokens) -> Result<()> {
    if !tokens.scopes.iter().any(|s| s == PLAN_SCOPE) {
        return Err(anyhow!("missing ChatGPT plan permission").context(Category::Consent));
    }
    Ok(())
}

fn fresh(t: &Tokens) -> bool {
    let n = now();
    if t.expires_at > n + REFRESH_SKEW {
        return true;
    }
    // Respect earliest_refresh_at while the token hasn't actually expired.
    t.earliest_refresh_at.is_some_and(|e| n < e) && t.expires_at > n + 5
}

fn not_signed_in() -> anyhow::Error {
    tagged(
        anyhow!("not signed in with ChatGPT for this connection; sign in first"),
        Some(Category::SignIn),
    )
}

pub(crate) async fn logout(dir: &Path, ep: &Endpoints) -> Result<LogoutOutcome> {
    let Some(profile) = Profile::open(dir, false)? else {
        return Ok(LogoutOutcome {
            signed_out: false,
            remote_revoked: false,
        });
    };
    let lock = profile.lock().await?;
    let (Some(account), Some(tokens)) = (profile.account()?, profile.tokens()?) else {
        profile.clear_tokens(&lock)?;
        return Ok(LogoutOutcome {
            signed_out: false,
            remote_revoked: false,
        });
    };
    let mut revoked = false;
    if account.endpoints == *ep
        && http::check_endpoints(ep).is_ok()
        && let Ok(client) = http::client_for(ep)
        && let Ok(Discovery {
            revocation_endpoint: Some(rev),
            ..
        }) = oidc::discover(&client, &ep.issuer).await
    {
        let body = form(&[
            ("token", &tokens.refresh_token),
            ("token_type_hint", "refresh_token"),
            ("client_id", &account.client_id),
        ]);
        for attempt in 0..3u32 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500 << attempt)).await;
            }
            match client
                .post(&rev)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body.clone())
                .timeout(Duration::from_secs(20))
                .send()
                .await
            {
                Ok(r) if r.status().as_u16() == 200 => {
                    revoked = true;
                    break;
                }
                Ok(r) if r.status().is_server_error() => continue,
                Ok(_) => break,
                Err(_) => continue,
            }
        }
    }
    // Keep the client mapping and host ID for later sign-ins.
    profile.clear_tokens(&lock)?;
    Ok(LogoutOutcome {
        signed_out: true,
        remote_revoked: revoked,
    })
}

pub(crate) async fn status(dir: &Path) -> Result<Status> {
    let mut s = Status {
        signed_in: false,
        registered: false,
        plan_usage_enabled: false,
        email: None,
        subject: None,
        access_expires_at: None,
        ext_agent_host_id: None,
        manage_usage_url: USAGE_URL,
    };
    let Some(profile) = Profile::open(dir, false)? else {
        return Ok(s);
    };
    s.ext_agent_host_id = profile.host()?.map(|h| h.ext_agent_host_id);
    if let Some(a) = profile.account()? {
        s.registered = true;
        s.email = a.email;
        s.subject = a.subject;
    }
    if let Some(t) = profile.tokens()? {
        s.signed_in = true;
        s.plan_usage_enabled = t.scopes.iter().any(|x| x == PLAN_SCOPE);
        s.access_expires_at = Some(t.expires_at);
    }
    Ok(s)
}

pub(crate) async fn model_catalog(dir: &Path, ep: &Endpoints) -> Result<Vec<ModelInfo>> {
    let (_, tokens) = access(dir, ep).await?;
    let client = http::client_for(ep)?;
    let resp = client
        .get(format!("{}/models", ep.api_base))
        .bearer_auth(&tokens.access_token)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(http::net)?;
    let status = resp.status();
    if !status.is_success() {
        let body = http::read_bounded(resp, MAX_JSON).await.unwrap_or_default();
        return Err(Failure::parse(status.as_u16(), &body).error("model list"));
    }
    let shape = |e: anyhow::Error| mark(e, Detail::http(Phase::Http, status.as_u16(), None, None));
    let body = http::read_bounded(resp, MAX_JSON).await.map_err(shape)?;
    let v: Value = serde_json::from_slice(&body)
        .map_err(|_| shape(anyhow!("model list response is malformed")))?;
    let models = v
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| shape(anyhow!("model list response has no `models` array")))?;
    Ok(models
        .iter()
        .filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|m| {
            Some(ModelInfo {
                slug: m.get("slug")?.as_str()?.to_owned(),
                display_name: m
                    .get("display_name")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect())
}

pub(crate) async fn complete(dir: &Path, ep: &Endpoints, request: Value) -> Result<Value> {
    // Translate first, so an unsupported request never touches the network or
    // the refresh path.
    let body = translate::to_responses(&request).map_err(|e| {
        mark(
            tagged(e, Some(Category::Unsupported)),
            Detail::phase(Phase::Translation),
        )
    })?;
    let (_, tokens) = access(dir, ep).await?;
    let client = http::client_for(ep)?;
    let resp = client
        .post(format!("{}/responses", ep.api_base))
        .bearer_auth(&tokens.access_token)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(serde_json::to_vec(&body)?)
        .send()
        .await
        .map_err(http::net)?;
    let status = resp.status();
    if !status.is_success() {
        let bytes = http::read_bounded(resp, 64 * 1024)
            .await
            .unwrap_or_default();
        let failure = Failure::parse(status.as_u16(), &bytes);
        // Without a documented code, a 503 is either the service being down
        // or direct plan routing not being enabled; neither is the device's
        // network, and neither is a permanent rejection.
        if failure.status == 503 && failure.code_category().is_none() {
            return Err(mark(
                tagged(
                    anyhow!("{}", failure.describe("Responses request")),
                    Some(Category::ServiceOrRouting),
                ),
                failure.detail(Phase::Http),
            ));
        }
        return Err(failure.error("Responses request"));
    }
    let media_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .unwrap_or("")
        .trim();
    if !media_type.eq_ignore_ascii_case("text/event-stream") {
        if media_type.eq_ignore_ascii_case("application/json") {
            let bytes = http::read_bounded(resp, 64 * 1024).await?;
            let failure = Failure::parse(status.as_u16(), &bytes);
            if failure.category().is_some() {
                return Err(failure.error("Responses request"));
            }
        }
        return Err(mark(
            anyhow!("Responses did not return an event stream"),
            Detail::http(Phase::Http, status.as_u16(), None, None),
        ));
    }
    let completed = sse::collect(resp, sse::Limits::default())
        .await
        .map_err(|e| mark(e, Detail::phase(Phase::Stream)))?;
    translate::to_chat_completion(&completed)
        .map_err(|e| mark(e, Detail::phase(Phase::Translation)))
}
