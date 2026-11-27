//! Claim -> execute -> report loop.
//!
//! This runner starts each claimed job at most once: the tool call is never
//! retried, only the result POST is (with backoff, bounded attempts and time).
//! This is not an exactly-once guarantee. A job interrupted by a timeout,
//! shutdown or crash, or whose result is never acknowledged, may already
//! have had side effects (for example from the check or an MCP tool) without
//! the server ever receiving a result for it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::client::{Claim, Job, Liveness, ServerClient, valid_space_id};
use crate::config::Config;
use crate::spaces::SpaceRoots;
use crate::tools::{Registry, ToolResult};
use crate::util::{Backoff, sleep_interruptible};

/// How often a running shell job asks whether the server still wants it.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

/// Asks the server about one running job until dropped; sets
/// [`crate::JOB_CANCELLED`] when the job is gone (run stopped, lease over,
/// runner changed). Transport errors never cancel.
struct Heartbeat {
    stop: Arc<AtomicBool>,
}

impl Heartbeat {
    fn start(client: ServerClient, id: String) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        // Not joined: a request in flight must not delay the result.
        std::thread::spawn(move || {
            loop {
                let until = Instant::now() + HEARTBEAT_INTERVAL;
                while Instant::now() < until {
                    if flag.load(Ordering::SeqCst) || crate::shutdown_requested() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                let gone = matches!(client.heartbeat(&id), Ok(Liveness::Gone));
                // A late answer for a finished job must not touch the next one.
                if gone && !flag.load(Ordering::SeqCst) {
                    eprintln!("metro-runner: job {id}: cancelled by the server; stopping it");
                    crate::JOB_CANCELLED.store(true, Ordering::SeqCst);
                    return;
                }
            }
        });
        Heartbeat { stop }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub const RESULT_MAX_ATTEMPTS: u32 = 8;
pub const RESULT_MAX_ELAPSED: Duration = Duration::from_secs(300);

/// How a claimed job finds its tools.
pub enum Dispatch {
    /// Dedicated (legacy) credential: one registry over `METRO_WORKSPACE`.
    Dedicated(Registry),
    /// Reusable credential: a fresh registry per job over
    /// `spaces/<space_id>/`. There is no registry over the base.
    PerSpace {
        cfg: Arc<Config>,
        roots: Arc<SpaceRoots>,
    },
}

impl Dispatch {
    fn run(&self, job: &Job) -> ToolResult {
        match self {
            Dispatch::Dedicated(r) => r.dispatch(&job.tool, &job.arguments),
            Dispatch::PerSpace { cfg, roots } => {
                // parse_claim_for already refused these; checked again so no
                // code path can reach the base without a valid Space id.
                let Some(sid) = job.space_id.as_deref().filter(|s| valid_space_id(s)) else {
                    return ToolResult::err(
                        "refusing job: no valid space_id for a reusable runner credential",
                    );
                };
                let ws = match roots.open_space(sid) {
                    Ok(w) => w,
                    Err(e) => {
                        return ToolResult::err(format!(
                            "refusing job: cannot open the workspace for Space {sid}: {e}"
                        ));
                    }
                };
                Registry::for_space(cfg.clone(), Arc::new(ws)).dispatch(&job.tool, &job.arguments)
            }
        }
    }
}

pub struct Runner {
    pub client: ServerClient,
    pub dispatch: Dispatch,
    pub poll_interval: Duration,
    pub error_backoff: Backoff,
    pub result_backoff: Backoff,
    /// Version reporting and the opt-in self-update.
    pub updates: crate::update::SelfUpdate,
    /// A version the server asked for on the last claim.
    pub update_request: Option<String>,
}

/// Outcome of a single loop iteration, for tests and logging.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Idle,
    Ran {
        id: String,
        ok: bool,
        reported: bool,
    },
    ClaimError,
}

impl Runner {
    /// Dedicated (legacy) runner over one registry.
    pub fn new(client: ServerClient, registry: Registry, poll_interval: Duration) -> Self {
        Self::with_dispatch(client, Dispatch::Dedicated(registry), poll_interval)
    }

    /// Reusable runner: each job runs in its own Space's root under `roots`.
    pub fn per_space(
        client: ServerClient,
        cfg: Arc<Config>,
        roots: Arc<SpaceRoots>,
        poll_interval: Duration,
    ) -> Self {
        Self::with_dispatch(client, Dispatch::PerSpace { cfg, roots }, poll_interval)
    }

