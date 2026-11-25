//! Scheduler tests with an injected wall clock and a fake submitter. No
//! model is called and nothing waits for a real tick.

use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};

use super::*;
use crate::auth::{AuthFailure, AuthFuture, HumanAuth};
use crate::db::Db;
use crate::{AppState, Config, Limits};

struct NoAuth;

impl HumanAuth for NoAuth {
    fn identity<'a>(&'a self, _bearer: &'a str) -> AuthFuture<'a> {
        Box::pin(async { Err(AuthFailure::Rejected("tests".to_owned())) })
    }
    fn namespace(&self) -> &str {
        "dev"
    }
}

fn config() -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        db_path: ":memory:".into(),
        model: None,
        limits: Limits::default(),
        org_admins: Vec::new(),
    }
}

async fn state_with(db: Db) -> AppState {
    init(&db.lock()).unwrap();
    AppState::new(config(), db, Arc::new(NoAuth)).await.unwrap()
}

async fn state() -> AppState {
    state_with(Db::open_in_memory().unwrap()).await
}

#[derive(Clone, Copy)]
enum Mode {
    Ok,
    Fail,
    Hang,
}

type Call = (String, Option<(String, String)>, String);

struct Fake {
    mode: Mutex<Mode>,
    calls: Mutex<Vec<Call>>,
    release: Arc<tokio::sync::Notify>,
}

impl Fake {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode: Mutex::new(mode),
            calls: Mutex::new(Vec::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        })
    }
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

impl Submitter for Fake {
    fn submit(
        &self,
        _state: AppState,
        owner_id: String,
        space: Option<(String, String)>,
        prompt: String,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>> {
        self.calls.lock().unwrap().push((owner_id, space, prompt));
        let mode = *self.mode.lock().unwrap();
        let release = self.release.clone();
        Box::pin(async move {
            match mode {
                Mode::Ok => Ok(()),
                Mode::Fail => anyhow::bail!("alice's device connector is offline"),
                Mode::Hang => {
                    release.notified().await;
                    Ok(())
                }
            }
        })
    }
}

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

const NOW: &str = "2026-11-22T02:00:00Z"; // 10:00 in Manila

fn once(at: &str) -> ScheduleIn {
    ScheduleIn {
        kind: "once".into(),
        at: Some(at.into()),
        expression: None,
        timezone: None,
    }
}

fn cron(expr: &str, tz: &str) -> ScheduleIn {
    ScheduleIn {
        kind: "cron".into(),
        at: None,
        expression: Some(expr.into()),
        timezone: Some(tz.into()),
    }
}

fn body(schedule: ScheduleIn) -> CreateBody {
    CreateBody {
        name: "Standup".into(),
        prompt: "Summarize yesterday".into(),
        space_id: None,
        room_id: None,
        schedule,
        enabled: true,
    }
}

fn create(
    st: &AppState,
    owner: &str,
    b: CreateBody,
    now: DateTime<Utc>,
) -> Result<Task, ScheduleError> {
    create_task(&st.db, owner, b, now, 8_000)
}

async fn tick(st: &AppState, now: DateTime<Utc>, fake: &Arc<Fake>) -> TickReport {
    tick_inner(st, now, fake.as_ref(), StdDuration::from_millis(200))
        .await
        .unwrap()
}

fn runs(st: &AppState, owner: &str, id: &str) -> Vec<RunView> {
    task_history(&st.db, owner, id).unwrap()
}

fn get(st: &AppState, id: &str) -> Task {
    get_task(&st.db.lock(), id).unwrap().unwrap()
}

#[tokio::test]
async fn once_fires_exactly_once_a_few_seconds_ahead() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let now = t(NOW);
    let task = create(&st, "alice", body(once("2026-11-22T10:00:06+08:00")), now).unwrap();
    assert_eq!(task.next_run_at, Some(t("2026-11-22T02:00:06Z")));
    assert_eq!(task.view()["schedule"]["at"], "2026-11-22T02:00:06Z");
    assert_eq!(
        earliest_due(&st.db).unwrap(),
        Some(t("2026-11-22T02:00:06Z"))
    );

