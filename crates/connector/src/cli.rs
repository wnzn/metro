//! Command line: `setup` (alias `init`), `login`, `logout`, `forget`,
//! `models`, `allow`, `status`, `list`, `run`.
//!
//! Secrets are never accepted on the command line. They come from a hidden
//! prompt, from a private file (`--device-token-file PATH`,
//! `--api-key-file PATH`) or from stdin (`-`).

use std::io::{BufRead, IsTerminal, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::Secret;
use crate::fsutil::read_secret_input;
use crate::metro::MetroClient;
use crate::netpolicy::{url_is_loopback, validate_base_url};
use crate::openai_compat::{self, Endpoint};
use crate::profile::{
    DEFAULT_PROFILE, DEFAULT_REQUEST_TIMEOUT_SECS, Kind, Profile, ProfileConfig, list_profiles,
    root_dir, validate_device_token, validate_model_name, validate_models, validate_profile_name,
    validate_provider_key,
};
use crate::provider::Provider;
use crate::run::{RunOptions, Stop, run};

pub const ENV_PROFILE: &str = "METRO_CONNECTOR_PROFILE";
pub const ENV_POLL_MS: &str = "METRO_CONNECTOR_POLL_MS";

pub const HELP: &str = "\
metro-connector: run your own model connection for Metro on this computer.

Your sign-in, CLI login or API key stays on this computer. Metro only gets
the answers to the requests you approve. The connector only makes outbound
connections to the Metro server you set up.

USAGE
  metro-connector setup   [options]   Set up a connection (asks questions)
  metro-connector run     [--profile NAME] [--once]
                                      Serve requests from Metro until Ctrl-C
  metro-connector login   [--profile NAME] [--api-key-file PATH|-] [--no-api-key]
                                      Sign in again, or replace the API key
  metro-connector logout  [--profile NAME]
                                      Sign out and delete the provider key or
                                      sign-in on this computer (keeps the
                                      device token)
  metro-connector forget  [--profile NAME] [--yes]
                                      Delete everything for this connection on
                                      this computer, including the device token
  metro-connector models  [--profile NAME]
                                      List the models your provider offers and
                                      which ones you approved
  metro-connector allow   [--profile NAME] MODEL...
                                      Set the approved models (replaces the list)
  metro-connector status  [--profile NAME]
  metro-connector list                List connection profiles
  metro-connector help | --help | --version

SETUP OPTIONS (anything you leave out is asked interactively)
  --profile NAME             Profile name (default: default). Use one profile
                             per connection you created in Metro.
  --server-url URL           Your Metro server, for example https://metro.example.com
                             (http:// only for this computer, e.g. http://127.0.0.1:8787)
  --kind KIND                device-openai-oauth | device-claude-code |
                             device-openai-compatible | device-systemone |
                             device-embeddings | device-embeddinggemma-2
                             device-systemone is a System One endpoint:
                             TypeSafe (https://api.typesafe.ai/v1) with your
                             own key, or a self-hosted OpenJev server
                             (http://127.0.0.1:3000/v1). device-embeddings is
                             an OpenAI-compatible embeddings endpoint.
                             device-embeddinggemma-2 is a media embedding
                             service that implements Metro's
                             /v1/media/embeddings (such as a self-hosted
                             EmbeddingGemma 2 server); approve
                             google/embeddinggemma-2 or the model it lists.
                             device-claude-code is Claude (local CLI): your
                             own Anthropic Console API key, billed at API
                             rates, no consumer subscription. You enter the
                             key on this computer (setup or `login`), never
                             in Metro.
  --device-token-file PATH   Read the Metro device token from a private file
                             (chmod 600), or - for stdin
  --base-url URL             The endpoint, for the openai-compatible,
                             systemone, embeddings and embeddinggemma-2
                             kinds, for example
                             https://api.example.com/v1 or http://127.0.0.1:11434/v1
  --api-key-file PATH        Read your API key from a private file, or - for stdin
  --no-api-key               The endpoint needs no key (local servers)
  --model NAME               Approve a model (repeat for more). Metro can only
                             use approved models.
  --no-login                 Don't start the sign-in now (run `login` later)
  --yes                      Don't ask for confirmation

ENVIRONMENT
  METRO_CONNECTOR_HOME       Where profiles are kept (default
                             ~/.config/metro-connector, mode 700)
  METRO_CONNECTOR_PROFILE    Default profile name
  METRO_CONNECTOR_POLL_MS    How often to ask Metro for work, in ms (default
                             2000, at most 30000)

Secrets are never accepted as command-line arguments, and are never printed.
";

#[derive(Debug, Default)]
struct Args {
    profile: Option<String>,
    server_url: Option<String>,
    kind: Option<String>,
    device_token_file: Option<String>,
    base_url: Option<String>,
    api_key_file: Option<String>,
    no_api_key: bool,
    models: Vec<String>,
    no_login: bool,
    yes: bool,
    once: bool,
    positional: Vec<String>,
}

const SECRET_FLAGS: &[&str] = &[
    "--device-token",
    "--token",
    "--api-key",
    "--key",
    "--password",
];

fn parse_args(args: &[String]) -> Result<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        if SECRET_FLAGS.contains(&flag) {
            bail!(
                "{flag} is not supported: secrets on the command line can leak through shell history and process lists. \
                 Use the hidden prompt, or --device-token-file / --api-key-file with a private file or -"
            );
        }
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
            "--kind" => a.kind = Some(value(flag)?),
            "--device-token-file" => a.device_token_file = Some(value(flag)?),
            "--base-url" => a.base_url = Some(value(flag)?),
            "--api-key-file" => a.api_key_file = Some(value(flag)?),
            "--model" => a.models.push(value(flag)?),
            "--no-api-key" => a.no_api_key = true,
            "--no-login" => a.no_login = true,
            "--yes" | "-y" => a.yes = true,
            "--once" => a.once = true,
            f if f.starts_with('-') && f != "-" => {
                bail!("unknown option {f}; see metro-connector --help")
            }
            _ => a.positional.push(arg.clone()),
        }
    }
    if a.device_token_file.as_deref() == Some("-") && a.api_key_file.as_deref() == Some("-") {
        bail!("only one of --device-token-file and --api-key-file can read from stdin");
    }
    if a.no_api_key && a.api_key_file.is_some() {
        bail!("use either --api-key-file or --no-api-key");
    }
    Ok(a)
}

fn interactive() -> bool {
    std::io::stdin().is_terminal()
}

/// Ask for a non-secret answer. Without a terminal the default (the saved
/// value, when re-running setup) is used; secrets never go through here.
fn ask(question: &str, default: Option<&str>) -> Result<String> {
    if !interactive() {
        return match default {
            Some(d) => Ok(d.to_string()),
            None => bail!("{question}: no answer given (pass it as an option; see --help)"),
        };
    }
    let mut err = std::io::stderr();
    match default {
        Some(d) => write!(err, "{question} [{d}]: ")?,
        None => write!(err, "{question}: ")?,
    }
    err.flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() {
        default.unwrap_or("").to_string()
    } else {
        line.to_string()
    })
}

