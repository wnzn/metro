//! The claim → search → result loop of a web device.
//!
//! The same rules as the model loop (`crate::run`): a job runs at most once,
//! only the result post is retried, offline backs off up to 30 s, 401/403
//! stops. Differences: only `web_search` and `web_fetch` are served (any
//! other operation is refused), and the time budget is per operation, below
//! Metro's run timeouts (60 s / 150 s): work stops at 50 s / 140 s after the
//! claim was sent, delivery 5 s before Metro gives up. Several loops may
//! share one client and engine (the desktop runs two), so a long fetch never
//! blocks a search.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Engine;
use crate::metro::{Claim, ErrorKind, MetroClient, MetroError};
use crate::run::{DELIVERY_SAFETY_GAP, MAX_BACKOFF, RunOptions, RunState, Stop, deliver, log};
use crate::secret::clean_line;

/// Metro's run timeouts for web jobs (`crate::web_tools` on the server).
pub const SEARCH_SERVER_SECS: u64 = 60;
pub const FETCH_SERVER_SECS: u64 = 150;
/// Work budgets: Metro's timeout minus room for the result.
pub const SEARCH_WORK: Duration = Duration::from_secs(50);
pub const FETCH_WORK: Duration = Duration::from_secs(140);

/// `(work deadline, delivery deadline)` for `operation` claimed at `at`.
pub fn deadlines(operation: &str, at: Instant) -> (Instant, Instant) {
    let (work, server) = if operation == "web_fetch" {
        (FETCH_WORK, FETCH_SERVER_SECS)
    } else {
        (SEARCH_WORK, SEARCH_SERVER_SECS)
    };
    (
        at + work,
        at + Duration::from_secs(server) - DELIVERY_SAFETY_GAP,
    )
}

async fn sleep_or_stop(d: Duration, opts: &RunOptions) -> bool {
    let stopped = async {
        match &opts.stop {
            None => {
                let _ = tokio::signal::ctrl_c().await;
            }
            Some(rx) => {
                let mut rx = rx.clone();
                let _ = rx.wait_for(|v| *v).await;
            }
        }
    };
    tokio::select! {
        _ = tokio::time::sleep(d) => false,
        _ = stopped => true,
    }
}

fn report(opts: &RunOptions, s: RunState) {
    if let Some(f) = &opts.on_state {
        f(s);
    }
}

/// Serve web jobs until stopped.
pub async fn run(client: &MetroClient, engine: &Engine, opts: &RunOptions) -> Stop {
    run_guarded(client, engine, opts, &|| true).await
}

pub(crate) async fn run_guarded(
    client: &MetroClient,
    engine: &Engine,
    opts: &RunOptions,
    live: &(dyn Fn() -> bool + Sync),
) -> Stop {
    let mut backoff = Duration::from_secs(1);
    let mut offline = false;
    loop {
        if !live() {
            return Stop::Done;
        }
        let claim_sent = Instant::now();
        let stop_rx = opts.stop.clone();
        let claim = tokio::select! {
            c = client.claim() => c,
            _ = async {
                match stop_rx {
                    None => { let _ = tokio::signal::ctrl_c().await; }
                    Some(mut rx) => { let _ = rx.wait_for(|v| *v).await; }
                }
            } => return Stop::Done,
        };
        match claim {
            Ok(Claim::Idle) => {
                report(opts, RunState::Online);
                if offline {
                    log("web search: connected to Metro again");
                    offline = false;
                }
                backoff = Duration::from_secs(1);
                if opts.once {
                    return Stop::Done;
                }
                if sleep_or_stop(opts.poll_interval, opts).await {
                    return Stop::Done;
                }
            }
            Ok(Claim::Invalid { id, reason }) => {
                log(format!("web job {id}: refused ({reason})"));
                let body = json!({"ok": false, "error": crate::SafeError::new(&reason).as_str()});
                let (_, until) = deadlines("web_search", claim_sent);
                if let Err(stop) = deliver(client, &id, &body, until).await {
                    return stop;
                }
            }
            Ok(Claim::Job(job)) => {
                report(opts, RunState::Online);
                backoff = Duration::from_secs(1);
                let started = Instant::now();
                let (work, until) = deadlines(&job.operation, claim_sent);
                let body = engine
                    .handle(&job.id, &job.operation, &job.request, work)
                    .await;
                let ok = body["ok"] == Value::Bool(true);
                log(format!(
                    "web job {} {}: {} in {:.1}s",
                    job.id,
                    clean_line(&job.operation, 32),
                    if ok { "done" } else { "refused" },
                    started.elapsed().as_secs_f32()
                ));
                if let Err(stop) = deliver(client, &job.id, &body, until).await {
                    return stop;
                }
                if opts.once {
                    return Stop::Done;
                }
            }
            Err(MetroError {
                kind: ErrorKind::Unauthorized,
                ..
            }) => return Stop::Unauthorized,
            Err(MetroError {
                kind: ErrorKind::Retryable,
                message,
            }) => {
                report(opts, RunState::Offline);
                if !offline {
                    log(format!(
                        "web search: Metro is unreachable ({message}); retrying"
                    ));
                    offline = true;
                }
                if opts.once {
                    return Stop::Fatal(message);
                }
                if sleep_or_stop(backoff, opts).await {
                    return Stop::Done;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
            Err(err) => return Stop::Fatal(err.message),
        }
    }
}