    assert_eq!(
        tick(&st, now + Duration::seconds(5), &fake).await,
        TickReport::default()
    );
    let r = tick(&st, now + Duration::seconds(6), &fake).await;
    assert_eq!(r.dispatched, 1);
    let r = tick(&st, now + Duration::seconds(7), &fake).await;
    assert_eq!(r, TickReport::default());
    assert_eq!(
        fake.calls(),
        vec![("alice".into(), None, "Summarize yesterday".into())]
    );
    let task = get(&st, &task.id);
    assert_eq!(task.next_run_at, None);
    assert_eq!(task.last_status.as_deref(), Some(status::DISPATCHED));
    let h = runs(&st, "alice", &task.id);
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].status, status::DISPATCHED);
    assert_eq!(h[0].due_at, "2026-11-22T02:00:06Z");
}

#[tokio::test]
async fn daily_manila_cron_and_local_display() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let task = create(&st, "alice", body(cron("0 9 * * *", "Asia/Manila")), t(NOW)).unwrap();
    // 10:00 Manila now, so the next 09:00 is tomorrow.
    assert_eq!(task.next_run_at, Some(t("2026-11-23T01:00:00Z")));
    let v = task.view();
    assert_eq!(v["next_run_at"], "2026-11-23T01:00:00Z");
    assert_eq!(v["next_run_local"], "2026-11-23T09:00:00+08:00");
    assert_eq!(v["owner_id"], "alice");
    assert_eq!(v["schedule"]["timezone"], "Asia/Manila");

    let r = tick(&st, t("2026-11-23T01:00:10Z"), &fake).await;
    assert_eq!(r.dispatched, 1);
    assert_eq!(
        get(&st, &task.id).next_run_at,
        Some(t("2026-11-24T01:00:00Z"))
    );
    // A later tick the same day does nothing.
    assert_eq!(
        tick(&st, t("2026-11-23T01:30:00Z"), &fake).await,
        TickReport::default()
    );
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test]
async fn dst_zone_next_run() {
    let st = state().await;
    // New York spring-forward night: 02:30 does not exist, fires at 03:00 EDT.
    let task = create(
        &st,
        "alice",
        body(cron("30 2 * * *", "America/New_York")),
        t("2027-03-13T12:00:00Z"),
    )
    .unwrap();
    assert_eq!(task.next_run_at, Some(t("2027-03-14T07:00:00Z")));
    assert_eq!(task.view()["next_run_local"], "2027-03-14T03:00:00-04:00");
}

#[tokio::test]
async fn validation() {
    let st = state().await;
    let now = t(NOW);
    let bad = |b: CreateBody| match create(&st, "alice", b, now) {
        Err(ScheduleError::BadRequest(m)) => m,
        other => panic!("expected 400, got {other:?}"),
    };
    bad(body(once("2026-11-22T01:59:00Z")));
    bad(body(once("2026-11-22T02:00:00Z")));
    bad(body(once("2026-11-22 10:00")));
    bad(body(once("2026-11-22T10:00:00"))); // no offset
    bad(body(once("2027-11-24T00:00:00Z"))); // more than a year
    assert!(bad(body(cron("*/5 * * * *", "Asia/Manila"))).contains("15 minutes"));
    bad(body(cron("0 9 * * *", "Mars/Olympus")));
    bad(body(cron("0 9 * * *", "+08:00")));
    bad(body(cron("0 0 9 * * *", "Asia/Manila")));
    bad(body(cron("0 0 31 2 *", "UTC")));
    let mut b = body(cron("0 9 * * *", "Asia/Manila"));
    b.schedule.at = Some("2026-11-23T00:00:00Z".into());
    bad(b);
    let mut b = body(cron("0 9 * * *", "Asia/Manila"));
    b.room_id = Some("lobby".into());
    bad(b);
    let mut b = body(cron("0 9 * * *", "Asia/Manila"));
    b.space_id = Some("s1".into());
    bad(b);
    let mut b = body(cron("0 9 * * *", "Asia/Manila"));
    b.name = " ".into();
    bad(b);
    let mut b = body(cron("0 9 * * *", "Asia/Manila"));
    b.prompt = "x".repeat(8_001);
    bad(b);
    // Hourly and every 15 minutes are fine.
    create(&st, "alice", body(cron("*/15 * * * *", "Asia/Manila")), now).unwrap();
    create(&st, "alice", body(cron("@hourly", "UTC")), now).unwrap();
}

