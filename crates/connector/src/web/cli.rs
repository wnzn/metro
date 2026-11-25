//! `metro-connector web …`: this computer as the person's web search device,
//! without the desktop app.
//!
//! Secrets never go on the command line: the Metro session token (used once
//! to register this computer, never stored), the device token and provider
//! keys come from private files (mode 600) or stdin (`-`).

use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::binding::{self, Binding};
use super::{Endpoints, Engine, ProviderId, keys, settings};
use crate::desktop::metro_api::{ApiError, MetroApi};
use crate::fsutil::read_secret_input;
use crate::metro::MetroClient;
use crate::netpolicy::{server_plain_http_warning, validate_server_url};
use crate::profile::{Profile, root_dir, validate_device_token, validate_profile_name};
use crate::run::{RunOptions, Stop};

pub const HELP: &str = "\
metro-connector web: run web searches for Metro on this computer, with your
own providers and keys. Searches run only for you, from this computer's
network; your keys stay here and are never sent to Metro.

USAGE
  metro-connector web setup --server-url URL (--session-token-file PATH | --device-token-file PATH)
                            [--label TEXT] [--profile NAME]
        Register this computer with Metro (the session token is used once
        and not stored), or save a web device token you already have.
  metro-connector web run [--profile NAME] [--once]
        Serve web searches until Ctrl-C.
  metro-connector web status [--profile NAME]
  metro-connector web key set PROVIDER --from-file PATH [--profile NAME]
  metro-connector web key delete PROVIDER [--profile NAME]
  metro-connector web key reset [--profile NAME]
        Delete every saved key (the way out when the key file can't be read).
  metro-connector web enable | disable [--profile NAME]
  metro-connector web order PROVIDER,PROVIDER,... [--profile NAME]
        The try order; providers left out are turned off.
  metro-connector web clear-cooldown PROVIDER [--profile NAME]

PROVIDERS: tavily, exa, parallel, firecrawl, tinyfish (tinyfish needs a key).
Default profile name: web.
";

const DEFAULT_PROFILE: &str = "web";

#[derive(Default, Debug)]
struct Args {
    profile: Option<String>,
    server_url: Option<String>,
    session_token_file: Option<String>,
    device_token_file: Option<String>,
    from_file: Option<String>,
    label: Option<String>,
    once: bool,
    positional: Vec<String>,
    /// The connector home (`root_dir()` when `None`); set by tests.
    root: Option<std::path::PathBuf>,
    /// Tests: fail setup right before the binding is published.
    fail_before_publish: bool,
}

fn parse(args: &[String]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => it
                    .next()
                    .cloned()
                    .with_context(|| format!("{name} needs a value")),
            }
        };
        match flag {
            "--profile" => a.profile = Some(value(flag)?),
            "--server-url" => a.server_url = Some(value(flag)?),
            "--session-token-file" => a.session_token_file = Some(value(flag)?),
            "--device-token-file" => a.device_token_file = Some(value(flag)?),
            "--from-file" => a.from_file = Some(value(flag)?),
            "--label" => a.label = Some(value(flag)?),
            "--once" => a.once = true,
            "--key" | "--api-key" | "--token" | "--session-token" | "--device-token" => bail!(
                "{flag} is not supported: secrets on the command line can leak; use a private file or -"
            ),
            f if f.starts_with('-') && f != "-" => {
                bail!("unknown option {f}; see `metro-connector web --help`")
            }
            _ => a.positional.push(arg.clone()),
        }
    }
    Ok(a)
}

fn profile(a: &Args) -> Result<Profile> {
    let name = a.profile.clone().unwrap_or_else(|| DEFAULT_PROFILE.into());
    validate_profile_name(&name)?;
    let root = match &a.root {
        Some(r) => r.clone(),
        None => root_dir()?,
    };
    // Private before anything is written, custom roots included (on
    // Windows: a protected DACL for this user and SYSTEM).
    for dir in [root.clone(), root.join("profiles")] {
        crate::desktop::store::prepare_private_dir(&dir)?;
    }
    let p = Profile::open(&root, &name)?;
    crate::desktop::store::prepare_private_dir(&p.dir)?;
    p.ensure()?;
    Ok(p)
}

