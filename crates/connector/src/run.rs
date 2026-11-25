//! The claim → run → result loop.
//!
//! - One job at a time. A job is run at most once: if posting its result
//!   fails, only the post is retried (a few times, with backoff), never the
//!   request itself.
//! - Offline or 5xx: back off (1s doubling to 30s) and keep trying.
//! - Time: Metro abandons a claimed job 300s after the claim. Work is cut off
//!   at the profile's `request_timeout_secs` (default 240, at most 270), and
//!   result delivery is bounded by the rest of that window.
//! - 401/403: the device token was revoked or replaced; stop with a clear
//!   message instead of hammering the server.
//! - Log lines name the job id, operation and outcome only, never request or
//!   response bodies, tokens or keys.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::metro::{Claim, ErrorKind, Job, MAX_RESPONSE_BYTES, MetroClient, MetroError};
use crate::policy::{
    Operation, approved_models_named, check_typed, operation_allowed, parse_operation,
    sanitize_complete_for_kind, sanitize_response,
};
use crate::profile::{MAX_REQUEST_TIMEOUT_SECS, SERVER_RUN_TIMEOUT_SECS};
use crate::provider::Provider;
use crate::secret::{SafeError, log_text, metro_error_text};

pub const RESULT_ATTEMPTS: u32 = 5;
/// Below Metro's 45s "offline" threshold, so a short outage doesn't leave
/// the device looking offline long after it recovers.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct RunOptions {
    pub poll_interval: Duration,
    /// Stop after handling one job, or after the first idle claim.
    pub once: bool,
    /// Stop when this becomes `true` (the desktop app's sign-out, account
    /// switch or exit). `None`: only Ctrl-C stops the loop (the CLI).
    pub stop: Option<tokio::sync::watch::Receiver<bool>>,
    /// Called with each state change, for a UI. Never given secrets.
    pub on_state: Option<std::sync::Arc<dyn Fn(RunState) + Send + Sync>>,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            poll_interval: Duration::from_secs(2),
            once: false,
            stop: None,
            on_state: None,
        }
    }
}

/// What the loop is doing, for a UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// Metro answered the last claim.
    Online,
    /// Metro could not be reached; retrying.
    Offline,
}

impl RunOptions {
    fn report(&self, s: RunState) {
        if let Some(f) = &self.on_state {
            f(s);
        }
    }

    /// Resolves when the loop must stop: Ctrl-C for the CLI, the stop signal
    /// for the desktop app (whose GUI process has no terminal).
    async fn stopped(&self) {
        match &self.stop {
            None => {
                let _ = tokio::signal::ctrl_c().await;
            }
            Some(rx) => {
                let mut rx = rx.clone();
                // A dropped sender also means stop.
                let _ = rx.wait_for(|v| *v).await;
            }
        }
    }
}

#[derive(Debug)]
pub enum Stop {
    /// `--once` finished, or Ctrl-C.
    Done,
    /// The server rejected the device token.
    Unauthorized,
    /// Protocol or configuration problem.
    Fatal(String),
}

pub fn log(msg: impl AsRef<str>) {
    eprintln!("metro-connector: {}", msg.as_ref());
}

/// When a claimed job must be finished by. Metro's relay gives up on a job
/// `SERVER_RUN_TIMEOUT_SECS` after the claim. The work gets at most the
/// profile's `request_timeout_secs` (itself at most the server limit minus
/// `RESULT_MARGIN_SECS`); result delivery may use the time up to the server
/// limit minus a small safety gap.
#[derive(Debug, Clone, Copy)]
pub struct Deadlines {
    pub work: Instant,
    pub deliver: Instant,
}

impl Deadlines {
    pub fn from_claim(claimed_at: Instant, request_timeout: Duration) -> Self {
        let server = claimed_at + Duration::from_secs(SERVER_RUN_TIMEOUT_SECS);
        Deadlines {
            work: claimed_at + request_timeout.min(Duration::from_secs(MAX_REQUEST_TIMEOUT_SECS)),
            deliver: server - DELIVERY_SAFETY_GAP,
        }
    }
}

/// Kept unused at the end of the server's window, for latency and clock
/// differences between Metro marking the claim and us receiving it.
pub const DELIVERY_SAFETY_GAP: Duration = Duration::from_secs(5);
/// One result post never waits longer than this.
pub const RESULT_POST_TIMEOUT: Duration = Duration::from_secs(20);