#[tokio::test]
async fn per_user_cap() {
    let st = state().await;
    for _ in 0..MAX_TASKS_PER_USER {
        create(&st, "alice", body(cron("0 9 * * *", "UTC")), t(NOW)).unwrap();
    }
    assert!(matches!(
        create(&st, "alice", body(cron("0 9 * * *", "UTC")), t(NOW)),
        Err(ScheduleError::Conflict(_))
    ));
    create(&st, "bob", body(cron("0 9 * * *", "UTC")), t(NOW)).unwrap();
}

#[tokio::test]
async fn owner_only_and_foreign_tasks_are_404() {
    let st = state().await;
    let task = create(&st, "alice", body(cron("0 9 * * *", "UTC")), t(NOW)).unwrap();
    let nf = |r: Result<(), ScheduleError>| assert!(matches!(r, Err(ScheduleError::NotFound(_))));
    nf(update_task(&st.db, "bob", &task.id, PatchBody::default(), t(NOW), 8_000).map(|_| ()));
    nf(delete_task(&st.db, "bob", &task.id));
    nf(task_history(&st.db, "bob", &task.id).map(|_| ()));
    nf(delete_task(&st.db, "alice", "sch_nope"));
    nf(delete_task(&st.db, "alice", "../etc"));
    assert!(list_tasks(&st.db, "bob").unwrap().is_empty());
    assert_eq!(list_tasks(&st.db, "alice").unwrap().len(), 1);
    delete_task(&st.db, "alice", &task.id).unwrap();
    assert!(list_tasks(&st.db, "alice").unwrap().is_empty());
}

fn space(st: &AppState, owner: &str) -> String {
    match st.db.create_space(owner, "Team", 10) {
        Ok(s) => s.id,
        Err(e) => panic!("{e:?}"),
    }
}

#[tokio::test]
async fn space_scope_requires_membership_and_room_in_that_space() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let a = space(&st, "alice");
    let other = space(&st, "carol");
    let thread = match st.db.create_room(&other, "carols-room", None, 50, 10) {
        Ok(r) => r.id,
        Err(e) => panic!("{e:?}"),
    };
    let target = |s: &str, r: &str| {
        let mut b = body(cron("0 9 * * *", "Asia/Manila"));
        b.space_id = Some(s.into());
        b.room_id = Some(r.into());
        b
    };
    // Bob is not a member: 404, as for a missing Space.
    assert!(matches!(
        create(&st, "bob", target(&a, LOBBY), t(NOW)),
        Err(ScheduleError::NotFound("no such Space"))
    ));
    // A room id that exists, but in another Space.
    assert!(matches!(
        create(&st, "alice", target(&a, &thread), t(NOW)),
        Err(ScheduleError::NotFound("no such room"))
    ));
    let task = create(&st, "alice", target(&a, LOBBY), t(NOW)).unwrap();
    assert_eq!(
        tick(&st, t("2026-11-23T01:00:00Z"), &fake).await.dispatched,
        1
    );
    assert_eq!(
        fake.calls(),
        vec![(
            "alice".into(),
            Some((a.clone(), LOBBY.into())),
            "Summarize yesterday".into()
        )]
    );
    assert_eq!(get(&st, &task.id).target, Some((a, LOBBY.into())));
}

const LOBBY: &str = crate::db::LOBBY_ID;