/// Where `b`'s keys, settings and cooldowns live: one private directory per
/// (server, owner) inside the profile. The owner is `account:<stable id>`
/// when Metro confirmed the account at setup, else `device:<token
/// fingerprint>`, so re-pairing a profile for someone else never reuses
/// the previous person's keys.
fn data_dir(p: &Profile, b: &Binding) -> Result<std::path::PathBuf> {
    owner_dir(p, &b.server_url, &b.owner)
}

/// [`data_dir`] for (server, owner), whether or not they are bound now.
fn owner_dir(p: &Profile, server: &str, owner: &str) -> Result<std::path::PathBuf> {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for part in [server, owner] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    let name: String = h.finalize()[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let accounts = p.dir.join("accounts");
    crate::desktop::store::prepare_private_dir(&accounts)?;
    let dir = accounts.join(name);
    crate::desktop::store::prepare_private_dir(&dir)?;
    Ok(dir)
}

/// The web device Metro registered for one (server, owner) on this profile,
/// kept beside that owner's keys and settings (not in the active binding),
/// so switching accounts back and forth reuses each one's own device.
const OWNER_DEVICE: &str = "web-device-id.json";

fn owner_device(dir: &std::path::Path) -> Option<String> {
    let bytes = crate::fsutil::read_private(&dir.join(OWNER_DEVICE), 4096).ok()??;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v["device_id"]
        .as_str()
        .filter(|id| crate::metro::valid_job_id(id))
        .map(str::to_owned)
}

fn save_owner_device(dir: &std::path::Path, id: &str) -> Result<()> {
    crate::fsutil::write_private(
        &dir.join(OWNER_DEVICE),
        serde_json::json!({"v": 1, "device_id": id})
            .to_string()
            .as_bytes(),
    )
}

/// Per-profile setup lock (interprocess): one setup at a time, from reading
/// the current binding through registration, publication and cleanup.
async fn setup_lock(p: &Profile) -> Result<crate::fsutil::FileLock> {
    let path = p.dir.join("setup.lock");
    tokio::task::spawn_blocking(move || crate::fsutil::lock_exclusive(&path)).await?
}

/// The profile's binding: identity and credential from one record.
fn device(p: &Profile) -> Result<Binding> {
    binding::load(&p.dir)?.with_context(|| {
        format!(
            "web search isn't set up in profile {:?}; run `metro-connector web setup`",
            p.name
        )
    })
}

fn provider(s: Option<&String>) -> Result<ProviderId> {
    let s = s.context("name a provider: tavily, exa, parallel, firecrawl or tinyfish")?;
    ProviderId::parse(s).with_context(|| format!("unknown provider {s:?}"))
}

fn say(s: impl AsRef<str>) {
    eprintln!("{}", s.as_ref());
}

fn warn_plain_http(server: &str, credential: &str) {
    if let Some(w) = server_plain_http_warning(server, credential) {
        say(format!("metro-connector: warning: {w}"));
    }
}

async fn setup(a: Args) -> Result<()> {
    let server = validate_server_url(
        a.server_url
            .as_deref()
            .context("--server-url is required")?,
        "--server-url",
    )?;
    let p = profile(&a)?;
    let _setup = setup_lock(&p).await?;
    let existing = binding::load(&p.dir)?;
    // `created`: a registration this setup made, removed again if the local
    // part fails.
    let (token, id, owner, api, created) = match (&a.session_token_file, &a.device_token_file) {
        (Some(f), None) => {
            let session = read_secret_input(f, "session token")?;
            let label = a.label.clone().unwrap_or_else(|| "metro-connector".into());
            warn_plain_http(&server, "Metro session token");
            let api = MetroApi::new(&server, session)
                .map_err(|e| anyhow::anyhow!("cannot reach Metro: {e:?}"))?;
            let account = api
                .me()
                .await
                .map_err(|e| anyhow::anyhow!("Metro didn't confirm the account: {e:?}"))?;
            let owner = format!("account:{account}");
            // The same person on the same server keeps one device: its
            // token is rotated instead of registering another.
            let reuse = owner_device(&owner_dir(&p, &server, &owner)?).or_else(|| {
                existing
                    .as_ref()
                    .filter(|b| b.server_url == server && b.owner == owner)
                    .and_then(|b| b.device_id.clone())
            });
            let rotated = match reuse {
                Some(id) => match api.rotate_web_device(&id).await {
                    Ok(t) => Some((id, t)),
                    Err(ApiError::NotFound) => None,
                    Err(e) => bail!("Metro refused to renew this computer's token: {e:?}"),
                },
                None => None,
            };
            let (id, token, created) = match rotated {
                Some((id, t)) => (id, t, false),
                None => {
                    let (id, t) = api.create_web_device(&label).await.map_err(|e| {
                        anyhow::anyhow!("Metro refused to register this computer: {e:?}")
                    })?;
                    (id, t, true)
                }
            };
            (token, Some(id), owner, Some(api), created)
        }
        (None, Some(f)) => {
            // Nobody's account is confirmed: never take over a profile
            // that is already set up (its keys may be someone else's).
            if existing.is_some() {
                bail!(
                    "profile {:?} is already set up for web search; use another --profile, or \
                         --session-token-file so Metro confirms whose it is",
                    p.name
                );
            }
            let t = read_secret_input(f, "device token")?;
            validate_device_token(&t)?;
            warn_plain_http(&server, "web device token");
            let fp = super::keys::fingerprint(&t);
            (t, None, format!("device:{fp}"), None, false)
        }
        _ => bail!("give exactly one of --session-token-file and --device-token-file"),
    };
    let publish = || -> Result<()> {
        let b = Binding::new(&server, &owner, id.clone(), token)?;
        let dir = data_dir(&p, &b)?;
        if a.fail_before_publish {
            bail!("setup interrupted (test)");
        }
        if let Some(id) = &id {
            save_owner_device(&dir, id)?;
        }
        // The commit point, one atomic write: identity and credential.
        binding::save(&p.dir, &b)
    };
    if let Err(e) = publish() {
        if let (true, Some(api), Some(id)) = (created, &api, &id) {
            let _ = api.delete_web_device(id).await;
        }
        return Err(e);
    }
    say(format!(
        "Web search is set up in profile {:?} for {server}{}. Run `metro-connector web run --profile {}`.",
        p.name,
        id.map(|i| format!(" (device {i})")).unwrap_or_default(),
        p.name
    ));
    Ok(())
}

async fn run(a: Args) -> Result<i32> {
    let p = profile(&a)?;
    let d = device(&p)?;
    let engine = Engine::new(&data_dir(&p, &d)?, Endpoints::from_env()?)?;
    warn_plain_http(&d.server_url, "web device token");
    let client = MetroClient::new(&d.server_url, d.token().clone(), Duration::from_secs(30))?;
    let poll = std::env::var(crate::cli::ENV_POLL_MS)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2000)
        .clamp(50, 30_000);
    let opts = RunOptions {
        poll_interval: Duration::from_millis(poll),
        once: a.once,
        ..RunOptions::default()
    };
    say(format!(
        "metro-connector: serving web search for {} (profile {:?}). Press Ctrl-C to stop.",
        d.server_url, p.name
    ));
    let workers = if a.once { 1 } else { super::WORKERS_PER_DEVICE };
    let runs = (0..workers).map(|_| super::worker::run(&client, &engine, &opts));
    let stops = futures_join(runs).await;
    Ok(
        match stops
            .into_iter()
            .find(|s| !matches!(s, Stop::Done))
            .unwrap_or(Stop::Done)
        {
            Stop::Done => 0,
            Stop::Unauthorized => {
                say(
                    "metro-connector: Metro rejected this computer's web device token (removed or replaced). Run `metro-connector web setup` again.",
                );
                3
            }
            Stop::Fatal(m) => {
                say(format!("metro-connector: stopped: {m}"));
                1
            }
        },
    )
}