    pub fn with_dispatch(
        client: ServerClient,
        dispatch: Dispatch,
        poll_interval: Duration,
    ) -> Self {
        Runner {
            client,
            dispatch,
            poll_interval,
            error_backoff: Backoff::new(Duration::from_millis(500), Duration::from_secs(60)),
            result_backoff: Backoff::new(Duration::from_millis(250), Duration::from_secs(30)),
            updates: crate::update::SelfUpdate::report_only(crate::update::InstallKind::Unmanaged),
            update_request: None,
        }
    }

    /// Set how this runner reports itself and whether it may self-update.
    pub fn with_updates(mut self, updates: crate::update::SelfUpdate) -> Self {
        self.updates = updates;
        self
    }

    /// Run until shutdown is requested.
    pub fn run(&mut self) {
        while !crate::shutdown_requested() {
            let step = self.step();
            // Between jobs only: the loop runs one job at a time, so nothing
            // is in flight here. On success this process is replaced.
            if let Some(version) = self.update_request.take() {
                self.updates.on_request(&version);
            }
            match step {
                Step::Idle => {
                    sleep_interruptible(self.poll_interval);
                }
                Step::Ran { .. } => {}
                Step::ClaimError => {
                    let d = self.error_backoff.next_delay();
                    sleep_interruptible(d);
                }
            }
        }
        eprintln!("metro-runner: shutdown requested, exiting");
    }

    pub fn step(&mut self) -> Step {
        let claimed = self
            .client
            .claim_with(&self.updates.body())
            .map(|(claim, update)| {
                self.updates.claim_succeeded();
                self.update_request = update;
                claim
            });
        let (id, result) = match claimed {
            Ok(Claim::Idle) => {
                self.error_backoff.reset();
                return Step::Idle;
            }
            Ok(Claim::Job(job)) => {
                self.error_backoff.reset();
                match &job.space_id {
                    Some(sid) if matches!(self.dispatch, Dispatch::PerSpace { .. }) => {
                        eprintln!("metro-runner: job {} tool {} space {sid}", job.id, job.tool)
                    }
                    _ => eprintln!("metro-runner: job {} tool {}", job.id, job.tool),
                }
                let started = Instant::now();
                crate::JOB_CANCELLED.store(false, Ordering::SeqCst);
                let beat = (job.tool == "workspace.shell")
                    .then(|| Heartbeat::start(self.client.clone(), job.id.clone()));
                let r = self.dispatch.run(&job);
                drop(beat);
                crate::JOB_CANCELLED.store(false, Ordering::SeqCst);
                eprintln!(
                    "metro-runner: job {} finished ok={} in {:.1}s ({} bytes)",
                    job.id,
                    r.ok,
                    started.elapsed().as_secs_f64(),
                    r.output.len()
                );
                (job.id, r)
            }
            Ok(Claim::Invalid { id, reason }) => {
                self.error_backoff.reset();
                eprintln!("metro-runner: job {id} rejected: {reason}");
                (id, ToolResult::err(reason))
            }
            Err(e) => {
                eprintln!(
                    "metro-runner: claim failed ({}): {e}",
                    if e.retryable {
                        "transient"
                    } else {
                        "non-transient"
                    }
                );
                return Step::ClaimError;
            }
        };
        let reported = self.report(&id, &result);
        Step::Ran {
            id,
            ok: result.ok,
            reported,
        }
    }

    /// POST the result, retrying transient failures. Retries only the
    /// delivery, never the tool call; returns false if no attempt was
    /// acknowledged.
    fn report(&mut self, id: &str, r: &ToolResult) -> bool {
        self.result_backoff.reset();
        let start = Instant::now();
        for attempt in 1..=RESULT_MAX_ATTEMPTS {
            match self.client.post_result(id, r.ok, &r.output) {
                Ok(()) => return true,
                Err(e) if !e.retryable => {
                    eprintln!("metro-runner: job {id}: result rejected, not retrying: {e}");
                    return false;
                }
                Err(e) => {
                    eprintln!(
                        "metro-runner: job {id}: result attempt {attempt}/{RESULT_MAX_ATTEMPTS} failed: {e}"
                    );
                    if attempt == RESULT_MAX_ATTEMPTS || start.elapsed() >= RESULT_MAX_ELAPSED {
                        break;
                    }
                    let d = self.result_backoff.next_delay();
                    if !sleep_interruptible(d) {
                        // Shutting down: one last immediate attempt.
                        return self.client.post_result(id, r.ok, &r.output).is_ok();
                    }
                }
            }
        }
        eprintln!("metro-runner: job {id}: giving up on reporting result");
        false
    }
}