#[tokio::test]
async fn revoked_membership_fails_visibly_and_pauses() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let a = space(&st, "alice");
    assert!(matches!(
        st.db.add_member(&a, "bob", 10),
        Ok(crate::db::AddMember::Added)
    ));
    let mut b = body(cron("0 9 * * *", "Asia/Manila"));
    b.space_id = Some(a.clone());
    b.room_id = Some(LOBBY.into());
    let task = create(&st, "bob", b, t(NOW)).unwrap();
    st.db.remove_member(&a, "bob").unwrap();

    let r = tick(&st, t("2026-11-23T01:00:00Z"), &fake).await;
    assert_eq!(r.revoked, 1);
    assert!(
        fake.calls().is_empty(),
        "nothing is submitted after revocation"
    );
    let task = get(&st, &task.id);
    assert!(!task.enabled);
    assert_eq!(task.next_run_at, None);
    assert_eq!(task.last_status.as_deref(), Some(status::REVOKED));
    assert_eq!(runs(&st, "bob", &task.id)[0].status, status::REVOKED);
    let notes = st.db.personal_after("bob", 0, 10).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].kind, kind::ERROR);
    assert!(notes[0].text.contains("no longer a member"));
    // Nothing about the Space reaches the room.
    assert!(st.db.events_after(&a, LOBBY, 0, 50).unwrap().is_empty());
    // Re-enabling is refused while bob is not a member; pausing/deleting work.
    let en = PatchBody {
        enabled: Some(true),
        ..Default::default()
    };
    assert!(matches!(
        update_task(&st.db, "bob", &task.id, en, t(NOW), 8_000),
        Err(ScheduleError::NotFound(_))
    ));
    delete_task(&st.db, "bob", &task.id).unwrap();
}

#[tokio::test]
async fn edit_or_pause_after_claim_cancels_the_occurrence() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let now = t("2026-11-23T01:00:00Z");
    let a = create(&st, "alice", body(cron("0 9 * * *", "Asia/Manila")), t(NOW)).unwrap();
    let b = create(&st, "alice", body(cron("0 9 * * *", "Asia/Manila")), t(NOW)).unwrap();
    let c = create(&st, "alice", body(cron("0 9 * * *", "Asia/Manila")), t(NOW)).unwrap();
    let (claims, _) = claim_due(&st.db, now, 32).unwrap();
    assert_eq!(claims.len(), 3);
    let edit = PatchBody {
        prompt: Some("changed".into()),
        ..Default::default()
    };
    update_task(&st.db, "alice", &a.id, edit, now, 8_000).unwrap();
    let pause = PatchBody {
        enabled: Some(false),
        ..Default::default()
    };
    update_task(&st.db, "alice", &b.id, pause, now, 8_000).unwrap();
    delete_task(&st.db, "alice", &c.id).unwrap();
    let mut report = TickReport::default();
    process_claims(
        &st,
        now,
        &claims,
        fake.as_ref(),
        StdDuration::from_secs(1),
        &mut report,
    )
    .await;
    assert_eq!(report.cancelled, 3);
    assert!(fake.calls().is_empty());
    assert_eq!(runs(&st, "alice", &a.id)[0].status, status::CANCELLED);
    assert_eq!(runs(&st, "alice", &b.id)[0].status, status::CANCELLED);
    assert_eq!(get(&st, &b.id).next_run_at, None);
}

#[tokio::test]
async fn pause_resume_does_not_catch_up() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let task = create(&st, "alice", body(cron("0 * * * *", "UTC")), t(NOW)).unwrap();
    let pause = PatchBody {
        enabled: Some(false),
        ..Default::default()
    };
    update_task(&st.db, "alice", &task.id, pause, t(NOW), 8_000).unwrap();
    assert_eq!(
        tick(&st, t("2026-11-22T05:00:00Z"), &fake).await,
        TickReport::default()
    );
    let resume = PatchBody {
        enabled: Some(true),
        ..Default::default()
    };
    let task = update_task(
        &st.db,
        "alice",
        &task.id,
        resume,
        t("2026-11-22T05:10:00Z"),
        8_000,
    )
    .unwrap();
    assert_eq!(task.next_run_at, Some(t("2026-11-22T06:00:00Z")));
    assert!(fake.calls().is_empty());
    assert!(runs(&st, "alice", &task.id).is_empty());
}