fn confirm(question: &str, default_yes: bool, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !interactive() {
        return Ok(default_yes);
    }
    let a = ask(question, Some(if default_yes { "Y/n" } else { "y/N" }))?;
    Ok(match a.to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default_yes,
    })
}

fn ask_secret(question: &str) -> Result<Secret> {
    let v = rpassword::prompt_password(format!("{question}: ")).context(
        "cannot read hidden input (no terminal?); use a private file or - for stdin instead",
    )?;
    Ok(Secret::new(v.trim()))
}

fn signed_in(kind: Kind) -> &'static str {
    match kind {
        Kind::ClaudeCode => "Anthropic Console API key saved on this computer.",
        _ => "Signed in.",
    }
}

fn say(msg: impl AsRef<str>) {
    eprintln!("{}", msg.as_ref());
}

fn profile_name(a: &Args) -> Result<String> {
    let name = a
        .profile
        .clone()
        .or_else(|| std::env::var(ENV_PROFILE).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| DEFAULT_PROFILE.to_string());
    validate_profile_name(&name)?;
    Ok(name)
}

fn open_profile(a: &Args) -> Result<Profile> {
    Profile::open(&root_dir()?, &profile_name(a)?)
}

fn read_device_token(a: &Args) -> Result<Secret> {
    let t = match &a.device_token_file {
        Some(src) => read_secret_input(src, "device token")?,
        None => {
            if !interactive() {
                bail!("no device token: pass --device-token-file PATH (or - for stdin)");
            }
            say("Paste the device token Metro showed you when you created this connection.");
            ask_secret("Device token (input hidden)")?
        }
    };
    validate_device_token(&t)?;
    Ok(t)
}

