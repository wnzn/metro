//! Bounded job queues between the server and each Space's paired runner.
//!
//! Every Space has its own queue. A runner token resolves to one Space and a
//! credential *generation*; the generation goes up whenever the owner
//! rotates or revokes the token. A runner only ever claims jobs of its own
//! Space, and a result is accepted only from the Space and generation that
//! claimed the job, so a stale or foreign runner can never answer a job.
//!
//! Jobs live in memory only: a job belongs to the request or agent run that
//! waits for it, and the room history in SQLite is the durable record. A job
//! is never re-queued once claimed, because tools such as `workspace.check`
//! need not be idempotent; a claimed job that misses its deadline, or whose
//! runner credential is replaced, fails.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::oneshot;
use tokio::time::Instant;

/// How many finished job ids are remembered to tell duplicates and late
/// results from unknown ids.
const FINISHED_MEMORY: usize = 4096;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ClaimedJob {
    pub id: String,
    pub tool: String,
    pub arguments: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobOutcome {
    pub ok: bool,
    pub output: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum JobError {
    /// The Space's queue is at capacity.
    QueueFull,
    /// No runner claimed the job in time.
    NotClaimed,
    /// The runner claimed the job but did not answer in time.
    TimedOut,
    /// The Space's runner token was rotated or revoked while the job was pending.
    CredentialChanged,
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            JobError::QueueFull => "the runner queue is full; try again shortly",
            JobError::NotClaimed => "no runner picked up the job in time; is the runner connected?",
            JobError::TimedOut => "the runner did not return a result in time",
            JobError::CredentialChanged => {
                "the Space's runner token was rotated or revoked; the job was cancelled"
            }
        })
    }
}

impl std::error::Error for JobError {}

#[derive(Debug, PartialEq, Eq)]
pub enum CompleteError {
    /// No job with this id is known to this Space.
    Unknown,
    /// The job exists but has not been claimed.
    NotClaimed,
    /// The job already has a result; the duplicate is ignored.
    Duplicate,
    /// The job failed (timed out, abandoned or cancelled) before the result arrived.
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Finished {
    Completed,
    Expired,
}

enum State {
    Queued,
    Claimed { at: Instant, generation: u64 },
}

struct Job {
    space: String,
    tool: String,
    arguments: serde_json::Value,
    state: State,
    tx: oneshot::Sender<Result<JobOutcome, JobError>>,
}

#[derive(Default)]
struct SpaceQueue {
    queue: VecDeque<String>,
    /// Highest credential generation seen for this Space.
    generation: u64,
}

#[derive(Default)]
struct Inner {
    spaces: HashMap<String, SpaceQueue>,
    jobs: HashMap<String, Job>,
    finished: HashMap<String, (String, Finished)>,
    finished_order: VecDeque<String>,
}

impl Inner {
    fn finish(&mut self, id: &str, space: &str, how: Finished) {
        if self
            .finished
            .insert(id.to_owned(), (space.to_owned(), how))
            .is_none()
        {
            self.finished_order.push_back(id.to_owned());
            while self.finished_order.len() > FINISHED_MEMORY {
                if let Some(old) = self.finished_order.pop_front() {
                    self.finished.remove(&old);
                }
            }
        }
    }

    /// Drop `id` if it is still pending, telling its waiter `why`.
    fn fail(&mut self, id: &str, why: Option<JobError>) {
        if let Some(job) = self.jobs.remove(id) {
            if let Some(q) = self.spaces.get_mut(&job.space) {
                q.queue.retain(|q| q != id);
            }
            self.finish(id, &job.space, Finished::Expired);
            if let Some(why) = why {
                let _ = job.tx.send(Err(why));
            }
        }
    }