#[tokio::test]
async fn edit_schedule_and_expired_once() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let task = create(&st, "alice", body(once("2026-11-22T03:00:00Z")), t(NOW)).unwrap();
    assert_eq!(
        tick(&st, t("2026-11-22T03:00:00Z"), &fake).await.dispatched,
        1
    );
    // Fired: re-enabling after a pause needs a new time.
    let pause = PatchBody {
        enabled: Some(false),
        ..Default::default()
    };
    update_task(
        &st.db,
        "alice",
        &task.id,
        pause,
        t("2026-11-22T03:01:00Z"),
        8_000,
    )
    .unwrap();
    let resume = PatchBody {
        enabled: Some(true),
        ..Default::default()
    };
    assert!(matches!(
        update_task(
            &st.db,
            "alice",
            &task.id,
            resume,
            t("2026-11-22T03:01:00Z"),
            8_000
        ),
        Err(ScheduleError::BadRequest(_))
    ));
    let new_time = PatchBody {
        enabled: Some(true),
        schedule: Some(once("2026-11-22T04:00:00Z")),
        ..Default::default()
    };
    let task = update_task(
        &st.db,
        "alice",
        &task.id,
        new_time,
        t("2026-11-22T03:01:00Z"),
        8_000,
    )
    .unwrap();
    assert_eq!(task.next_run_at, Some(t("2026-11-22T04:00:00Z")));
    assert_eq!(
        tick(&st, t("2026-11-22T04:00:01Z"), &fake).await.dispatched,
        1
    );
    assert_eq!(runs(&st, "alice", &task.id).len(), 2);
}

#[tokio::test]
async fn missed_recurrences_are_recorded_once_not_replayed() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let task = create(&st, "alice", body(cron("0 * * * *", "UTC")), t(NOW)).unwrap();
    // Server "down" from 02:00 until 06:30: 03:00, 04:00, 05:00, 06:00 missed.
    let r = tick(&st, t("2026-11-22T06:30:00Z"), &fake).await;
    assert_eq!(r.missed, 1);
    assert_eq!(r.dispatched, 0);
    assert!(fake.calls().is_empty());
    let h = runs(&st, "alice", &task.id);
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].status, status::MISSED);
    assert!(
        h[0].detail.as_deref().unwrap().contains("4 occurrence(s)"),
        "{h:?}"
    );
    assert_eq!(
        get(&st, &task.id).next_run_at,
        Some(t("2026-11-22T07:00:00Z"))
    );
    assert_eq!(
        tick(&st, t("2026-11-22T07:00:30Z"), &fake).await.dispatched,
        1
    );
    delete_task(&st.db, "alice", &task.id).unwrap();

    // A one-time task found after the grace window is missed, not run late.
    let late = create(&st, "alice", body(once("2026-11-22T08:00:00Z")), t(NOW)).unwrap();
    let r = tick(&st, t("2026-11-22T08:06:00Z"), &fake).await;
    assert_eq!(r.missed, 1);
    assert_eq!(fake.calls().len(), 1);
    assert_eq!(get(&st, &late.id).next_run_at, None);
    // Within the grace window it still runs.
    create(&st, "alice", body(once("2026-11-22T09:00:00Z")), t(NOW)).unwrap();
    assert_eq!(
        tick(&st, t("2026-11-22T09:04:00Z"), &fake).await.dispatched,
        1
    );
}