fn read_api_key(a: &Args, base_url: &str) -> Result<Option<Secret>> {
    if a.no_api_key {
        return Ok(None);
    }
    let k = match &a.api_key_file {
        Some(src) => read_secret_input(src, "API key")?,
        None => {
            if !interactive() {
                bail!("no API key: pass --api-key-file PATH (or -), or --no-api-key");
            }
            let hint = if url_is_loopback(base_url) {
                " (leave empty if your local server needs none)"
            } else {
                ""
            };
            ask_secret(&format!("API key for {base_url}{hint} (input hidden)"))?
        }
    };
    if k.is_empty() {
        return Ok(None);
    }
    validate_provider_key(&k)?;
    Ok(Some(k))
}

fn choose_kind(a: &Args) -> Result<Kind> {
    if let Some(k) = &a.kind {
        return k.parse();
    }
    if !interactive() {
        bail!(
            "pass --kind (device-openai-oauth, device-claude-code, device-openai-compatible, \
             device-systemone, device-embeddings or device-embeddinggemma-2)"
        );
    }
    say("What kind of connection did you create in Metro?");
    for (i, k) in Kind::ALL.iter().enumerate() {
        say(format!("  {}) {}  ({})", i + 1, k.describe(), k.as_str()));
    }
    loop {
        let ans = ask(&format!("Choose 1-{}", Kind::ALL.len()), None)?;
        if let Ok(n) = ans.parse::<usize>()
            && (1..=Kind::ALL.len()).contains(&n)
        {
            return Ok(Kind::ALL[n - 1]);
        }
        if let Ok(k) = ans.parse() {
            return Ok(k);
        }
        say(format!(
            "Please enter a number from 1 to {}.",
            Kind::ALL.len()
        ));
    }
}

async fn choose_models(
    a: &Args,
    discover: impl std::future::Future<Output = Result<Vec<String>>>,
) -> Result<Vec<String>> {
    if !a.models.is_empty() {
        validate_models(&a.models)?;
        return Ok(a.models.clone());
    }
    if !interactive() {
        bail!("pass --model NAME for each model Metro may use");
    }
    say("Looking up the models your provider offers (from this computer)...");
    let available = match discover.await {
        Ok(m) => m,
        Err(e) => {
            say(format!(
                "Couldn't list models: {}",
                crate::secret::log_text(&e, &[])
            ));
            Vec::new()
        }
    };
    if !available.is_empty() {
        for (i, m) in available.iter().enumerate().take(200) {
            say(format!("  {:>3}) {m}", i + 1));
        }
    }
    loop {
        let ans = ask(
            "Which models may Metro use? Numbers or names, separated by commas",
            None,
        )?;
        let mut picked = Vec::new();
        let mut ok = true;
        for part in ans.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let name = match part.parse::<usize>() {
                Ok(n) if (1..=available.len()).contains(&n) => available[n - 1].clone(),
                _ => part.to_string(),
            };
            if validate_model_name(&name).is_err() {
                say(format!("{part:?} is not a valid model name."));
                ok = false;
                break;
            }
            if !picked.contains(&name) {
                picked.push(name);
            }
        }
        if ok && !picked.is_empty() && validate_models(&picked).is_ok() {
            return Ok(picked);
        }
        say("Choose at least one model.");
    }
}

/// What setup does with a saved secret.
enum Decision<T> {
    /// Keep what is saved. Only ever chosen when the destination is
    /// unchanged and the saved secret is bound to exactly that destination.
    Keep,
    /// Replace it with a freshly supplied value (`None` = keyless).
    Replace(T),
}

