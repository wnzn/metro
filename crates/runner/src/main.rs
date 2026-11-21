use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use metro_runner::client::ServerClient;
use metro_runner::config::{Config, Sandbox};
use metro_runner::confine::Workspace;
use metro_runner::runner::Runner;
use metro_runner::tools::Registry;

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

fn main() -> ExitCode {
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("metro-runner: configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    let ws = match Workspace::open(&cfg.workspace) {
        Ok(w) => w,
        Err(e) => {
            eprintln!(
                "metro-runner: cannot open workspace {:?}: {e}",
                cfg.workspace
            );
            return ExitCode::from(2);
        }
    };
    if let Err(e) = preflight_docker(&cfg) {
        eprintln!("metro-runner: docker sandbox preflight failed (no host fallback): {e}");
        return ExitCode::from(2);
    }
    install_signal_handlers();

    let sandbox = match &cfg.sandbox {
        Sandbox::None => "none (host; trust boundary, not a sandbox)".to_string(),
        Sandbox::Docker(d) => format!("docker image={}", d.image),
    };
    eprintln!(
        "metro-runner {}: server={} workspace={} sandbox={} check={} mcp={}",
        env!("CARGO_PKG_VERSION"),
        cfg.server_url,
        cfg.workspace.display(),
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

    let client = ServerClient::new(&cfg);
    let poll = cfg.poll_interval;
    let cfg = Arc::new(cfg);
    let registry = Registry::standard(cfg, Arc::new(ws));
    Runner::new(client, registry, poll).run();
    ExitCode::SUCCESS
}