/// Receives the device-local diagnostic of each failed ChatGPT catalog or
/// inference job, once per job, on this device only.
pub type FailureObserver = std::sync::Arc<dyn Fn(crate::openai_oauth::LocalFailure) + Send + Sync>;

/// Run one job and build the result body. Never returns provider error text
/// unless it is a `SafeError`. The adapter call is bounded by `deadline`:
/// when it passes, the adapter's future is dropped, which cancels its HTTP
/// requests and (claude_code) kills the CLI's process group.
pub async fn handle_job(provider: &Provider<'_>, job: &Job, deadline: Instant) -> Value {
    handle_job_observed(provider, job, deadline, None).await
}

/// [`handle_job`], also handing a failed ChatGPT catalog or inference call's
/// [`crate::openai_oauth::LocalFailure`] to `on_failure`. The result body is
/// the same either way: the diagnostic is never put in it.
pub async fn handle_job_observed(
    provider: &Provider<'_>,
    job: &Job,
    deadline: Instant,
    on_failure: Option<&FailureObserver>,
) -> Value {
    let secrets = provider.known_secrets();
    let secret_refs: Vec<&crate::Secret> = secrets.iter().collect();
    let op = match parse_operation(&job.operation, &job.request) {
        Ok(op) => op,
        Err(e) => return json!({"ok": false, "error": e.as_str()}),
    };
    // The profile's kind decides what it serves, never the job.
    if let Err(e) = operation_allowed(provider.config.kind, &op) {
        return json!({"ok": false, "error": e.as_str()});
    }
    let allowed = &provider.config.models;
    let work = async {
        match op {
            Operation::Models => {
                let available = provider.available_models_named().await?;
                Ok::<Value, anyhow::Error>(approved_models_named(allowed, &available))
            }
            Operation::Complete(req) => {
                let clean = sanitize_complete_for_kind(&req, allowed, provider.config.kind)?;
                let model = clean["model"].as_str().unwrap_or_default().to_string();
                let raw = provider.complete(clean).await?;
                Ok(sanitize_response(&raw, &model)?)
            }
            Operation::Decide(req) => {
                let clean = check_typed(&req, allowed)?;
                provider.decide(clean).await
            }
            Operation::Embed(req) => {
                let clean = check_typed(&req, allowed)?;
                provider.embed(clean).await
            }
            Operation::MediaEmbed(req) => {
                let clean = check_typed(&req, allowed)?;
                provider.media_embed(clean).await
            }
        }
    };
    let outcome = match tokio::time::timeout_at(deadline.into(), work).await {
        Ok(r) => r,
        Err(_) => Err(SafeError::new("the provider did not answer in time").into()),
    };
    match outcome {
        Ok(response) => {
            let len = serde_json::to_vec(&response)
                .map(|b| b.len())
                .unwrap_or(usize::MAX);
            if len > MAX_RESPONSE_BYTES {
                return json!({"ok": false, "error": "the response is too large to send to Metro"});
            }
            json!({"ok": true, "response": response})
        }
        Err(err) => {
            log(format!("job {}: {}", job.id, log_text(&err, &secret_refs)));
            if let (Some(observe), Some(failure)) =
                (on_failure, crate::openai_oauth::local_failure(&err))
            {
                observe(failure);
            }
            json!({"ok": false, "error": metro_error_text(&err)})
        }
    }
}

/// Post a result with bounded retries, never past `until` (after that Metro
/// has given up on the job anyway). Returns `Err` only for 401/403.
async fn deliver(client: &MetroClient, id: &str, body: &Value, until: Instant) -> Result<(), Stop> {
    let mut delay = Duration::from_secs(1);
    for attempt in 1..=RESULT_ATTEMPTS {
        let left = until.saturating_duration_since(Instant::now());
        if left < Duration::from_millis(200) {
            log(format!(
                "job {id}: no time left to deliver the result; the job will not be run again"
            ));
            return Ok(());
        }
        match client
            .post_result(id, body, left.min(RESULT_POST_TIMEOUT))
            .await
        {
            Ok(()) => return Ok(()),
            Err(MetroError {
                kind: ErrorKind::Unauthorized,
                ..
            }) => return Err(Stop::Unauthorized),
            Err(MetroError {
                kind: ErrorKind::Retryable,
                message,
            }) if attempt < RESULT_ATTEMPTS => {
                log(format!(
                    "job {id}: result not delivered ({message}); retrying"
                ));
                let left = until.saturating_duration_since(Instant::now());
                tokio::time::sleep(delay.min(left)).await;
                delay = (delay * 2).min(Duration::from_secs(8));
            }
            Err(err) => {
                log(format!(
                    "job {id}: result not delivered ({}); the job will not be run again",
                    err.message
                ));
                return Ok(());
            }
        }
    }
    Ok(())
}