/// `setup` (and `init`).
///
/// Everything is asked and checked first; nothing on disk changes until the
/// very end, so an aborted or failed re-setup leaves the saved profile as it
/// was (vendor sign-in, if the person chooses to run it, manages its own
/// state). A saved device token or API key is reused only for the identical
/// normalized destination; a changed Metro server URL needs a new device
/// token, and a changed base URL needs a new key or `--no-api-key`. `--yes`
/// never bypasses that. Each secret is stored bound to its destination (see
/// `profile::SecretRecord`), so even an interrupted write can't pair an old
/// secret with a new host.
async fn cmd_setup(a: Args) -> Result<()> {
    let root = root_dir()?;
    let name = match (&a.profile, interactive()) {
        (Some(_), _) | (None, false) => profile_name(&a)?,
        (None, true) => {
            let n = ask("Profile name for this connection", Some(&profile_name(&a)?))?;
            validate_profile_name(&n)?;
            n
        }
    };
    let profile = Profile::open(&root, &name)?;
    let previous = if profile.exists() {
        profile.load().ok()
    } else {
        None
    };
    if profile.exists()
        && !confirm(
            &format!("Profile {name:?} already exists. Update it?"),
            false,
            a.yes,
        )?
    {
        bail!("setup cancelled");
    }

    // ---- Metro server and device token.
    let server_url = match &a.server_url {
        Some(u) => validate_base_url(u, "--server-url")?,
        None => {
            let def = previous.as_ref().map(|p| p.server_url.clone());
            let u = ask(
                "Metro server URL (shown on Metro's connection page)",
                def.as_deref(),
            )?;
            validate_base_url(&u, "server URL")?
        }
    };
    let kind = match (&a.kind, &previous) {
        (None, Some(p)) if !interactive() => p.kind,
        _ => choose_kind(&a)?,
    };
    if let Some(p) = &previous
        && p.kind != kind
    {
        bail!(
            "profile {name:?} is a {} connection; run `metro-connector forget --profile {name}` first, or use another --profile",
            p.kind
        );
    }

    let old_server = previous.as_ref().map(|p| p.server_url.clone());
    if let Some(old) = old_server.as_deref()
        && old != server_url
    {
        say(format!(
            "The Metro server changes from {old} to {server_url}. The saved device token belongs to {old} \
             and will not be reused; enter the device token Metro shows for {server_url}."
        ));
    }
    let token_reusable = old_server.as_deref() == Some(server_url.as_str())
        && profile.device_token_for(&server_url).is_ok();
    let token = if a.device_token_file.is_none()
        && token_reusable
        && confirm(
            &format!("Keep the saved device token for {server_url}?"),
            true,
            a.yes,
        )? {
        Decision::Keep
    } else {
        Decision::Replace(read_device_token(&a)?)
    };

    // ---- Model endpoint and key (endpoint kinds only).
    let mut key_decision: Decision<Option<Secret>> = Decision::Keep;
    let mut effective_key: Option<Secret> = None;
    let base_url = if kind.uses_endpoint() {
        let base_url = match &a.base_url {
            Some(u) => validate_base_url(u, "--base-url")?,
            None => {
                let def = previous.as_ref().and_then(|p| p.base_url.clone());
                let question = match kind {
                    Kind::SystemOne => {
                        "System One base URL (e.g. https://api.typesafe.ai/v1 or http://127.0.0.1:3000/v1)"
                    }
                    Kind::Embeddings => {
                        "Embeddings base URL (e.g. https://api.openai.com/v1 or http://127.0.0.1:11434/v1)"
                    }
                    Kind::MediaEmbeddings => {
                        "Media embedding service base URL, including /v1 (e.g. http://127.0.0.1:8090/v1)"
                    }
                    _ => {
                        "OpenAI-compatible base URL (e.g. https://api.example.com/v1 or http://127.0.0.1:11434/v1)"
                    }
                };
                let u = ask(question, def.as_deref())?;
                validate_base_url(&u, "base URL")?
            }
        };
        let old_base = previous.as_ref().and_then(|p| p.base_url.clone());
        let same_base = old_base.as_deref() == Some(base_url.as_str());
        if let Some(old) = old_base.as_deref()
            && !same_base
        {
            say(format!(
                "The model endpoint changes from {old} to {base_url}. A key saved for {old} \
                 will not be sent to {base_url}; enter the key for {base_url}, or choose no key."
            ));
        }
        // Only for the identical endpoint, and only if the saved state
        // (a key bound to it, or no key) is readable as such.
        let saved = if same_base {
            profile.provider_key_for(&base_url).ok()
        } else {
            None
        };
        key_decision = if a.no_api_key {
            Decision::Replace(None)
        } else if a.api_key_file.is_some() {
            Decision::Replace(read_api_key(&a, &base_url)?)
        } else if let Some(saved) = saved {
            let what = if saved.is_some() {
                "the saved API key"
            } else {
                "sending no API key"
            };
            if confirm(&format!("Keep {what} for {base_url}?"), true, a.yes)? {
                effective_key = saved;
                Decision::Keep
            } else {
                Decision::Replace(read_api_key(&a, &base_url)?)
            }
        } else {
            Decision::Replace(read_api_key(&a, &base_url)?)
        };
        if let Decision::Replace(k) = &key_decision {
            effective_key = k.clone();
        }
        Some(base_url)
    } else {
        if a.base_url.is_some() || a.api_key_file.is_some() || a.no_api_key {
            bail!(
                "--base-url, --api-key-file and --no-api-key are only for device-openai-compatible, \
                 device-systemone, device-embeddings and device-embeddinggemma-2"
            );
        }
        None
    };

    let mut cfg = ProfileConfig {
        version: 1,
        kind,
        server_url,
        models: previous
            .as_ref()
            .map(|p| p.models.clone())
            .unwrap_or_default(),
        base_url,
        request_timeout_secs: previous
            .as_ref()
            .map(|p| p.request_timeout_secs)
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS),
    };
    cfg.validate()?;

    // ---- Vendor sign-in (the adapter's own state, in vendor/ only).
    if !kind.uses_endpoint() {
        let (question, later) = match kind {
            Kind::ClaudeCode => (
                "Add your Anthropic Console API key on this computer now?".to_string(),
                format!(
                    "Add your Anthropic Console API key later with: metro-connector login --profile {name}\n\
                     Enter it on this computer only. Don't enter it in Metro."
                ),
            ),
            _ => (
                format!("Sign in now with {}?", kind.describe()),
                format!("You can sign in later with: metro-connector login --profile {name}"),
            ),
        };
        if !a.no_login && confirm(&question, true, a.yes)? {
            profile.ensure()?;
            Provider {
                profile: &profile,
                config: &cfg,
            }
            .login()
            .await
            .context("sign-in did not finish")?;
            say(signed_in(kind));
        } else {
            say(later);
        }
    }

    // ---- Approved models. Discovery uses the in-memory endpoint and key,
    // never what is saved for another destination.
    let keep_models = a.models.is_empty()
        && !cfg.models.is_empty()
        && (!interactive()
            || confirm(
                &format!("Keep the approved models ({})?", cfg.models.join(", ")),
                true,
                a.yes,
            )?);
    if !keep_models {
        let discover = async {
            match (&cfg.base_url, cfg.kind) {
                (Some(b), Kind::SystemOne | Kind::Embeddings | Kind::MediaEmbeddings) => {
                    let kind = if cfg.kind == Kind::SystemOne {
                        "system_one"
                    } else {
                        "embeddings"
                    };
                    let key = effective_key.as_ref().map(|k| k.expose());
                    metro_model_protocol::list_models(b, key, kind).await
                }
                (Some(b), _) => {
                    openai_compat::models(&Endpoint {
                        base_url: b.clone(),
                        key: effective_key.clone(),
                        timeout: Duration::from_secs(cfg.request_timeout_secs),
                    })
                    .await
                }
                (None, _) => {
                    Provider {
                        profile: &profile,
                        config: &cfg,
                    }
                    .available_models()
                    .await
                }
            }
        };
        cfg.models = choose_models(&a, discover).await?;
    }
    drop(effective_key);

    // ---- Commit. Secrets first, each bound to its new destination, then
    // profile.json. If this stops half-way, lookups see a destination
    // mismatch and refuse; nothing old is ever sent to a new host.
    cfg.validate()?;
    profile.ensure()?;
    if let Decision::Replace(t) = &token {
        profile.save_device_token(&cfg.server_url, t)?;
    }
    drop(token);
    if let (Decision::Replace(k), Some(b)) = (&key_decision, &cfg.base_url) {
        profile.save_provider_key(b, k.as_ref())?;
    }
    drop(key_decision);
    profile.save(&cfg)?;

    say("");
    say(format!("Saved connection profile {name:?} ({kind})."));
    say(format!("  Metro server:    {}", cfg.server_url));
    if let Some(b) = &cfg.base_url {
        say(format!("  Model endpoint:  {b}"));
    }
    say(format!("  Approved models: {}", cfg.models.join(", ")));
    say(format!("  Stored in:       {}", profile.dir.display()));
    say("");
    say(format!(
        "Start it with:  metro-connector run --profile {name}"
    ));
    Ok(())
}