/// Run every future to completion (no extra dependency for one join).
async fn futures_join<F: std::future::Future<Output = Stop>>(
    fs: impl Iterator<Item = F>,
) -> Vec<Stop> {
    let handles: Vec<_> = fs.map(Box::pin).collect();
    let mut out = Vec::new();
    let mut pending = handles;
    // Poll them together.
    let res = std::future::poll_fn(|cx| {
        let mut i = 0;
        while i < pending.len() {
            match pending[i].as_mut().poll(cx) {
                std::task::Poll::Ready(s) => {
                    out.push(s);
                    drop(pending.swap_remove(i));
                }
                std::task::Poll::Pending => i += 1,
            }
        }
        if pending.is_empty() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    });
    res.await;
    out
}

fn status(a: Args) -> Result<()> {
    let p = profile(&a)?;
    let d = device(&p)?;
    let dir = data_dir(&p, &d)?;
    let s = settings::load(&dir)?;
    let k = keys::KeyStore::new(&dir).load();
    say(format!("server: {}", d.server_url));
    say(format!(
        "web search: {}",
        if s.enabled { "on" } else { "off" }
    ));
    if let Err(e) = &k {
        say(format!("keys: {e}"));
    }
    let now = super::time_now();
    for ps in &s.providers {
        let key = k.as_ref().ok().is_some_and(|k| k.has(ps.id.as_str()));
        let cooling: Vec<String> = s
            .cooldowns
            .iter()
            .filter(|(c, v)| c.starts_with(&format!("{}:", ps.id.as_str())) && v.until > now)
            .map(|(c, v)| {
                format!(
                    "{c} paused until {} ({})",
                    super::rfc3339(v.until),
                    v.outcome
                )
            })
            .collect();
        say(format!(
            "  {:<10} {:<4} {}{}",
            ps.id.as_str(),
            if ps.enabled { "on" } else { "off" },
            if key {
                "key saved"
            } else if ps.id.keyless() {
                "free"
            } else {
                "needs a key"
            },
            if cooling.is_empty() {
                String::new()
            } else {
                format!("; {}", cooling.join("; "))
            }
        ));
    }
    Ok(())
}

