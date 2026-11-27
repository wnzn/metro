//! Bounded job queues between the server and the runner serving each Space.
//!
//! Every Space has its own queue and a *runner generation* (stored in the
//! database) that goes up whenever who may serve the Space changes: a
//! dedicated token is issued, rotated or revoked, or a registered runner is
//! attached, detached, reassigned, rotated or deleted. A runner's credential
//! resolves, on every request, to the Spaces it may serve and each one's
//! current generation. A runner only ever claims jobs of those Spaces, and a
//! result is accepted only from the same claimer, for a Space it still
//! serves, at the generation the job was claimed under, so a stale or foreign
//! runner can never answer a job, and nothing in flight crosses to a new
//! attachment.
//!
//! A registered runner may serve several Spaces; its claim takes the oldest
//! queued job across all of them, so no Space starves another.
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
    /// The Space the job belongs to (runner protocol 2 only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
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
    /// The Space's runner changed (token rotated or revoked, runner detached,
    /// reassigned or deleted) while the job was pending.
    CredentialChanged,
}

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            JobError::QueueFull => "the runner queue is full; try again shortly",
            JobError::NotClaimed => "no runner picked up the job in time; is the runner connected?",
            JobError::TimedOut => "the runner did not return a result in time",
            JobError::CredentialChanged => {
                "the Space's runner changed (token rotated or revoked, or runner detached, \
                 reassigned or deleted); the job was cancelled"
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
    Claimed {
        at: Instant,
        generation: u64,
        claimer: String,
    },
}

struct Job {
    space: String,
    /// Order of arrival across all Spaces.
    seq: u64,
    /// The Space's runner generation when the job was queued.
    generation: u64,
    tool: String,
    arguments: serde_json::Value,
    /// How long the runner has after claiming it.
    lease: Duration,
    state: State,
    tx: oneshot::Sender<Result<JobOutcome, JobError>>,
}

#[derive(Default)]
struct SpaceQueue {
    queue: VecDeque<String>,
    /// Highest credential generation seen for this Space.
    generation: u64,
    drop_before: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct RunnerVersion {
    pub generation: u64,
    pub drop_before: u64,
}

#[derive(Default)]
struct Inner {
    spaces: HashMap<String, SpaceQueue>,
    jobs: HashMap<String, Job>,
    finished: HashMap<String, (String, Finished)>,
    finished_order: VecDeque<String>,
    next_seq: u64,
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