async fn cmd_login(a: Args) -> Result<()> {
    let profile = open_profile(&a)?;
    let cfg = profile.load()?;
    if cfg.kind.uses_endpoint() {
        let base = cfg.base_url.as_deref().unwrap_or_default();
        let key = read_api_key(&a, base)?;
        profile.save_provider_key(base, key.as_ref())?;
        say(if key.is_some() {
            "API key saved."
        } else {
            "No API key will be sent."
        });
        return Ok(());
    }
    Provider {
        profile: &profile,
        config: &cfg,
    }
    .login()
    .await
    .context("sign-in did not finish")?;
    say(signed_in(cfg.kind));
    Ok(())
}

async fn cmd_logout(a: Args) -> Result<()> {
    let profile = open_profile(&a)?;
    let cfg = profile.load()?;
    Provider {
        profile: &profile,
        config: &cfg,
    }
    .logout()
    .await?;
    say(format!(
        "Signed out of profile {:?} on this computer. The Metro device token is kept; use `forget` to delete it too.",
        profile.name
    ));
    Ok(())
}

async fn cmd_forget(a: Args) -> Result<()> {
    let profile = open_profile(&a)?;
    if !profile.dir.exists() {
        say(format!("Nothing saved for profile {:?}.", profile.name));
        return Ok(());
    }
    if !confirm(
        &format!(
            "Delete profile {:?} from this computer (device token, key and sign-in)?",
            profile.name
        ),
        false,
        a.yes,
    )? {
        bail!("nothing deleted (pass --yes to confirm)");
    }
    if let Ok(cfg) = profile.load() {
        let _ = Provider {
            profile: &profile,
            config: &cfg,
        }
        .logout()
        .await;
    }
    profile.forget_all()?;
    say(format!(
        "Deleted profile {:?}. Also revoke the connection in Metro if you no longer use it.",
        profile.name
    ));
    Ok(())
}