/// Change the person's preferences only. `off`: this change turns web
/// search off, which may start over from an unreadable file.
fn edit(a: &Args, off: bool, f: impl FnOnce(&mut settings::Settings) -> Result<()>) -> Result<()> {
    let p = profile(a)?;
    let dir = data_dir(&p, &device(&p)?)?;
    settings::update_prefs(&dir, off, f)
}

/// Change what jobs learned (an explicit action: may start over from an
/// unreadable state file).
fn edit_state(a: &Args, f: impl FnOnce(&mut settings::Settings) -> Result<()>) -> Result<()> {
    let p = profile(a)?;
    let dir = data_dir(&p, &device(&p)?)?;
    settings::update_state(&dir, true, f)
}

/// `metro-connector web …`.
pub async fn main(args: &[String]) -> i32 {
    if args.is_empty()
        || args
            .iter()
            .any(|a| a == "--help" || a == "-h" || a == "help")
    {
        print!("{HELP}");
        return if args.is_empty() { 2 } else { 0 };
    }
    let a = match parse(&args[1..]) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("metro-connector: {e:#}");
            return 2;
        }
    };
    match command(&args[0], a).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("metro-connector: {}", crate::secret::log_text(&e, &[]));
            1
        }
    }
}

/// One `web` command with parsed arguments.
async fn command(cmd: &str, a: Args) -> Result<i32> {
    let pos = a.positional.clone();
    let r: Result<i32> = match cmd {
        "setup" => setup(a).await.map(|_| 0),
        "run" => run(a).await,
        "status" => status(a).map(|_| 0),
        "enable" | "disable" => {
            let on = cmd == "enable";
            edit(&a, !on, |s| {
                s.enabled = on;
                Ok(())
            })
            .map(|_| 0)
        }
        "order" => (|| {
            let list: Vec<ProviderId> = pos
                .first()
                .context("give the order, like tavily,exa,parallel")?
                .split(',')
                .map(|p| {
                    ProviderId::parse(p.trim()).with_context(|| format!("unknown provider {p:?}"))
                })
                .collect::<Result<_>>()?;
            let mut full: Vec<(ProviderId, bool)> = list.iter().map(|&p| (p, true)).collect();
            for p in ProviderId::ALL {
                if !list.contains(&p) {
                    full.push((p, false));
                }
            }
            edit(&a, false, |s| {
                s.set_providers(&full).map_err(anyhow::Error::msg)
            })
        })()
        .map(|_| 0),
        "clear-cooldown" => (|| {
            let pid = provider(pos.first())?;
            edit_state(&a, |s| {
                settings::clear_cooldowns(s, pid);
                s.rejected_keys.remove(pid.as_str());
                Ok(())
            })
        })()
        .map(|_| 0),
        "key" => (|| {
            let p = profile(&a)?;
            let store = keys::KeyStore::new(&data_dir(&p, &device(&p)?)?);
            match pos.first().map(String::as_str) {
                Some("set") => {
                    let pid = provider(pos.get(1))?;
                    let file = a
                        .from_file
                        .as_deref()
                        .context("--from-file PATH (or -) is required")?;
                    store.set(pid, read_secret_input(file, "API key")?)?;
                    say(format!("Saved a key for {} on this computer.", pid.name()));
                }
                Some("delete") => {
                    let pid = provider(pos.get(1))?;
                    let had = store.delete(pid)?;
                    say(if had {
                        format!("Deleted the key for {}; it's used free again.", pid.name())
                    } else {
                        format!("No key was saved for {}.", pid.name())
                    });
                }
                Some("reset") => {
                    store.reset()?;
                    say("Deleted every saved web search key on this computer.");
                }
                _ => bail!(
                    "use `web key set PROVIDER --from-file PATH`, `web key delete PROVIDER` or \
                     `web key reset`"
                ),
            }
            Ok(())
        })()
        .map(|_| 0),
        other => {
            eprintln!(
                "metro-connector: unknown web command {other:?}; see `metro-connector web --help`"
            );
            return Ok(2);
        }
    };
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::write_private;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn secrets_are_refused_on_argv() {
        for bad in [
            "--key",
            "--api-key=x",
            "--token",
            "--session-token",
            "--device-token",
        ] {
            assert!(parse(&s(&[bad, "v"])).is_err(), "{bad}");
        }
        let a = parse(&s(&["tavily", "--from-file", "-", "--profile=p"])).unwrap();
        assert_eq!(a.positional, vec!["tavily"]);
        assert_eq!(a.from_file.as_deref(), Some("-"));
        assert_eq!(a.profile.as_deref(), Some("p"));
    }

    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use serde_json::{Value, json};

    const KEY: &str = "alice-tavily-key-123456";

    #[derive(Default)]
    struct FakeMetro {
        /// device id -> owner
        devices: std::collections::HashMap<String, String>,
        created: u32,
        rotated: u32,
        deleted: Vec<String>,
        /// device id -> its currently valid token
        tokens: std::collections::HashMap<String, String>,
    }

    fn who(h: &HeaderMap) -> Option<&'static str> {
        match h.get("authorization").and_then(|v| v.to_str().ok()) {
            Some("Bearer tok-alice") => Some("alice"),
            Some("Bearer tok-bob") => Some("bob"),
            _ => None,
        }
    }

    /// A token that names its owner: `metro_pc_` + `aa…`/`bb…` + a counter.
    fn token_of(owner: &str, n: u32) -> String {
        let tag = if owner == "alice" { "aa" } else { "bb" };
        format!("metro_pc_{tag}{n:062x}")
    }

    type Metro = Arc<Mutex<FakeMetro>>;

    async fn fakes() -> (String, Arc<Mutex<Vec<Option<String>>>>, Metro) {
        use axum::extract::Path as P;
        let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let metro: Metro = Arc::default();
        let (m1, m2, m3) = (metro.clone(), metro.clone(), metro.clone());
        let s2 = seen.clone();
        let app = Router::new()
            .route(
                "/v1/me",
                get(move |h: HeaderMap| async move {
                    axum::Json(json!({"id": who(&h).unwrap_or("nobody")}))
                }),
            )
            .route(
                "/v1/web-devices",
                post(move |h: HeaderMap| {
                    let m = m1.clone();
                    async move {
                        let owner = who(&h).unwrap_or("nobody").to_owned();
                        let mut m = m.lock().unwrap();
                        m.created += 1;
                        let id = format!("wd_{:032x}", m.created);
                        m.devices.insert(id.clone(), owner.clone());
                        let token = token_of(&owner, m.created);
                        m.tokens.insert(id.clone(), token.clone());
                        (axum::http::StatusCode::CREATED, axum::Json(json!({"id": id, "token": token})))
                            .into_response()
                    }
                }),
            )
            .route(
                "/v1/web-devices/{id}/token",
                post(move |h: HeaderMap, P(id): P<String>| {
                    let m = m2.clone();
                    async move {
                        let owner = who(&h).unwrap_or("nobody");
                        let mut m = m.lock().unwrap();
                        if m.devices.get(&id).map(String::as_str) != Some(owner) {
                            return axum::http::StatusCode::NOT_FOUND.into_response();
                        }
                        m.rotated += 1;
                        let token = token_of(owner, 1000 + m.rotated);
                        m.tokens.insert(id.clone(), token.clone());
                        (axum::http::StatusCode::CREATED, axum::Json(json!({"token": token})))
                            .into_response()
                    }
                }),
            )
            .route(
                "/v1/web-devices/{id}",
                axum::routing::delete(move |h: HeaderMap, P(id): P<String>| {
                    let m = m3.clone();
                    async move {
                        let owner = who(&h).unwrap_or("nobody");
                        let mut m = m.lock().unwrap();
                        if m.devices.get(&id).map(String::as_str) != Some(owner) {
                            return axum::http::StatusCode::NOT_FOUND;
                        }
                        m.devices.remove(&id);
                        m.deleted.push(id);
                        axum::http::StatusCode::NO_CONTENT
                    }
                }),
            )
            .route(
                "/tavily/search",
                post(move |h: HeaderMap| {
                    let s2 = s2.clone();
                    async move {
                        s2.lock().unwrap().push(
                            h.get("authorization").and_then(|v| v.to_str().ok()).map(str::to_owned),
                        );
                        axum::Json(json!({"results": [{"title": "t", "url": "https://t.example/", "content": "c"}]}))
                    }
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (base, seen, metro)
    }

    fn args(root: &std::path::Path, v: &[&str]) -> Args {
        let mut a = parse(&s(v)).unwrap();
        a.root = Some(root.to_path_buf());
        a
    }

    #[test]
    fn a_legacy_encoded_binding_keeps_its_exact_owner_and_keys() {
        let t = tempfile::tempdir().unwrap();
        let p = profile(&args(&t.path().join("home"), &[])).unwrap();
        let server = format!("http://127.0.0.1:8787/{}", "%C3%A9".repeat(350));
        assert!(server.len() > 2048);
        let owner = "account:alice";
        let token = crate::Secret::new(token_of("alice", 1));
        // Write the v1 format directly, as an older web setup would have.
        let wire = json!({
            "v": 1, "server_url": server, "owner": owner,
            "device_id": "wd_fixture", "token": token.expose(),
        });
        let sealed = crate::openai_oauth::store::seal(serde_json::to_vec(&wire).unwrap()).unwrap();
        write_private(&p.dir.join(binding::FILE), &sealed).unwrap();
        let old_dir = owner_dir(&p, &server, owner).unwrap();
        keys::KeyStore::new(&old_dir)
            .set(ProviderId::Tavily, crate::Secret::new(KEY))
            .unwrap();

        let d = device(&p).unwrap();
        assert_eq!(d.server_url, server);
        assert_eq!(d.owner, owner);
        assert_eq!(d.token_for(&server, owner).unwrap(), &token);
        assert!(d.token_for(&server, "account:bob").is_err());
        assert!(d.token_for(&format!("{server}-other"), owner).is_err());
        let dir = data_dir(&p, &d).unwrap();
        assert_eq!(dir, old_dir);
        assert!(keys::KeyStore::new(&dir).load().unwrap().has("tavily"));
    }

    async fn search_once(root: &std::path::Path, base: &str) -> Value {
        let a = args(root, &[]);
        let p = profile(&a).unwrap();
        let d = device(&p).unwrap();
        let mut e = super::super::Endpoints::vendor();
        e.tavily = format!("{base}/tavily");
        let engine = Engine::new(&data_dir(&p, &d).unwrap(), e).unwrap();
        engine
            .rotate(
                "j",
                &super::super::Op::Search(super::super::SearchReq {
                    query: "q".into(),
                    keywords: vec![],
                    max_results: 1,
                }),
                std::time::Instant::now() + Duration::from_secs(10),
            )
            .await
    }

    #[tokio::test]
    async fn re_pairing_for_another_account_or_server_never_reuses_the_previous_keys() {
        let (base, seen, _) = fakes().await;
        let (other_server, _, other_metro) = fakes().await;
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("home");
        let file = |name: &str, v: &str| {
            let p = t.path().join(name);
            write_private(&p, format!("{v}\n").as_bytes()).unwrap();
            p.to_string_lossy().into_owned()
        };
        let (alice, bob, key) = (file("a", "tok-alice"), file("b", "tok-bob"), file("k", KEY));
        let setup = |server: &str, session: &str| {
            args(
                &root,
                &["--server-url", server, "--session-token-file", session],
            )
        };
        assert_eq!(command("setup", setup(&base, &alice)).await.unwrap(), 0);
        assert_eq!(
            command("key", args(&root, &["set", "tavily", "--from-file", &key]))
                .await
                .unwrap(),
            0
        );
        let v = search_once(&root, &base).await;
        assert_eq!(v["mode"], "key");
        assert_eq!(
            seen.lock().unwrap().pop().unwrap().as_deref(),
            Some(format!("Bearer {KEY}").as_str())
        );

        // Bob re-pairs the same profile: Alice's key is not his.
        assert_eq!(command("setup", setup(&base, &bob)).await.unwrap(), 0);
        let v = search_once(&root, &base).await;
        assert_eq!(v["mode"], "keyless");
        assert_eq!(
            seen.lock().unwrap().pop().unwrap(),
            None,
            "no Authorization at all"
        );

        // Alice again: her own key is back.
        assert_eq!(command("setup", setup(&base, &alice)).await.unwrap(), 0);
        assert_eq!(search_once(&root, &base).await["mode"], "key");

        // The same account id on another server has a separate device and keys.
        assert_eq!(
            command("setup", setup(&other_server, &alice))
                .await
                .unwrap(),
            0
        );
        assert_eq!(other_metro.lock().unwrap().created, 1);
        assert_eq!(search_once(&root, &base).await["mode"], "keyless");

        // The canonical URL (not the spelling used at setup) identifies the server.
        assert_eq!(
            command("setup", setup(&format!("{base}/"), &alice))
                .await
                .unwrap(),
            0
        );
        assert_eq!(search_once(&root, &base).await["mode"], "key");

        // A device token alone can't take over a set-up profile...
        let token = file("t", &format!("metro_pc_{:064x}", 99));
        let only = |profile: &str| {
            args(
                &root,
                &[
                    "--server-url",
                    &base,
                    "--device-token-file",
                    &token,
                    "--profile",
                    profile,
                ],
            )
        };
        let mut a = only("web");
        a.profile = None;
        assert!(command("setup", a).await.is_err());
        // ...but sets up a fresh one, with no keys.
        assert_eq!(command("setup", only("fresh")).await.unwrap(), 0);
        let p = profile(&args(&root, &["--profile", "fresh"])).unwrap();
        let d = device(&p).unwrap();
        assert!(d.owner.starts_with("device:"));
        assert!(
            !keys::KeyStore::new(&data_dir(&p, &d).unwrap())
                .load()
                .unwrap()
                .has("tavily")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [root.clone(), root.join("profiles"), p.dir.clone()] {
                assert_eq!(
                    std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                    0o700,
                    "{dir:?}"
                );
            }
        }
    }

    fn session_files(t: &std::path::Path) -> (String, String) {
        let file = |name: &str, v: &str| {
            let p = t.join(name);
            write_private(&p, format!("{v}\n").as_bytes()).unwrap();
            p.to_string_lossy().into_owned()
        };
        (file("sa", "tok-alice"), file("sb", "tok-bob"))
    }

    /// The account a token was issued for, by its tag.
    fn token_owner(b: &Binding) -> &'static str {
        if b.token().expose().starts_with("metro_pc_aa") {
            "account:alice"
        } else {
            "account:bob"
        }
    }

    #[tokio::test]
    async fn an_interrupted_re_pair_keeps_one_whole_identity_and_cleans_up() {
        let (base, _, metro) = fakes().await;
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("home");
        let (alice, bob) = session_files(t.path());
        let setup = |session: &str| {
            args(
                &root,
                &["--server-url", &base, "--session-token-file", session],
            )
        };
        assert_eq!(command("setup", setup(&alice)).await.unwrap(), 0);
        let mut a = setup(&bob);
        a.fail_before_publish = true;
        assert!(command("setup", a).await.is_err());
        let p = profile(&args(&root, &[])).unwrap();
        let b = device(&p).unwrap();
        assert_eq!(b.owner, "account:alice");
        assert_eq!(
            token_owner(&b),
            "account:alice",
            "identity and token from one record"
        );
        // Bob's registration made by the failed setup was removed again.
        let m = metro.lock().unwrap();
        assert_eq!(m.created, 2);
        assert_eq!(m.deleted, vec![format!("wd_{:032x}", 2)]);
        assert_eq!(m.devices.len(), 1);
    }

    #[tokio::test]
    async fn repeated_setup_for_the_same_account_keeps_one_device() {
        let (base, _, metro) = fakes().await;
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("home");
        let (alice, bob) = session_files(t.path());
        let setup = |session: &str| {
            args(
                &root,
                &["--server-url", &base, "--session-token-file", session],
            )
        };
        for _ in 0..3 {
            assert_eq!(command("setup", setup(&alice)).await.unwrap(), 0);
        }
        {
            let m = metro.lock().unwrap();
            assert_eq!((m.created, m.rotated, m.devices.len()), (1, 2, 1));
        }
        let p = profile(&args(&root, &[])).unwrap();
        assert_eq!(
            device(&p).unwrap().device_id,
            Some(format!("wd_{:032x}", 1))
        );
        // Another account gets its own device; Alice's isn't Bob's to rotate.
        assert_eq!(command("setup", setup(&bob)).await.unwrap(), 0);
        assert_eq!(metro.lock().unwrap().created, 2);
        // The device was removed in Metro meanwhile: setup registers anew.
        metro.lock().unwrap().devices.clear();
        assert_eq!(command("setup", setup(&bob)).await.unwrap(), 0);
        assert_eq!(metro.lock().unwrap().created, 3);
    }

    #[test]
    fn a_reader_during_rebinds_never_sees_a_mixed_identity() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().to_path_buf();
        crate::fsutil::ensure_private_dir(&dir).unwrap();
        let rec = |who: &str, n: u32| {
            Binding::new(
                "https://m.example",
                &format!("account:{who}"),
                Some(format!("wd_{n}")),
                crate::Secret::new(token_of(who, n)),
            )
            .unwrap()
        };
        binding::save(&dir, &rec("alice", 0)).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let (dir, stop) = (dir.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 1;
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    let who = if n % 2 == 0 { "alice" } else { "bob" };
                    binding::save(&dir, &rec(who, n)).unwrap();
                    n += 1;
                }
            })
        };
        let mut seen = std::collections::HashSet::new();
        let start = std::time::Instant::now();
        let mut reads = 0;
        while (reads < 2000 || seen.len() < 2) && start.elapsed() < Duration::from_secs(30) {
            let b = binding::load(&dir).unwrap().unwrap();
            assert_eq!(token_owner(&b), b.owner, "mixed identity");
            seen.insert(b.owner.clone());
            reads += 1;
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        writer.join().unwrap();
        assert_eq!(seen.len(), 2, "both identities were observed");
    }

    #[tokio::test]
    async fn alternating_accounts_each_keep_their_own_device() {
        let (base, _, metro) = fakes().await;
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("home");
        let (alice, bob) = session_files(t.path());
        let setup = |session: &str| {
            args(
                &root,
                &["--server-url", &base, "--session-token-file", session],
            )
        };
        for _ in 0..3 {
            for who in [&alice, &bob] {
                assert_eq!(command("setup", setup(who)).await.unwrap(), 0);
            }
        }
        let m = metro.lock().unwrap();
        assert_eq!(m.created, 2, "exactly one registration each");
        assert_eq!(m.devices.len(), 2);
        assert_eq!(m.rotated, 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn overlapping_setups_register_once_and_leave_a_working_binding() {
        let (base, _, metro) = fakes().await;
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("home");
        let (alice, _) = session_files(t.path());
        let setup = || {
            args(
                &root,
                &["--server-url", &base, "--session-token-file", &alice],
            )
        };
        let (a, b) = tokio::join!(command("setup", setup()), command("setup", setup()));
        assert_eq!((a.unwrap(), b.unwrap()), (0, 0));
        let p = profile(&args(&root, &[])).unwrap();
        let bound = device(&p).unwrap();
        let m = metro.lock().unwrap();
        assert_eq!(m.created, 1, "no duplicate registration");
        let id = bound.device_id.clone().unwrap();
        // The published token is the one Metro accepts now (it can claim).
        assert_eq!(m.tokens[&id], bound.token().expose());
    }
}
