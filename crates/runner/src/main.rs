use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use metro_runner::client::ServerClient;
use metro_runner::config::{Config, Credential, Sandbox};
use metro_runner::confine::Workspace;
use metro_runner::runner::Runner;
use metro_runner::spaces::SpaceRoots;
use metro_runner::tools::{Registry, SubprocessPolicy};
use metro_runner::update::{self, InstallKind, SelfUpdate, StartAction, Updater};

/// Where releases come from: GitHub and the vendored Sigstore root; in an
/// e2e test build, the loopback fixture source and the test verifier.
type ReleaseSource = (
    metro_release::fetch::Source,
    Box<dyn metro_release::ReleaseVerifier>,
);

fn release_source() -> Result<ReleaseSource, metro_release::Error> {
    #[cfg(feature = "e2e-test-release")]
    if let Some(s) = metro_release::e2e::release_source() {
        return Ok(s);
    }
    let verifier = metro_release::sigstore::SigstoreVerifier::vendored()?;
    Ok((metro_release::fetch::Source::github(), Box::new(verifier)))
}

/// Version reporting, and the self-updater when this host opted in and this
/// is a managed install of a published release build.
fn self_update(install: InstallKind, cfg: &Config) -> SelfUpdate {
    let channel = update::channel();
    let updater = match (&install, update::own_version()) {
        (InstallKind::Managed { root }, Some(own))
            if cfg.self_update && channel == metro_release::Channel::Release =>
        {
            match release_source() {
                Ok((source, verifier)) => Some(Updater {
                    root: root.clone(),
                    own,
                    fetcher: metro_release::fetch::BlockingFetcher::new(
                        source,
                        std::time::Duration::from_secs(300),
                    ),
                    verifier,
                }),
                Err(e) => {
                    eprintln!("metro-runner: warning: self-update unavailable: {e}");
                    None
                }
            }
        }
        _ => None,
    };
    let updates = SelfUpdate::new(install, channel, cfg.self_update, updater);
    eprintln!(
        "metro-runner: install={} channel={} self-update={}",
        updates.install.as_str(),
        channel.as_str(),
        if updates.enabled { "on" } else { "off" }
    );
    updates
}

extern "C" fn on_signal(_: libc::c_int) {
    if metro_runner::SHUTDOWN.swap(true, Ordering::SeqCst) {
        // Second signal: exit now. _exit is async-signal-safe.
        unsafe { libc::_exit(130) };
    }
}

fn install_signal_handlers() {
    for sig in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: the handler only touches an atomic and calls _exit.
        unsafe {
            libc::signal(sig, on_signal as *const () as libc::sighandler_t);
        }
    }
}