async fn cmd_models(a: Args) -> Result<()> {
    let profile = open_profile(&a)?;
    let cfg = profile.load()?;
    let available = Provider {
        profile: &profile,
        config: &cfg,
    }
    .available_models()
    .await?;
    for m in &available {
        let mark = if cfg.models.contains(m) {
            "approved"
        } else {
            ""
        };
        println!("{m}\t{mark}");
    }
    for m in cfg.models.iter().filter(|m| !available.contains(m)) {
        println!("{m}\tapproved (not offered by the provider right now)");
    }
    Ok(())
}

fn cmd_allow(a: Args) -> Result<()> {
    let profile = open_profile(&a)?;
    let mut cfg = profile.load()?;
    let mut models = a.positional.clone();
    models.extend(a.models.iter().cloned());
    if models.is_empty() {
        bail!("name at least one model: metro-connector allow MODEL...");
    }
    validate_models(&models)?;
    cfg.models = models;
    profile.save(&cfg)?;
    say(format!("Approved models: {}", cfg.models.join(", ")));
    Ok(())
}

fn cmd_status(a: Args) -> Result<()> {
    let profile = open_profile(&a)?;
    let cfg = profile.load()?;
    println!("profile:      {}", profile.name);
    println!("kind:         {}", cfg.kind);
    println!("server:       {}", cfg.server_url);
    if let Some(b) = &cfg.base_url {
        println!("endpoint:     {b}");
        let key = match profile.provider_key_destination()? {
            None => "none".to_string(),
            Some(d) if &d == b => "saved".to_string(),
            Some(d) => format!("saved for a different endpoint ({d}); will not be used"),
        };
        println!("api key:      {key}");
    }
    let tok = match profile.device_token_destination() {
        Ok(Some(d)) if d == cfg.server_url => "saved".to_string(),
        Ok(Some(d)) => format!("saved for a different server ({d}); will not be used"),
        Ok(None) => "missing".to_string(),
        Err(_) => "unusable; run setup again".to_string(),
    };
    println!("device token: {tok}");
    println!("models:       {}", cfg.models.join(", "));
    println!("directory:    {}", profile.dir.display());
    Ok(())
}

fn cmd_list() -> Result<()> {
    let root = root_dir()?;
    for name in list_profiles(&root)? {
        let p = Profile::open(&root, &name)?;
        match p.load() {
            Ok(cfg) => println!("{name}\t{}\t{}", cfg.kind, cfg.server_url),
            Err(_) => println!("{name}\t(invalid profile)"),
        }
    }
    Ok(())
}

