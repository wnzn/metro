//! `metro-connector web …`: this computer as the person's web search device,
//! without the desktop app.
//!
//! Secrets never go on the command line: the Metro session token (used once
//! to register this computer, never stored), the device token and provider
//! keys come from private files (mode 600) or stdin (`-`).

use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{Endpoints, Engine, ProviderId, keys, settings};
use crate::desktop::metro_api::MetroApi;
use crate::fsutil::{read_private, read_secret_input, write_private};
use crate::metro::MetroClient;
use crate::netpolicy::validate_base_url;
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
  metro-connector web enable | disable [--profile NAME]
  metro-connector web order PROVIDER,PROVIDER,... [--profile NAME]
        The try order; providers left out are turned off.
  metro-connector web clear-cooldown PROVIDER [--profile NAME]

PROVIDERS: tavily, exa, parallel, firecrawl, tinyfish (tinyfish needs a key).
Default profile name: web.
";

const DEFAULT_PROFILE: &str = "web";
const DEVICE_FILE: &str = "web-device.json";

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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Device {
    v: u32,
    server_url: String,
    #[serde(default)]
    device_id: Option<String>,
}

fn profile(a: &Args) -> Result<Profile> {
    let name = a.profile.clone().unwrap_or_else(|| DEFAULT_PROFILE.into());
    validate_profile_name(&name)?;
    let p = Profile::open(&root_dir()?, &name)?;
    p.ensure()?;
    Ok(p)
}

fn device(p: &Profile) -> Result<Device> {
    let bytes = read_private(&p.dir.join(DEVICE_FILE), 16 * 1024)?.with_context(|| {
        format!(
            "web search isn't set up in profile {:?}; run `metro-connector web setup`",
            p.name
        )
    })?;
    serde_json::from_slice(&bytes).context("web-device.json is not valid")
}

fn provider(s: Option<&String>) -> Result<ProviderId> {
    let s = s.context("name a provider: tavily, exa, parallel, firecrawl or tinyfish")?;
    ProviderId::parse(s).with_context(|| format!("unknown provider {s:?}"))
}

fn say(s: impl AsRef<str>) {
    eprintln!("{}", s.as_ref());
}

async fn setup(a: Args) -> Result<()> {
    let p = profile(&a)?;
    let server = validate_base_url(
        a.server_url
            .as_deref()
            .context("--server-url is required")?,
        "server_url",
    )?;
    let (token, id) = match (&a.session_token_file, &a.device_token_file) {
        (Some(f), None) => {
            let session = read_secret_input(f, "session token")?;
            let label = a.label.clone().unwrap_or_else(|| "metro-connector".into());
            let api = MetroApi::new(&server, session)
                .map_err(|e| anyhow::anyhow!("cannot reach Metro: {e:?}"))?;
            let (id, token) = api
                .create_web_device(&label)
                .await
                .map_err(|e| anyhow::anyhow!("Metro refused to register this computer: {e:?}"))?;
            (token, Some(id))
        }
        (None, Some(f)) => {
            let t = read_secret_input(f, "device token")?;
            validate_device_token(&t)?;
            (t, None)
        }
        _ => bail!("give exactly one of --session-token-file and --device-token-file"),
    };
    p.save_device_token(&server, &token)?;
    let d = Device {
        v: 1,
        server_url: server.clone(),
        device_id: id.clone(),
    };
    write_private(&p.dir.join(DEVICE_FILE), &serde_json::to_vec_pretty(&d)?)?;
    let s = settings::load(&p.dir)?;
    settings::save(&p.dir, &s)?;
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
    let token = p.device_token_for(&d.server_url)?;
    let engine = Engine::new(&p.dir, Endpoints::from_env()?)?;
    let client = MetroClient::new(&d.server_url, token, Duration::from_secs(30))?;
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
    let s = settings::load(&p.dir)?;
    let k = keys::KeyStore::new(&p.dir).load();
    match device(&p) {
        Ok(d) => say(format!("server: {}", d.server_url)),
        Err(_) => say("server: not set up"),
    }
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

fn edit(a: &Args, f: impl FnOnce(&mut settings::Settings) -> Result<()>) -> Result<()> {
    let p = profile(a)?;
    let mut s = settings::load(&p.dir)?;
    f(&mut s)?;
    settings::save(&p.dir, &s)
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
    let pos = a.positional.clone();
    let r: Result<i32> = match args[0].as_str() {
        "setup" => setup(a).await.map(|_| 0),
        "run" => run(a).await,
        "status" => status(a).map(|_| 0),
        "enable" | "disable" => {
            let on = args[0] == "enable";
            edit(&a, |s| {
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
            edit(&a, |s| s.set_providers(&full).map_err(anyhow::Error::msg))
        })()
        .map(|_| 0),
        "clear-cooldown" => (|| {
            let pid = provider(pos.first())?;
            edit(&a, |s| {
                settings::clear_cooldowns(s, pid);
                s.rejected_keys.remove(pid.as_str());
                Ok(())
            })
        })()
        .map(|_| 0),
        "key" => (|| {
            let p = profile(&a)?;
            let store = keys::KeyStore::new(&p.dir);
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
                _ => bail!(
                    "use `web key set PROVIDER --from-file PATH` or `web key delete PROVIDER`"
                ),
            }
            Ok(())
        })()
        .map(|_| 0),
        other => {
            eprintln!(
                "metro-connector: unknown web command {other:?}; see `metro-connector web --help`"
            );
            return 2;
        }
    };
    match r {
        Ok(c) => c,
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
}