    /// Note that `space` now uses runner `generation`. Returns false when
    /// the caller holds an older generation than one already seen. A newer
    /// generation fails older claims; the durable assignment floor fences
    /// older queued jobs independently of credential rotation or HTTP order.
    fn observe(&mut self, space: &str, version: RunnerVersion) -> bool {
        let q = self.spaces.entry(space.to_owned()).or_default();
        let generation = q.generation.max(version.generation);
        let floor = q.drop_before.max(version.drop_before);
        let changed = generation != q.generation || floor != q.drop_before;
        q.generation = generation;
        q.drop_before = floor;
        if changed {
            let doomed: Vec<String> = self
                .jobs
                .iter()
                .filter(|(_, job)| job.space == space)
                .filter(|(_, job)| match job.state {
                    State::Claimed { generation: g, .. } => g < generation,
                    State::Queued => job.generation < floor,
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in doomed {
                self.fail(&id, Some(JobError::CredentialChanged));
            }
        }
        version.generation == generation
    }
}

#[derive(Clone)]
pub struct JobQueue {
    inner: Arc<Mutex<Inner>>,
    capacity: usize,
    claim_timeout: Duration,
    run_timeout: Duration,
    /// Space ids whose runner assignment changed ([`JobQueue::revoke`]).
    revoked: tokio::sync::broadcast::Sender<String>,
}

impl JobQueue {
    /// `capacity` bounds the queued jobs of each Space.
    pub fn new(capacity: usize, claim_timeout: Duration, run_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            capacity,
            claim_timeout,
            run_timeout,
            revoked: tokio::sync::broadcast::channel(64).0,
        }
    }

    /// Space ids, as their runner assignment changes. A lagging receiver
    /// should treat a lag as a change.
    pub fn subscribe_revoked(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.revoked.subscribe()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Insert synchronously under the database's assignment fence, then
    /// return a waiter that abandons the job if dropped, even before polling.
    #[cfg(test)]
    pub(crate) fn enqueue_at(
        &self,
        space: &str,
        version: RunnerVersion,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<impl Future<Output = Result<JobOutcome, JobError>> + Send + use<>, JobError> {
        self.enqueue_leased(space, version, tool, arguments, None)
    }

    /// Insert synchronously under the database's assignment fence, then
    /// return a waiter that abandons the job if dropped, even before polling.
    /// `lease` extends the default run timeout (a long shell command).
    pub(crate) fn enqueue_leased(
        &self,
        space: &str,
        version: RunnerVersion,
        tool: &str,
        arguments: serde_json::Value,
        lease: Option<Duration>,
    ) -> Result<impl Future<Output = Result<JobOutcome, JobError>> + Send + use<>, JobError> {
        let lease = lease.map_or(self.run_timeout, |l| l.max(self.run_timeout));
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, mut rx) = oneshot::channel();
        {
            let mut inner = self.lock();
            if !inner.observe(space, version) {
                return Err(JobError::CredentialChanged);
            }
            let seq = inner.next_seq;
            inner.next_seq += 1;
            let q = inner.spaces.entry(space.to_owned()).or_default();
            if q.queue.len() >= self.capacity {
                return Err(JobError::QueueFull);
            }
            q.queue.push_back(id.clone());
            inner.jobs.insert(
                id.clone(),
                Job {
                    space: space.to_owned(),
                    seq,
                    generation: version.generation,
                    tool: tool.to_owned(),
                    arguments,
                    lease,
                    state: State::Queued,
                    tx,
                },
            );
        }
        let claim_deadline = Instant::now() + self.claim_timeout;
        let guard = AbandonGuard {
            queue: self.clone(),
            id,
        };
        Ok(async move {
            let guard = guard;
            if let Ok(result) = tokio::time::timeout_at(claim_deadline, &mut rx).await {
                return result.unwrap_or(Err(JobError::TimedOut));
            }
            // Not answered before the claim deadline: fail it if still
            // queued, otherwise give it the rest of its run time.
            let run_deadline = {
                let mut inner = guard.queue.lock();
                match inner.jobs.get(&guard.id).map(|job| (&job.state, job.lease)) {
                    Some((State::Queued, _)) => {
                        inner.fail(&guard.id, None);
                        return Err(JobError::NotClaimed);
                    }
                    Some((State::Claimed { at, .. }, lease)) => *at + lease,
                    // Finished while we took the lock; the result is in `rx`.
                    None => Instant::now(),
                }
            };
            match tokio::time::timeout_at(run_deadline, &mut rx).await {
                Ok(result) => result.unwrap_or(Err(JobError::TimedOut)),
                Err(_) => rx.try_recv().unwrap_or(Err(JobError::TimedOut)),
            }
        })
    }

    #[cfg(test)]
    async fn run_at(
        &self,
        space: &str,
        version: RunnerVersion,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<JobOutcome, JobError> {
        self.enqueue_at(space, version, tool, arguments)?.await
    }

    /// The oldest queued job across `spaces` (each with the runner
    /// generation the caller holds for it), now claimed by `claimer`. `None`
    /// when there is none; Spaces whose generation has been superseded are
    /// skipped.
    pub(crate) fn claim_at(
        &self,
        claimer: &str,
        spaces: &[(String, RunnerVersion)],
    ) -> Option<ClaimedJob> {
        let mut inner = self.lock();
        let mut best: Option<(u64, String, u64)> = None;
        for (space, version) in spaces {
            if !inner.observe(space, *version) {
                continue;
            }
            let inner = &mut *inner;
            let Some(q) = inner.spaces.get_mut(space) else {
                continue;
            };
            // Drop ids of jobs that already left (abandoned or failed).
            while let Some(id) = q.queue.front() {
                if inner.jobs.contains_key(id) {
                    break;
                }
                q.queue.pop_front();
            }
            if let Some(job) = q.queue.front().and_then(|id| inner.jobs.get(id))
                && best.as_ref().is_none_or(|(seq, _, _)| job.seq < *seq)
            {
                best = Some((job.seq, space.clone(), version.generation));
            }
        }
        let (_, space, generation) = best?;
        let id = inner.spaces.get_mut(&space)?.queue.pop_front()?;
        let job = inner.jobs.get_mut(&id)?;
        job.state = State::Claimed {
            at: Instant::now(),
            generation,
            claimer: claimer.to_owned(),
        };
        Some(ClaimedJob {
            id,
            tool: job.tool.clone(),
            arguments: job.arguments.clone(),
            space_id: Some(space),
        })
    }

    /// Accept a result from `claimer`, which may serve `spaces` (each with
    /// the runner generation it holds). A job of any other Space is
    /// indistinguishable from an unknown one.
    pub(crate) fn complete_at(
        &self,
        claimer: &str,
        spaces: &[(String, RunnerVersion)],
        id: &str,
        outcome: JobOutcome,
    ) -> Result<(), CompleteError> {
        let mut inner = self.lock();
        let served = |space: &str| {
            spaces
                .iter()
                .find(|(s, _)| s == space)
                .map(|(s, g)| (s.clone(), *g))
        };
        let space = match inner.jobs.get(id) {
            Some(job) => job.space.clone(),
            None => {
                return Err(match inner.finished.get(id) {
                    Some((s, _)) if served(s).is_none() => CompleteError::Unknown,
                    Some((_, Finished::Completed)) => CompleteError::Duplicate,
                    Some((_, Finished::Expired)) => CompleteError::Expired,
                    None => CompleteError::Unknown,
                });
            }
        };
        let Some((space, version)) = served(&space) else {
            return Err(CompleteError::Unknown);
        };
        if !inner.observe(&space, version) {
            return Err(CompleteError::Expired);
        }
        // `observe` may have failed the job just now.
        let Some(job) = inner.jobs.get(id) else {
            return Err(CompleteError::Expired);
        };
        match &job.state {
            State::Queued => return Err(CompleteError::NotClaimed),
            State::Claimed { claimer: c, .. } if c != claimer => {
                return Err(CompleteError::Unknown);
            }
            State::Claimed { generation: g, .. } if *g != version.generation => {
                inner.fail(id, Some(JobError::CredentialChanged));
                return Err(CompleteError::Expired);
            }
            State::Claimed { at, .. } if at.elapsed() > job.lease => {
                inner.fail(id, Some(JobError::TimedOut));
                return Err(CompleteError::Expired);
            }
            State::Claimed { .. } => {}
        }
        let job = inner.jobs.remove(id).expect("checked above");
        inner.finish(id, &space, Finished::Completed);
        drop(inner);
        // The waiter may have just given up; the result then goes nowhere.
        let _ = job.tx.send(Ok(outcome));
        Ok(())
    }

    /// Whether `claimer` should keep working on job `id`: `Ok` while it is
    /// claimed by `claimer` at a generation it still serves and within its
    /// lease. A finished or failed job (run stopped, lease over, runner
    /// changed) is [`CompleteError::Expired`].
    pub(crate) fn heartbeat_at(
        &self,
        claimer: &str,
        spaces: &[(String, RunnerVersion)],
        id: &str,
    ) -> Result<(), CompleteError> {
        let mut inner = self.lock();
        let space = match inner.jobs.get(id) {
            Some(job) => job.space.clone(),
            None => {
                return Err(match inner.finished.get(id) {
                    Some((s, _)) if spaces.iter().any(|(x, _)| x == s) => CompleteError::Expired,
                    _ => CompleteError::Unknown,
                });
            }
        };
        let Some(version) = spaces.iter().find(|(s, _)| *s == space).map(|(_, v)| *v) else {
            return Err(CompleteError::Unknown);
        };
        if !inner.observe(&space, version) {
            return Err(CompleteError::Expired);
        }
        let Some(job) = inner.jobs.get(id) else {
            return Err(CompleteError::Expired);
        };
        match &job.state {
            State::Queued => Err(CompleteError::NotClaimed),
            State::Claimed { claimer: c, .. } if c != claimer => Err(CompleteError::Unknown),
            State::Claimed { generation: g, .. } if *g != version.generation => {
                Err(CompleteError::Expired)
            }
            State::Claimed { at, .. } if at.elapsed() > job.lease => Err(CompleteError::Expired),
            State::Claimed { .. } => Ok(()),
        }
    }

    /// The same runner's token was rotated (`generation` is the Space's new
    /// generation): jobs claimed under the old token fail; queued jobs wait
    /// for the runner holding the new token.
    pub fn rotate(&self, space: &str, generation: u64) {
        self.lock().observe(
            space,
            RunnerVersion {
                generation,
                drop_before: 0,
            },
        );
    }

    /// Who serves the Space changed (token revoked; runner attached,
    /// detached, reassigned or deleted; `generation` is the new one): every
    /// job queued or claimed before fails.
    pub fn revoke(&self, space: &str, generation: u64) {
        self.lock().observe(
            space,
            RunnerVersion {
                generation,
                drop_before: generation,
            },
        );
        let _ = self.revoked.send(space.to_owned());
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        self.lock().jobs.len()
    }

    #[cfg(test)]
    async fn run(
        &self,
        space: &str,
        generation: u64,
        tool: &str,
        arguments: serde_json::Value,
    ) -> Result<JobOutcome, JobError> {
        self.run_at(
            space,
            RunnerVersion {
                generation,
                drop_before: 0,
            },
            tool,
            arguments,
        )
        .await
    }

    #[cfg(test)]
    fn claim(&self, claimer: &str, spaces: &[(String, u64)]) -> Option<ClaimedJob> {
        self.claim_at(claimer, &test_versions(spaces))
    }

    #[cfg(test)]
    fn complete(
        &self,
        claimer: &str,
        spaces: &[(String, u64)],
        id: &str,
        outcome: JobOutcome,
    ) -> Result<(), CompleteError> {
        self.complete_at(claimer, &test_versions(spaces), id, outcome)
    }
}

#[cfg(test)]
fn test_versions(spaces: &[(String, u64)]) -> Vec<(String, RunnerVersion)> {
    spaces
        .iter()
        .map(|(s, g)| {
            (
                s.clone(),
                RunnerVersion {
                    generation: *g,
                    drop_before: 0,
                },
            )
        })
        .collect()
}

/// Withdraws an unfinished job when its waiter goes away.
struct AbandonGuard {
    queue: JobQueue,
    id: String,
}

impl Drop for AbandonGuard {
    fn drop(&mut self) {
        self.queue.lock().fail(&self.id, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const D: &str = "dedicated";

    fn queue() -> JobQueue {
        JobQueue::new(2, Duration::from_millis(200), Duration::from_millis(300))
    }

    fn sp(space: &str, generation: u64) -> Vec<(String, u64)> {
        vec![(space.to_owned(), generation)]
    }

    async fn claim_wait(q: &JobQueue, who: &str, spaces: &[(String, u64)]) -> ClaimedJob {
        loop {
            if let Some(job) = q.claim(who, spaces) {
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
            tokio::spawn(
                async move { q.run("s1", 1, "workspace.list", json!({"path": "."})).await },
            );
        assert_eq!(
            runner.claim(D, &sp("s2", 1)),
            None,
            "another Space's runner sees nothing"
        );
        let job = claim_wait(&runner, D, &sp("s1", 1)).await;
        assert_eq!(job.tool, "workspace.list");
        assert_eq!(job.space_id.as_deref(), Some("s1"));
        assert_eq!(
            runner.complete(D, &sp("s2", 1), &job.id, ok("x")),
            Err(CompleteError::Unknown)
        );
        assert_eq!(
            runner.complete("runner:other", &sp("s1", 1), &job.id, ok("x")),
            Err(CompleteError::Unknown),
            "another claimer cannot answer"
        );
        runner
            .complete(D, &sp("s1", 1), &job.id, ok("a\nb"))
            .unwrap();
        assert_eq!(
            runner.complete(D, &sp("s1", 1), &job.id, ok("again")),
            Err(CompleteError::Duplicate)
        );
        assert_eq!(
            runner.complete(D, &sp("s2", 1), &job.id, ok("again")),
            Err(CompleteError::Unknown)
        );
        assert_eq!(task.await.unwrap().unwrap().output, "a\nb");
        assert_eq!(runner.pending(), 0);
    }

    #[tokio::test]
    async fn one_runner_serves_several_spaces_oldest_first() {
        let q = JobQueue::new(8, Duration::from_secs(5), Duration::from_secs(5));
        let spawn = |space: &'static str, n: u64| {
            let q = q.clone();
            tokio::spawn(async move { q.run(space, 1, "t", json!({"n": n})).await })
        };
        // Two jobs in a busy Space, then one in a quiet one, then another busy one.
        let a1 = spawn("busy", 1);
        while q.pending() < 1 {
            tokio::task::yield_now().await;
        }
        let a2 = spawn("busy", 2);
        while q.pending() < 2 {
            tokio::task::yield_now().await;
        }
        let b1 = spawn("quiet", 3);
        while q.pending() < 3 {
            tokio::task::yield_now().await;
        }
        let both = vec![("busy".to_owned(), 1), ("quiet".to_owned(), 1)];
        let r = "runner:r1";
        let order: Vec<u64> = (0..3)
            .map(|_| {
                let job = q.claim(r, &both).unwrap();
                q.complete(r, &both, &job.id, ok("")).unwrap();
                job.arguments["n"].as_u64().unwrap()
            })
            .collect();
        assert_eq!(order, vec![1, 2, 3], "arrival order across Spaces");
        for t in [a1, a2, b1] {
            t.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn rotation_fails_claimed_jobs_and_keeps_queued_ones() {
        let q = JobQueue::new(4, Duration::from_secs(5), Duration::from_secs(5));
        let a = tokio::spawn({
            let q = q.clone();
            async move { q.run("s", 1, "t", json!({"n": 1})).await }
        });
        let claimed = claim_wait(&q, D, &sp("s", 1)).await;
        let b = tokio::spawn({
            let q = q.clone();
            async move { q.run("s", 1, "t", json!({"n": 2})).await }
        });
        while q.pending() < 2 {
            tokio::task::yield_now().await;
        }
        q.rotate("s", 2);
        assert_eq!(a.await.unwrap(), Err(JobError::CredentialChanged));
        // The old runner can neither answer nor claim.
        assert_eq!(
            q.complete(D, &sp("s", 1), &claimed.id, ok("late")),
            Err(CompleteError::Expired)
        );
        assert_eq!(q.claim(D, &sp("s", 1)), None);
        // The new runner gets the queued job.
        let next = q.claim(D, &sp("s", 2)).unwrap();
        assert_eq!(next.arguments, json!({"n": 2}));
        q.complete(D, &sp("s", 2), &next.id, ok("fresh")).unwrap();
        assert_eq!(b.await.unwrap().unwrap().output, "fresh");
    }

    #[tokio::test]
    async fn committed_rotation_refuses_old_claims_and_results_before_notification() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let sid = db.create_space("alice", "A", 4).unwrap().id;
        let q = JobQueue::new(4, Duration::from_secs(5), Duration::from_secs(5));
        let old = db.set_runner_token(&sid, b"old").unwrap().unwrap();
        let submit = |n| {
            db.with_runner_queue_version(&sid, |v| q.enqueue_at(&sid, v, "t", json!(n)))
                .unwrap()
                .unwrap()
                .unwrap()
        };
        let dropped = submit(0);
        assert_eq!(q.pending(), 1);
        drop(dropped);
        assert_eq!(q.pending(), 0);
        let claimed = submit(1);
        let queued = submit(2);
        let old_access = [(sid.clone(), old)];
        let first = db
            .with_runner_queue_access(&old_access, |spaces| q.claim_at(D, spaces))
            .unwrap()
            .unwrap();
        let new = db.set_runner_token(&sid, b"new").unwrap().unwrap();
        // No queue notification from the rotation route has run.
        db.with_runner_queue_access(&old_access, |spaces| {
            assert!(q.claim_at(D, spaces).is_none());
            assert_eq!(
                q.complete_at(D, spaces, &first.id, ok("old")),
                Err(CompleteError::Unknown)
            );
        })
        .unwrap();
        let new_access = [(sid, new)];
        db.with_runner_queue_access(&new_access, |spaces| {
            let second = q.claim_at(D, spaces).unwrap();
            assert_eq!(second.arguments, json!(2));
            q.complete_at(D, spaces, &second.id, ok("new")).unwrap();
        })
        .unwrap();
        assert_eq!(claimed.await, Err(JobError::CredentialChanged));
        assert_eq!(queued.await, Ok(ok("new")));
    }

    #[tokio::test]
    async fn committed_detach_fences_before_delayed_revoke_and_keeps_other_spaces() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let a = db.create_space("alice", "A", 4).unwrap().id;
        let b = db.create_space("alice", "B", 4).unwrap().id;
        let q = JobQueue::new(4, Duration::from_secs(5), Duration::from_secs(5));
        db.set_runner_token(&a, b"a-old").unwrap();
        db.set_runner_token(&b, b"b-current").unwrap();
        let old = db.runner_queue_version(&a).unwrap().unwrap();
        let sibling = db.runner_queue_version(&b).unwrap().unwrap();
        let spawn = |sid: &str, version, n| {
            let q = q.clone();
            let sid = sid.to_owned();
            tokio::spawn(async move { q.run_at(&sid, version, "t", json!({"n": n})).await })
        };
        let left = spawn(&a, old, 1);
        let right = spawn(&b, sibling, 2);
        while q.pending() < 2 {
            tokio::task::yield_now().await;
        }

        // The detach commits but its HTTP task hasn't notified the queue.
        let detached = db.detach_runner(&a).unwrap().unwrap();
        db.set_runner_token(&a, b"a-new").unwrap();
        let rotated = db.set_runner_token(&a, b"a-rotated").unwrap().unwrap();
        q.rotate(&a, rotated); // A later handler notification arrives first.
        let claim = |hash| {
            let auth = db.runner_by_token_hash(hash).unwrap().unwrap();
            db.with_runner_queue_access(&[auth], |spaces| q.claim_at(D, spaces))
                .unwrap()
        };
        assert!(claim(b"a-rotated").is_none());
        assert_eq!(left.await.unwrap(), Err(JobError::CredentialChanged));
        assert_eq!(
            q.run_at(&a, old, "t", json!({})).await,
            Err(JobError::CredentialChanged)
        );
        let bjob = claim(b"b-current").unwrap();
        assert_eq!(bjob.arguments, json!({"n": 2}));

        let after = db.runner_queue_version(&a).unwrap().unwrap();
        let fresh = spawn(&a, after, 3);
        while q.pending() < 2 {
            tokio::task::yield_now().await;
        }
        q.revoke(&a, detached); // Late revocation can't consume post-detach work.
        let newest = db
            .set_runner_token(&a, b"a-rotated-again")
            .unwrap()
            .unwrap();
        let ajob = claim(b"a-rotated-again").unwrap(); // Pure rotation kept it.
        assert_eq!(ajob.arguments, json!({"n": 3}));
        for (sid, generation, job) in [(&a, newest, ajob), (&b, sibling.generation, bjob)] {
            db.with_runner_queue_access(&[(sid.clone(), generation)], |spaces| {
                q.complete_at(D, spaces, &job.id, ok("done"))
            })
            .unwrap()
            .unwrap();
        }
        assert!(fresh.await.unwrap().is_ok());
        assert!(right.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn revocation_fails_everything() {
        let q = JobQueue::new(4, Duration::from_secs(5), Duration::from_secs(5));
        let a = tokio::spawn({
            let q = q.clone();
            async move { q.run("s", 0, "t", json!({})).await }
        });
        while q.pending() < 1 {
            tokio::task::yield_now().await;
        }
        q.revoke("s", 3);
        assert_eq!(a.await.unwrap(), Err(JobError::CredentialChanged));
        assert_eq!(q.pending(), 0);
    }

    #[tokio::test]
    async fn stale_generation_cannot_queue_and_reattach_keeps_new_jobs() {
        let q = JobQueue::new(4, Duration::from_secs(5), Duration::from_secs(5));
        q.revoke("s", 5);
        // Read before the change: refused, so it never reaches the new runner.
        assert_eq!(
            q.run("s", 4, "t", json!({})).await,
            Err(JobError::CredentialChanged)
        );
        // Queued at the new generation; a repeated revoke at the same
        // generation leaves it alone.
        let a = tokio::spawn({
            let q = q.clone();
            async move { q.run("s", 5, "t", json!({})).await }
        });
        while q.pending() < 1 {
            tokio::task::yield_now().await;
        }
        q.revoke("s", 5);
        let job = q.claim("runner:b", &sp("s", 5)).unwrap();
        q.complete("runner:b", &sp("s", 5), &job.id, ok("b"))
            .unwrap();
        assert_eq!(a.await.unwrap().unwrap().output, "b");
    }

    #[tokio::test]
    async fn detached_runner_late_result_is_refused() {
        let q = JobQueue::new(4, Duration::from_secs(5), Duration::from_secs(5));
        let r = "runner:a";
        let a = tokio::spawn({
            let q = q.clone();
            async move { q.run("s", 1, "t", json!({})).await }
        });
        let job = claim_wait(&q, r, &sp("s", 1)).await;
        q.revoke("s", 2);
        assert_eq!(a.await.unwrap(), Err(JobError::CredentialChanged));
        // The runner no longer serves the Space at all.
        assert_eq!(
            q.complete(r, &[], &job.id, ok("late")),
            Err(CompleteError::Unknown)
        );
        // Or it still lists the Space with its old generation.
        assert_eq!(
            q.complete(r, &sp("s", 1), &job.id, ok("late")),
            Err(CompleteError::Expired)
        );
    }

    #[tokio::test]
    async fn a_long_lease_outlives_the_default_and_the_heartbeat_follows_the_job() {
        // Default run timeout 300 ms; this job may take 2 s.
        let q = queue();
        let v = RunnerVersion {
            generation: 1,
            drop_before: 0,
        };
        let waiter = q
            .enqueue_leased(
                "s",
                v,
                "workspace.shell",
                json!({}),
                Some(Duration::from_secs(2)),
            )
            .unwrap();
        let task = tokio::spawn(waiter);
        let job = claim_wait(&q, D, &sp("s", 1)).await;
        let beat = |who: &str, spaces: &[(String, u64)]| {
            q.heartbeat_at(who, &test_versions(spaces), &job.id)
        };
        assert_eq!(beat(D, &sp("s", 1)), Ok(()));
        assert_eq!(beat("runner:x", &sp("s", 1)), Err(CompleteError::Unknown));
        assert_eq!(beat(D, &sp("other", 1)), Err(CompleteError::Unknown));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(beat(D, &sp("s", 1)), Ok(()), "still inside its own lease");
        q.complete(D, &sp("s", 1), &job.id, ok("done")).unwrap();
        assert_eq!(task.await.unwrap().unwrap().output, "done");
        assert_eq!(beat(D, &sp("s", 1)), Err(CompleteError::Expired));
        assert_eq!(beat(D, &[]), Err(CompleteError::Unknown));
    }

    #[tokio::test]
    async fn abandoning_or_revoking_tells_the_heartbeat_to_stop() {
        let q = JobQueue::new(4, Duration::from_secs(5), Duration::from_secs(5));
        let mut revoked = q.subscribe_revoked();
        let task = tokio::spawn({
            let q = q.clone();
            async move { q.run("s", 1, "workspace.shell", json!({})).await }
        });
        let job = claim_wait(&q, D, &sp("s", 1)).await;
        // The run stopped: its waiter is dropped.
        task.abort();
        let _ = task.await;
        assert_eq!(
            q.heartbeat_at(D, &test_versions(&sp("s", 1)), &job.id),
            Err(CompleteError::Expired)
        );
        let task = tokio::spawn({
            let q = q.clone();
            async move { q.run("s", 1, "workspace.shell", json!({})).await }
        });
        let job = claim_wait(&q, D, &sp("s", 1)).await;
        q.revoke("s", 2);
        assert_eq!(revoked.recv().await.unwrap(), "s");
        assert_eq!(task.await.unwrap(), Err(JobError::CredentialChanged));
        assert_eq!(
            q.heartbeat_at(D, &test_versions(&sp("s", 2)), &job.id),
            Err(CompleteError::Expired)
        );
    }

    #[tokio::test]
    async fn unclaimed_job_fails_and_is_withdrawn() {
        let q = queue();
        assert_eq!(
            q.run("s", 1, "workspace.check", json!({})).await,
            Err(JobError::NotClaimed)
        );
        assert_eq!(q.pending(), 0);
        assert_eq!(q.claim(D, &sp("s", 1)), None);
    }

    #[tokio::test]
    async fn late_result_is_expired() {
        let q = queue();
        let runner = q.clone();
        let task = tokio::spawn(async move { q.run("s", 1, "workspace.check", json!({})).await });
        let job = claim_wait(&runner, D, &sp("s", 1)).await;
        assert_eq!(task.await.unwrap(), Err(JobError::TimedOut));
        assert_eq!(
            runner.complete(D, &sp("s", 1), &job.id, ok("")),
            Err(CompleteError::Expired)
        );
        assert_eq!(
            runner.complete(D, &sp("s", 1), "nope", ok("")),
            Err(CompleteError::Unknown)
        );
    }

    #[tokio::test]
    async fn queue_is_bounded_per_space_and_abandoned_jobs_leave() {
        let q = queue();
        let spawn = |space: &'static str| {
            let q = q.clone();
            tokio::spawn(async move { q.run(space, 1, "t", json!({})).await })
        };
        let a = spawn("s");
        let b = spawn("s");
        while q.pending() < 2 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            q.run("s", 1, "t", json!({})).await,
            Err(JobError::QueueFull)
        );
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
        assert_eq!(q.claim(D, &sp("s", 1)), None);
    }
}