#[tokio::test]
async fn external_failure_is_recorded_and_not_retried() {
    let st = state().await;
    let fake = Fake::new(Mode::Fail);
    let task = create(&st, "alice", body(cron("0 9 * * *", "Asia/Manila")), t(NOW)).unwrap();
    let r = tick(&st, t("2026-11-23T01:00:00Z"), &fake).await;
    assert_eq!(r.errors, 1);
    let h = runs(&st, "alice", &task.id);
    assert_eq!(h[0].status, status::ERROR);
    assert!(h[0].detail.as_deref().unwrap().contains("offline"));
    let task = get(&st, &task.id);
    assert!(task.enabled, "a routing error does not pause the task");
    assert_eq!(task.last_status.as_deref(), Some(status::ERROR));
    assert_eq!(task.next_run_at, Some(t("2026-11-24T01:00:00Z")));
    assert_eq!(
        tick(&st, t("2026-11-23T01:01:00Z"), &fake).await,
        TickReport::default()
    );
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test]
async fn hung_submission_is_unknown_and_not_retried() {
    let st = state().await;
    let fake = Fake::new(Mode::Hang);
    let task = create(&st, "alice", body(once("2026-11-22T02:00:10Z")), t(NOW)).unwrap();
    let r = tick(&st, t("2026-11-22T02:00:10Z"), &fake).await;
    assert_eq!(r.unknown, 1);
    assert_eq!(runs(&st, "alice", &task.id)[0].status, status::UNKNOWN);
    assert_eq!(
        get(&st, &task.id).last_status.as_deref(),
        Some(status::UNKNOWN)
    );
    *fake.mode.lock().unwrap() = Mode::Ok;
    assert_eq!(
        tick(&st, t("2026-11-22T02:00:20Z"), &fake).await,
        TickReport::default()
    );
    assert_eq!(fake.calls().len(), 1);
    // The submission was left running, not cancelled; when it returns, its
    // real result replaces `unknown`.
    fake.release.notify_one();
    let mut status_now = String::new();
    for _ in 0..200 {
        status_now = runs(&st, "alice", &task.id)[0].status.clone();
        if status_now != status::UNKNOWN {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    assert_eq!(status_now, status::DISPATCHED);
    assert_eq!(
        get(&st, &task.id).last_status.as_deref(),
        Some(status::DISPATCHED)
    );
}

#[tokio::test]
async fn concurrent_ticks_submit_once() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    for _ in 0..5 {
        create(&st, "alice", body(cron("0 9 * * *", "Asia/Manila")), t(NOW)).unwrap();
    }
    let now = t("2026-11-23T01:00:00Z");
    let (a, b, c) = tokio::join!(
        tick(&st, now, &fake),
        tick(&st, now, &fake),
        tick(&st, now, &fake)
    );
    assert_eq!(a.dispatched + b.dispatched + c.dispatched, 5);
    assert_eq!(fake.calls().len(), 5);
}

#[tokio::test]
async fn unique_occurrence_guards_a_reset_next_run() {
    // Even if next_run_at were pointed back at an occurrence already claimed
    // (for example by a second process with a stale view), the unique
    // (task_id, due_at) row stops a second submission.
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let task = create(&st, "alice", body(cron("0 9 * * *", "Asia/Manila")), t(NOW)).unwrap();
    let now = t("2026-11-23T01:00:00Z");
    assert_eq!(tick(&st, now, &fake).await.dispatched, 1);
    st.db
        .lock()
        .execute(
            "UPDATE schedules SET next_run_at = ?2 WHERE id = ?1",
            params![task.id, ms(t("2026-11-23T01:00:00Z"))],
        )
        .unwrap();
    assert_eq!(
        tick(&st, now + Duration::seconds(30), &fake).await,
        TickReport::default()
    );
    assert_eq!(fake.calls().len(), 1);
    assert_eq!(
        get(&st, &task.id).next_run_at,
        Some(t("2026-11-24T01:00:00Z"))
    );
}

#[tokio::test]
async fn restart_marks_interrupted_and_never_replays() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metro.db");
    let id;
    {
        let st = state_with(Db::open(&path).unwrap()).await;
        id = create(&st, "alice", body(cron("0 9 * * *", "Asia/Manila")), t(NOW))
            .unwrap()
            .id;
        // Claimed, then the process "dies" before submission finishes.
        let (claims, _) = claim_due(&st.db, t("2026-11-23T01:00:00Z"), 32).unwrap();
        assert_eq!(claims.len(), 1);
    }
    let st = state_with(Db::open(&path).unwrap()).await;
    // A second process on the same file sees nothing left to claim.
    assert!(
        claim_due(&st.db, t("2026-11-23T01:00:05Z"), 32)
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(
        recover_interrupted(&st.db, t("2026-11-23T01:01:00Z")).unwrap(),
        1
    );
    let fake = Fake::new(Mode::Ok);
    assert_eq!(
        tick(&st, t("2026-11-23T01:01:00Z"), &fake).await,
        TickReport::default()
    );
    assert!(fake.calls().is_empty());
    let h = runs(&st, "alice", &id);
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].status, status::INTERRUPTED);
    let task = get(&st, &id);
    assert_eq!(task.last_status.as_deref(), Some(status::INTERRUPTED));
    assert_eq!(task.next_run_at, Some(t("2026-11-24T01:00:00Z")));
    // The next day fires normally.
    assert_eq!(
        tick(&st, t("2026-11-24T01:00:00Z"), &fake).await.dispatched,
        1
    );
}