async fn cmd_run(a: Args) -> Result<i32> {
    let profile = open_profile(&a)?;
    let cfg = profile.load()?;
    if cfg.models.is_empty() {
        bail!(
            "no approved models; run `metro-connector allow --profile {} MODEL...`",
            profile.name
        );
    }
    let token = profile.device_token_for(&cfg.server_url)?;
    let client = if cfg.kind.embeds_media() {
        // Claims may carry an 8 MiB asset: more room and time to read them.
        MetroClient::new(&cfg.server_url, token, Duration::from_secs(120))?.with_media_claims()
    } else {
        MetroClient::new(&cfg.server_url, token, Duration::from_secs(30))?
    };
    let poll = std::env::var(ENV_POLL_MS)
        .ok()
        .filter(|v| !v.is_empty())
        .map(|v| {
            v.parse::<u64>()
                .context("METRO_CONNECTOR_POLL_MS must be a number")
        })
        .transpose()?
        .unwrap_or(2000)
        // Metro marks a device offline after 45s without a poll.
        .clamp(50, 30_000);
    let opts = RunOptions {
        poll_interval: Duration::from_millis(poll),
        once: a.once,
        ..RunOptions::default()
    };
    let provider = Provider {
        profile: &profile,
        config: &cfg,
    };
    say(format!(
        "metro-connector: serving profile {:?} ({}) for {}; approved models: {}. Press Ctrl-C to stop.",
        profile.name,
        cfg.kind,
        cfg.server_url,
        cfg.models.join(", ")
    ));
    Ok(match run(&client, &provider, &opts).await {
        Stop::Done => 0,
        Stop::Unauthorized => {
            say(format!(
                "metro-connector: Metro rejected the device token (it was revoked or replaced). \
                 Create a new one in Metro, then run `metro-connector setup --profile {}`.",
                profile.name
            ));
            3
        }
        Stop::Fatal(msg) => {
            say(format!("metro-connector: stopped: {msg}"));
            1
        }
    })
}

/// Entry point. Returns the process exit code.
pub async fn main_with(args: Vec<String>) -> i32 {
    let (cmd, rest) = match args.split_first() {
        Some((c, r)) => (c.as_str(), r),
        None => {
            eprint!("{HELP}");
            return 2;
        }
    };
    if matches!(cmd, "help" | "--help" | "-h") || rest.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return 0;
    }
    if matches!(cmd, "--version" | "-V" | "version") {
        println!("metro-connector {}", env!("CARGO_PKG_VERSION"));
        return 0;
    }
    let a = match parse_args(rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("metro-connector: {e:#}");
            return 2;
        }
    };
    if cmd != "allow" && !a.positional.is_empty() {
        eprintln!("metro-connector: unexpected argument {:?}", a.positional[0]);
        return 2;
    }
    let r = match cmd {
        "setup" | "init" => cmd_setup(a).await.map(|_| 0),
        "login" => cmd_login(a).await.map(|_| 0),
        "logout" => cmd_logout(a).await.map(|_| 0),
        "forget" => cmd_forget(a).await.map(|_| 0),
        "models" => cmd_models(a).await.map(|_| 0),
        "allow" => cmd_allow(a).map(|_| 0),
        "status" => cmd_status(a).map(|_| 0),
        "list" => cmd_list().map(|_| 0),
        "run" => cmd_run(a).await,
        other => {
            eprintln!("metro-connector: unknown command {other:?}; see metro-connector --help");
            return 2;
        }
    };
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("metro-connector: {}", crate::secret::log_text(&e, &[]));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn secrets_are_refused_on_argv() {
        for f in ["--device-token", "--api-key=sk-x", "--token", "--key"] {
            assert!(parse_args(&s(&[f, "v"])).is_err(), "{f}");
        }
    }

    #[test]
    fn parses_flags() {
        let a = parse_args(&s(&[
            "--profile=work",
            "--kind",
            "device-openai-compatible",
            "--model",
            "a",
            "--model=b",
            "--no-api-key",
            "--yes",
        ]))
        .unwrap();
        assert_eq!(a.profile.as_deref(), Some("work"));
        assert_eq!(a.models, vec!["a", "b"]);
        assert!(a.no_api_key && a.yes);
        assert!(parse_args(&s(&["--device-token-file", "-", "--api-key-file", "-"])).is_err());
        assert!(parse_args(&s(&["--bogus"])).is_err());
    }
}