    /// Note that `space` now uses credential `generation`. Returns false when
    /// the caller holds an older generation than one already seen. A newer
    /// generation fails every job claimed under an older one, and with
    /// `drop_queued` every queued job too.
    fn observe(&mut self, space: &str, generation: u64, drop_queued: bool) -> bool {
        let q = self.spaces.entry(space.to_owned()).or_default();
        if generation < q.generation {
            return false;
        }
        let advanced = generation > q.generation;
        q.generation = generation;
        if advanced || drop_queued {
            let doomed: Vec<String> = self
                .jobs
                .iter()
                .filter(|(_, job)| job.space == space)
                .filter(|(_, job)| match job.state {
                    State::Claimed { generation: g, .. } => g < generation,
                    State::Queued => drop_queued,
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in doomed {
                self.fail(&id, Some(JobError::CredentialChanged));
            }
        }
        true
    }
}

#[derive(Clone)]
pub struct JobQueue {
    inner: Arc<Mutex<Inner>>,
    capacity: usize,
    claim_timeout: Duration,
    run_timeout: Duration,
}

impl JobQueue {
    /// `capacity` bounds the queued jobs of each Space.
    pub fn new(capacity: usize, claim_timeout: Duration, run_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            capacity,
            claim_timeout,
            run_timeout,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Queue `tool` for `space`'s runner and wait for its result. Dropping
    /// the future abandons the job: a queued job is withdrawn and a late
    /// result is refused as expired.
    pub async fn run(
        &self,
        space: &str,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<JobOutcome, JobError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, mut rx) = oneshot::channel();
        {
            let mut inner = self.lock();
            let q = inner.spaces.entry(space.to_owned()).or_default();
            if q.queue.len() >= self.capacity {
                return Err(JobError::QueueFull);
            }
            q.queue.push_back(id.clone());
            inner.jobs.insert(
                id.clone(),
                Job {
                    space: space.to_owned(),
                    tool: tool.to_owned(),
                    arguments,
                    state: State::Queued,
                    tx,
                },
            );
        }
        let guard = AbandonGuard {
            queue: self,
            id: &id,
        };

        let claim_deadline = Instant::now() + self.claim_timeout;
        if let Ok(result) = tokio::time::timeout_at(claim_deadline, &mut rx).await {
            drop(guard);
            return result.unwrap_or(Err(JobError::TimedOut));
        }
        // Not answered before the claim deadline: fail it if still queued,
        // otherwise give it the rest of its run time.
        let run_deadline = {
            let mut inner = self.lock();
            match inner.jobs.get(&id).map(|job| &job.state) {
                Some(State::Queued) => {
                    inner.fail(&id, None);
                    return Err(JobError::NotClaimed);
                }
                Some(State::Claimed { at, .. }) => *at + self.run_timeout,
                // Finished while we took the lock; the result is in `rx`.
                None => Instant::now(),
            }
        };
        let result = match tokio::time::timeout_at(run_deadline, &mut rx).await {
            Ok(result) => result.unwrap_or(Err(JobError::TimedOut)),
            Err(_) => rx.try_recv().unwrap_or(Err(JobError::TimedOut)),
        };
        drop(guard);
        result
    }

    /// The oldest queued job of `space`, now claimed by the runner holding
    /// credential `generation`. `None` when there is none, or when the
    /// credential has been superseded.
    pub fn claim(&self, space: &str, generation: u64) -> Option<ClaimedJob> {
        let mut inner = self.lock();
        if !inner.observe(space, generation, false) {
            return None;
        }
        let now = Instant::now();
        while let Some(id) = inner
            .spaces
            .get_mut(space)
            .and_then(|q| q.queue.pop_front())
        {
            if let Some(job) = inner.jobs.get_mut(&id) {
                job.state = State::Claimed {
                    at: now,
                    generation,
                };
                return Some(ClaimedJob {
                    id,
                    tool: job.tool.clone(),
                    arguments: job.arguments.clone(),
                });
            }
        }
        None
    }

    /// Accept a result from `space`'s runner holding credential `generation`.
    pub fn complete(
        &self,
        space: &str,
        generation: u64,
        id: &str,
        outcome: JobOutcome,
    ) -> Result<(), CompleteError> {
        let mut inner = self.lock();
        if !inner.observe(space, generation, false) {
            return Err(CompleteError::Expired);
        }
        match inner.jobs.get(id) {
            // Another Space's job is indistinguishable from an unknown one.
            Some(job) if job.space != space => return Err(CompleteError::Unknown),
            None => {
                return Err(match inner.finished.get(id) {
                    Some((s, _)) if s != space => CompleteError::Unknown,
                    Some((_, Finished::Completed)) => CompleteError::Duplicate,
                    Some((_, Finished::Expired)) => CompleteError::Expired,
                    None => CompleteError::Unknown,
                });
            }
            Some(job) => match job.state {
                State::Queued => return Err(CompleteError::NotClaimed),
                State::Claimed { generation: g, .. } if g != generation => {
                    inner.fail(id, Some(JobError::CredentialChanged));
                    return Err(CompleteError::Expired);
                }
                State::Claimed { at, .. } if at.elapsed() > self.run_timeout => {
                    inner.fail(id, Some(JobError::TimedOut));
                    return Err(CompleteError::Expired);
                }
                State::Claimed { .. } => {}
            },
        }
        let job = inner.jobs.remove(id).expect("checked above");
        inner.finish(id, space, Finished::Completed);
        drop(inner);
        // The waiter may have just given up; the result then goes nowhere.
        let _ = job.tx.send(Ok(outcome));
        Ok(())
    }

    /// The Space's token was rotated to `generation`: jobs claimed under the
    /// old token fail; queued jobs wait for the new runner.
    pub fn rotate(&self, space: &str, generation: u64) {
        self.lock().observe(space, generation, false);
    }

    /// The Space's token was revoked (`generation` is the new, tokenless
    /// generation): every pending job fails.
    pub fn revoke(&self, space: &str, generation: u64) {
        self.lock().observe(space, generation, true);
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        self.lock().jobs.len()
    }
}

/// Withdraws an unfinished job when its waiter goes away.
struct AbandonGuard<'a> {
    queue: &'a JobQueue,
    id: &'a str,
}

impl Drop for AbandonGuard<'_> {
    fn drop(&mut self) {
        self.queue.lock().fail(self.id, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn queue() -> JobQueue {
        JobQueue::new(2, Duration::from_millis(200), Duration::from_millis(300))
    }

    async fn claim_wait(q: &JobQueue, space: &str, generation: u64) -> ClaimedJob {
        loop {
            if let Some(job) = q.claim(space, generation) {
                return job;
            }
            tokio::task::yield_now().await;
        }
    }

    fn ok(output: &str) -> JobOutcome {
        JobOutcome {
            ok: true,
            output: output.into(),
        }
    }

    #[tokio::test]
    async fn claim_and_complete_within_one_space() {
        let q = queue();
        let runner = q.clone();
        let task =
            tokio::spawn(async move { q.run("s1", "workspace.list", json!({"path": "."})).await });
        assert_eq!(
            runner.claim("s2", 1),
            None,
            "another Space's runner sees nothing"
        );
        let job = claim_wait(&runner, "s1", 1).await;
        assert_eq!(job.tool, "workspace.list");
        assert_eq!(
            runner.complete("s2", 1, &job.id, ok("x")),
            Err(CompleteError::Unknown)
        );
        runner.complete("s1", 1, &job.id, ok("a\nb")).unwrap();
        assert_eq!(
            runner.complete("s1", 1, &job.id, ok("again")),
            Err(CompleteError::Duplicate)
        );
        assert_eq!(
            runner.complete("s2", 1, &job.id, ok("again")),
            Err(CompleteError::Unknown)
        );
        assert_eq!(task.await.unwrap().unwrap().output, "a\nb");
        assert_eq!(runner.pending(), 0);
    }

    #[tokio::test]
    async fn rotation_fails_claimed_jobs_and_keeps_queued_ones() {
        let q = JobQueue::new(4, Duration::from_secs(5), Duration::from_secs(5));
        let a = tokio::spawn({
            let q = q.clone();
            async move { q.run("s", "t", json!({"n": 1})).await }
        });
        let claimed = claim_wait(&q, "s", 1).await;
        let b = tokio::spawn({
            let q = q.clone();
            async move { q.run("s", "t", json!({"n": 2})).await }
        });
        while q.pending() < 2 {
            tokio::task::yield_now().await;
        }
        q.rotate("s", 2);
        assert_eq!(a.await.unwrap(), Err(JobError::CredentialChanged));
        // The old runner can neither answer nor claim.
        assert_eq!(
            q.complete("s", 1, &claimed.id, ok("late")),
            Err(CompleteError::Expired)
        );
        assert_eq!(q.claim("s", 1), None);
        // The new runner gets the queued job.
        let next = q.claim("s", 2).unwrap();
        assert_eq!(next.arguments, json!({"n": 2}));
        q.complete("s", 2, &next.id, ok("fresh")).unwrap();
        assert_eq!(b.await.unwrap().unwrap().output, "fresh");
    }

    #[tokio::test]
    async fn revocation_fails_everything() {
        let q = JobQueue::new(4, Duration::from_secs(5), Duration::from_secs(5));
        let a = tokio::spawn({
            let q = q.clone();
            async move { q.run("s", "t", json!({})).await }
        });
        while q.pending() < 1 {
            tokio::task::yield_now().await;
        }
        q.revoke("s", 3);
        assert_eq!(a.await.unwrap(), Err(JobError::CredentialChanged));
        assert_eq!(q.pending(), 0);
    }

    #[tokio::test]
    async fn unclaimed_job_fails_and_is_withdrawn() {
        let q = queue();
        assert_eq!(
            q.run("s", "workspace.check", json!({})).await,
            Err(JobError::NotClaimed)
        );
        assert_eq!(q.pending(), 0);
        assert_eq!(q.claim("s", 1), None);
    }

    #[tokio::test]
    async fn late_result_is_expired() {
        let q = queue();
        let runner = q.clone();
        let task = tokio::spawn(async move { q.run("s", "workspace.check", json!({})).await });
        let job = claim_wait(&runner, "s", 1).await;
        assert_eq!(task.await.unwrap(), Err(JobError::TimedOut));
        assert_eq!(
            runner.complete("s", 1, &job.id, ok("")),
            Err(CompleteError::Expired)
        );
        assert_eq!(
            runner.complete("s", 1, "nope", ok("")),
            Err(CompleteError::Unknown)
        );
    }

    #[tokio::test]
    async fn queue_is_bounded_per_space_and_abandoned_jobs_leave() {
        let q = queue();
        let spawn = |space: &'static str| {
            let q = q.clone();
            tokio::spawn(async move { q.run(space, "t", json!({})).await })
        };
        let a = spawn("s");
        let b = spawn("s");
        while q.pending() < 2 {
            tokio::task::yield_now().await;
        }
        assert_eq!(q.run("s", "t", json!({})).await, Err(JobError::QueueFull));
        // Another Space has its own capacity.
        let c = spawn("other");
        while q.pending() < 3 {
            tokio::task::yield_now().await;
        }
        for t in [a, b, c] {
            t.abort();
            let _ = t.await;
        }
        assert_eq!(q.pending(), 0);
        assert_eq!(q.claim("s", 1), None);
    }
}