#[tokio::test]
async fn history_is_bounded() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let task = create(&st, "alice", body(cron("*/15 * * * *", "UTC")), t(NOW)).unwrap();
    let mut now = task.next_run_at.unwrap();
    for _ in 0..(HISTORY_PER_TASK + 5) {
        assert_eq!(tick(&st, now, &fake).await.dispatched, 1);
        now += Duration::minutes(15);
    }
    assert_eq!(runs(&st, "alice", &task.id).len(), HISTORY_PER_TASK);
    let n: i64 = st
        .db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM schedule_runs WHERE task_id = ?1",
            [&task.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n as usize, HISTORY_PER_TASK);
}

#[test]
fn patch_distinguishes_null_from_absent() {
    let p: PatchBody = serde_json::from_str(r#"{"space_id":null}"#).unwrap();
    assert_eq!(p.space_id, Some(None));
    assert_eq!(p.room_id, None);
    let p: PatchBody = serde_json::from_str(r#"{}"#).unwrap();
    assert_eq!(p.space_id, None);
    assert!(serde_json::from_str::<PatchBody>(r#"{"owner_id":"bob"}"#).is_err());
    assert!(
        serde_json::from_str::<CreateBody>(
            r#"{"name":"a","prompt":"b","schedule":{"kind":"once","at":"x"},"owner_id":"bob"}"#
        )
        .is_err()
    );
}

#[tokio::test]
async fn move_to_personal_chat_clears_room() {
    let st = state().await;
    let a = space(&st, "alice");
    let mut b = body(cron("0 9 * * *", "UTC"));
    b.space_id = Some(a);
    b.room_id = Some(LOBBY.into());
    let task = create(&st, "alice", b, t(NOW)).unwrap();
    let p: PatchBody = serde_json::from_str(r#"{"space_id":null}"#).unwrap();
    let task = update_task(&st.db, "alice", &task.id, p, t(NOW), 8_000).unwrap();
    assert_eq!(task.target, None);
    assert_eq!(task.view()["room_id"], serde_json::Value::Null);
}

#[tokio::test]
async fn run_now_at_the_due_instant_never_swallows_the_occurrence() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let now = t(NOW);
    let task = create(&st, "alice", body(once("2026-11-22T10:00:06+08:00")), now).unwrap();
    let due = task.next_run_at.unwrap();
    // "Run now" at exactly the instant the occurrence is due.
    let (claim, _) = claim_now(&st.db, "alice", &task.id, None, due).unwrap();
    finish(
        &st.db,
        &claim,
        status::DISPATCHING,
        status::DISPATCHED,
        "ok",
        due,
    )
    .unwrap();
    // The scheduled occurrence is still claimed and dispatched, once.
    let r = tick(&st, due, &fake).await;
    assert_eq!(r.dispatched, 1, "{r:?}");
    assert_eq!(
        tick(&st, due + Duration::seconds(1), &fake).await,
        TickReport::default()
    );
    assert_eq!(fake.calls().len(), 1);
    let h = runs(&st, "alice", &task.id);
    assert_eq!(h.len(), 2, "{h:?}");
    assert!(h.iter().all(|r| r.due_at == "2026-11-22T02:00:06Z"));
    assert_eq!(h.iter().filter(|r| r.run_now).count(), 1);
    assert_eq!(h.iter().filter(|r| !r.run_now).count(), 1);
    // Two run-nows in the same millisecond get distinct rows too.
    let (a, _) = claim_now(&st.db, "alice", &task.id, None, due).unwrap();
    let (b, _) = claim_now(&st.db, "alice", &task.id, None, due).unwrap();
    assert_ne!(a.run_id, b.run_id);
}

#[tokio::test]
async fn the_summary_follows_the_latest_occurrence_by_run_id() {
    let st = state().await;
    let now = t(NOW);
    let task = create(&st, "alice", body(once("2026-11-22T10:00:06+08:00")), now).unwrap();
    let due = task.next_run_at.unwrap();
    // The scheduled occurrence and a run now share the same time.
    let (scheduled, _) = claim_due(&st.db, due, 10).unwrap();
    let scheduled = scheduled.into_iter().next().unwrap();
    let (manual, _) = claim_now(&st.db, "alice", &task.id, None, due).unwrap();
    finish(
        &st.db,
        &manual,
        status::DISPATCHING,
        status::DISPATCHED,
        "ok",
        due,
    )
    .unwrap();
    // The older occurrence finishing later doesn't overwrite the summary.
    finish(
        &st.db,
        &scheduled,
        status::DISPATCHING,
        status::ERROR,
        "boom",
        due,
    )
    .unwrap();
    assert_eq!(
        get(&st, &task.id).last_status.as_deref(),
        Some(status::DISPATCHED)
    );
    // A transition that didn't happen changes nothing either.
    finish(&st.db, &manual, status::UNKNOWN, status::ERROR, "late", due).unwrap();
    assert_eq!(
        get(&st, &task.id).last_status.as_deref(),
        Some(status::DISPATCHED)
    );
}

#[tokio::test]
async fn run_now_dispatches_exactly_the_claimed_snapshot() {
    let st = state().await;
    let fake = Fake::new(Mode::Ok);
    let now = t(NOW);
    let task = create(&st, "alice", body(cron("0 3 * * *", "UTC")), now).unwrap();
    // A stale revision is refused and records nothing.
    let err = run_now_with(
        &st,
        "alice",
        &task.id,
        Some(task.revision + 1),
        fake.clone(),
        StdDuration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ScheduleError::Conflict(_)), "{err:?}");
    assert!(runs(&st, "alice", &task.id).is_empty());
    assert!(fake.calls().is_empty());
    // Claimed, then edited: the claimed prompt is what's sent.
    let (claim, snapshot) =
        claim_now(&st.db, "alice", &task.id, Some(task.revision), Utc::now()).unwrap();
    let edit = PatchBody {
        prompt: Some("Edited after the claim".into()),
        ..Default::default()
    };
    update_task(&st.db, "alice", &task.id, edit, now, 8_000).unwrap();
    let run = dispatch_claimed(
        &st,
        claim,
        snapshot,
        fake.as_ref(),
        StdDuration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(run.status, status::DISPATCHED);
    assert!(run.run_now);
    assert_eq!(fake.calls()[0].2, "Summarize yesterday");
    // A required Space the task doesn't target is refused.
    let mut conn = st.db.lock();
    let tx = conn.transaction().unwrap();
    let err = claim_now_in(&tx, "alice", &task.id, None, Some("s-other"), Utc::now()).unwrap_err();
    assert!(matches!(err, ScheduleError::Conflict(_)), "{err:?}");
}

#[tokio::test]
async fn run_now_is_recorded_even_if_its_caller_goes_away() {
    let st = state().await;
    let fake = Fake::new(Mode::Hang);
    let task = create(&st, "alice", body(cron("0 3 * * *", "UTC")), t(NOW)).unwrap();
    let (s2, f2, id) = (st.clone(), fake.clone(), task.id.clone());
    // The caller (an HTTP handler, say) is dropped while the prompt is out.
    let caller = tokio::spawn(async move {
        let _ = run_now_with(&s2, "alice", &id, None, f2, StdDuration::from_secs(30)).await;
    });
    for _ in 0..200 {
        if !fake.calls().is_empty() {
            break;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    assert_eq!(fake.calls().len(), 1);
    caller.abort();
    let _ = caller.await;
    fake.release.notify_one();
    for _ in 0..200 {
        let h = runs(&st, "alice", &task.id);
        if h[0].status != status::DISPATCHING {
            assert_eq!(h[0].status, status::DISPATCHED, "{h:?}");
            return;
        }
        tokio::time::sleep(StdDuration::from_millis(10)).await;
    }
    panic!(
        "the occurrence was left dispatching: {:?}",
        runs(&st, "alice", &task.id)
    );
}