async fn sleep_or_shutdown(d: Duration, opts: &RunOptions) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(d) => false,
        _ = opts.stopped() => true,
    }
}

pub async fn run(client: &MetroClient, provider: &Provider<'_>, opts: &RunOptions) -> Stop {
    run_inner(client, provider, opts, None).await
}

/// [`run`], handing each failed ChatGPT catalog or inference job's local
/// diagnostic to `on_failure` (see [`handle_job_observed`]).
pub async fn run_with_failure_observer(
    client: &MetroClient,
    provider: &Provider<'_>,
    opts: &RunOptions,
    on_failure: FailureObserver,
) -> Stop {
    run_inner(client, provider, opts, Some(&on_failure)).await
}

async fn run_inner(
    client: &MetroClient,
    provider: &Provider<'_>,
    opts: &RunOptions,
    on_failure: Option<&FailureObserver>,
) -> Stop {
    let approved_models = (provider.config.kind.capability() == crate::profile::Capability::Chat)
        .then_some(provider.config.models.as_slice());
    let mut backoff = Duration::from_secs(1);
    let mut offline = false;
    loop {
        // Metro starts a job's clock when it hands the job out, which is
        // after this moment: timing from here is conservative.
        let claim_sent = Instant::now();
        let claim = tokio::select! {
            c = client.claim_with_inventory(approved_models) => c,
            _ = opts.stopped() => return Stop::Done,
        };
        match claim {
            Ok(Claim::Idle) => {
                opts.report(RunState::Online);
                if offline {
                    log("connected to Metro again");
                    offline = false;
                }
                backoff = Duration::from_secs(1);
                if opts.once {
                    return Stop::Done;
                }
                if sleep_or_shutdown(opts.poll_interval, opts).await {
                    return Stop::Done;
                }
            }
            Ok(Claim::Invalid { id, reason }) => {
                log(format!("job {id}: refused ({reason})"));
                let body = json!({"ok": false, "error": SafeError::new(&reason).as_str()});
                let until =
                    claim_sent + Duration::from_secs(SERVER_RUN_TIMEOUT_SECS) - DELIVERY_SAFETY_GAP;
                if let Err(stop) = deliver(client, &id, &body, until).await {
                    return stop;
                }
                if opts.once {
                    return Stop::Done;
                }
            }
            Ok(Claim::Job(job)) => {
                opts.report(RunState::Online);
                if offline {
                    log("connected to Metro again");
                    offline = false;
                }
                backoff = Duration::from_secs(1);
                let started = Instant::now();
                let deadlines = Deadlines::from_claim(
                    claim_sent,
                    Duration::from_secs(provider.config.request_timeout_secs),
                );
                let body = handle_job_observed(provider, &job, deadlines.work, on_failure).await;
                let ok = body["ok"] == Value::Bool(true);
                log(format!(
                    "job {} {}: {} in {:.1}s",
                    job.id,
                    crate::secret::clean_line(&job.operation, 32),
                    if ok { "done" } else { "failed" },
                    started.elapsed().as_secs_f32()
                ));
                if let Err(stop) = deliver(client, &job.id, &body, deadlines.deliver).await {
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
                opts.report(RunState::Offline);
                if !offline {
                    log(format!("Metro is unreachable ({message}); retrying"));
                    offline = true;
                }
                if opts.once {
                    return Stop::Fatal(message);
                }
                if sleep_or_shutdown(backoff, opts).await {
                    return Stop::Done;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
            Err(err) => return Stop::Fatal(err.message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadlines_leave_headroom_for_the_result() {
        let t = Instant::now();
        let d = Deadlines::from_claim(t, Duration::from_secs(240));
        assert_eq!(d.work - t, Duration::from_secs(240));
        assert_eq!(d.deliver - t, Duration::from_secs(295));
        // A larger configured value never runs past server limit minus margin.
        let d = Deadlines::from_claim(t, Duration::from_secs(3600));
        assert_eq!(d.work - t, Duration::from_secs(270));
        assert!(d.work < d.deliver);
        assert!(d.deliver - t < Duration::from_secs(SERVER_RUN_TIMEOUT_SECS));
    }
}
