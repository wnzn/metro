//! Claim -> execute -> report loop.
//!
//! This runner starts each claimed job at most once: the tool call is never
//! retried, only the result POST is (with backoff, bounded attempts and time).
//! This is not an exactly-once guarantee. A job interrupted by a timeout,
//! shutdown or crash, or whose result is never acknowledged, may already
//! have had side effects (for example from the check or an MCP tool) without
//! the server ever receiving a result for it.

use std::time::{Duration, Instant};

use crate::client::{Claim, ServerClient};
use crate::tools::{Registry, ToolResult};
use crate::util::{Backoff, sleep_interruptible};

pub const RESULT_MAX_ATTEMPTS: u32 = 8;
pub const RESULT_MAX_ELAPSED: Duration = Duration::from_secs(300);

pub struct Runner {
    pub client: ServerClient,
    pub registry: Registry,
    pub poll_interval: Duration,
    pub error_backoff: Backoff,
    pub result_backoff: Backoff,
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
    pub fn new(client: ServerClient, registry: Registry, poll_interval: Duration) -> Self {
        Runner {
            client,
            registry,
            poll_interval,
            error_backoff: Backoff::new(Duration::from_millis(500), Duration::from_secs(60)),
            result_backoff: Backoff::new(Duration::from_millis(250), Duration::from_secs(30)),
        }
    }

    /// Run until shutdown is requested.
    pub fn run(&mut self) {
        while !crate::shutdown_requested() {
            match self.step() {
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
        let (id, result) = match self.client.claim() {
            Ok(Claim::Idle) => {
                self.error_backoff.reset();
                return Step::Idle;
            }
            Ok(Claim::Job(job)) => {
                self.error_backoff.reset();
                eprintln!("metro-runner: job {} tool {}", job.id, job.tool);
                let started = Instant::now();
                let r = self.registry.dispatch(&job.tool, &job.arguments);
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