/// In docker mode, make sure the docker CLI works and the pinned image is
/// present locally before taking any job. Fails closed.
fn preflight_docker(cfg: &Config) -> Result<(), String> {
    let Sandbox::Docker(d) = &cfg.sandbox else {
        return Ok(());
    };
    let mut cmd = std::process::Command::new(&d.docker_bin);
    cmd.env_clear();
    for k in metro_runner::exec::DOCKER_CLI_ENV_ALLOWLIST {
        if let Ok(v) = std::env::var(k) {
            cmd.env(k, v);
        }
    }
    let out = cmd
        .args(["image", "inspect", "--format", "{{.Id}}", &d.image])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {:?}: {e}", d.docker_bin))?;
    if !out.status.success() {
        return Err(format!(
            "sandbox image {} is not available locally (pre-pull it; the runner uses --pull=never): {}",
            d.image,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Running inside a container with METRO_SANDBOX=docker means talking to
/// some daemon from inside a container. Say what that requires rather than
/// pretend it is a supported default.
fn warn_nested_docker(cfg: &Config) {
    if !matches!(cfg.sandbox, Sandbox::Docker(_)) {
        return;
    }
    let in_container = std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists();
    if in_container {
        eprintln!(
            "metro-runner: warning: METRO_SANDBOX=docker inside a container. The docker daemon \
             resolves bind-mount sources on ITS host, so {} must exist at the same path there \
             (bind-mount it at an identical path; a named volume path will not match), the \
             docker CLI must be in this image (the stock runner image has none), and whoever \
             can reach the daemon socket you provide is effectively root on that host. The \
             socket is never mounted into tool containers.",
            cfg.workspace.display()
        );
    }
}

fn main() -> ExitCode {
    let check_start = match std::env::args_os().nth(1) {
        None => false,
        Some(a) if a == "--check-start" && std::env::args_os().count() == 2 => true,
        Some(_) => {
            eprintln!(
                "metro-runner: usage: metro-runner [--check-start] (configured by environment)"
            );
            return ExitCode::from(2);
        }
    };
    // A managed runner counts its starts first, before anything that could
    // fail, so a crash-looping new version is rolled back (see `update`).
    let install = update::detect_current();
    if !check_start
        && let InstallKind::Managed { root } = &install
        && let Some(own) = update::own_version()
    {
        match update::on_start(root, &own) {
            Ok(StartAction::Continue) => {}
            Ok(StartAction::Busy) => {
                eprintln!(
                    "metro-runner: another installer holds the update lock for {own}; exiting to retry"
                );
                return ExitCode::from(75);
            }
            Ok(StartAction::RollBack(switch)) => {
                eprintln!(
                    "metro-runner: {own} failed to start {} times after an update; going back to the previous version",
                    update::MAX_STARTS
                );
                let e = switch.exec();
                eprintln!("metro-runner: could not start the previous version: {e}");
                return ExitCode::from(75);
            }
            Err(e) => {
                eprintln!(
                    "metro-runner: update recovery could not proceed: {e}; exiting before claiming work"
                );
                return ExitCode::from(75);
            }
        }
        // e2e test builds only: a version that "fails to start" after its
        // start was counted (tests/e2e/updates.py).
        #[cfg(feature = "e2e-test-release")]
        if std::env::var("METRO_TEST_CRASH_VERSION").is_ok_and(|v| v == own.to_string()) {
            eprintln!("TEST BUILD: {own} crashes on start (METRO_TEST_CRASH_VERSION)");
            return ExitCode::from(3);
        }
    }
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("metro-runner: configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = preflight_docker(&cfg) {
        eprintln!("metro-runner: docker sandbox preflight failed (no host fallback): {e}");
        return ExitCode::from(2);
    }
    warn_nested_docker(&cfg);

    // Open the workspace (dedicated) or the per-Space base (reusable) before
    // taking any job; both fail closed.
    enum Opened {
        Dedicated(Workspace),
        Reusable(SpaceRoots),
    }
    let opened = match cfg.credential {
        Credential::Dedicated => Workspace::open(&cfg.workspace).map(Opened::Dedicated),
        Credential::Reusable => SpaceRoots::open(&cfg.workspace).map(Opened::Reusable),
    };
    let opened = match opened {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "metro-runner: cannot open workspace {:?}: {e}",
                cfg.workspace
            );
            return ExitCode::from(2);
        }
    };
    if check_start {
        // Config, workspace and docker preflight passed; no server contact.
        eprintln!(
            "metro-runner {}: start check passed",
            env!("CARGO_PKG_VERSION")
        );
        return ExitCode::SUCCESS;
    }
    install_signal_handlers();

    let sandbox = match &cfg.sandbox {
        Sandbox::None => "none (host; trust boundary, not a sandbox)".to_string(),
        Sandbox::Docker(d) => format!("docker image={}", d.image),
    };
    let (credential, ws_desc) = match cfg.credential {
        Credential::Dedicated => ("dedicated", cfg.workspace.display().to_string()),
        Credential::Reusable => (
            "reusable",
            format!(
                "{}/{}/<space_id>",
                cfg.workspace.display(),
                metro_runner::spaces::SPACES_DIR
            ),
        ),
    };
    eprintln!(
        "metro-runner {}: server={} credential={} workspace={} sandbox={} check={} mcp={}",
        env!("CARGO_PKG_VERSION"),
        cfg.server_url,
        credential,
        ws_desc,
        sandbox,
        if cfg.check_argv.is_some() {
            "configured"
        } else {
            "off"
        },
        if cfg.mcp_argv.is_some() {
            "configured"
        } else {
            "off"
        },
    );

    if let Some(w) = cfg.plain_http_warning() {
        eprintln!("metro-runner: warning: {w}");
    }

    if let SubprocessPolicy::Disabled(why) = SubprocessPolicy::for_config(&cfg)
        && (cfg.check_argv.is_some() || cfg.mcp_argv.is_some())
    {
        eprintln!("metro-runner: warning: workspace.check/workspace.mcp {why}");
    }

    let updates = self_update(install, &cfg);
    let client = ServerClient::new(&cfg);
    let poll = cfg.poll_interval;
    let cfg = Arc::new(cfg);
    let mut runner = match opened {
        Opened::Dedicated(ws) => Runner::new(client, Registry::standard(cfg, Arc::new(ws)), poll),
        Opened::Reusable(roots) => Runner::per_space(client, cfg, Arc::new(roots), poll),
    };
    runner = runner.with_updates(updates);
    runner.run();
    ExitCode::SUCCESS
}
