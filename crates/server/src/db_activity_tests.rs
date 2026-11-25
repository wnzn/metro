//! Startup reconciliation of turns left `started` ([`super::interrupt_orphaned_activity`]).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use super::*;

const SID: &str = "s-0000000000000";
const SID2: &str = "s-1111111111111";

/// A fresh database file with two Spaces, each with a lobby and a second
/// room, closed so tests can write raw rows.
fn fresh(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("metro.sqlite3");
    drop(Db::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    for sid in [SID, SID2] {
        conn.execute(
            "INSERT INTO spaces (id, name, owner_id, created_at) VALUES (?1, ?1, 'alice', 0)",
            [sid],
        )
        .unwrap();
        for room in [LOBBY_ID, "room2"] {
            conn.execute(
                "INSERT INTO space_rooms (space_id, id, name, created_at) VALUES (?1, ?2, ?2, 0)",
                [sid, room],
            )
            .unwrap();
        }
    }
    path
}

fn act(id: &str, phase: &str) -> String {
    serde_json::to_string(&Activity::agent(id, "worker").at(phase)).unwrap()
}

/// Insert a raw row; `activity` is stored exactly as given.
fn room_row(
    c: &Connection,
    sid: &str,
    room: &str,
    kind: &str,
    actor: (&str, Option<&str>),
    activity: Option<&str>,
) -> i64 {
    c.execute(
        "INSERT INTO space_events (space_id, room_id, kind, actor, actor_id, text, created_at, activity)
         VALUES (?1, ?2, ?3, ?4, ?5, '', 1, ?6)",
        params![sid, room, kind, actor.0, actor.1, activity],
    )
    .unwrap();
    c.last_insert_rowid()
}

fn chat_row(
    c: &Connection,
    user: &str,
    kind: &str,
    actor: (&str, Option<&str>),
    activity: Option<&str>,
) -> i64 {
    c.execute(
        "INSERT INTO personal_events (user_id, kind, actor, actor_id, text, created_at, activity)
         VALUES (?1, ?2, ?3, ?4, '', 1, ?5)",
        params![user, kind, actor.0, actor.1, activity],
    )
    .unwrap();
    c.last_insert_rowid()
}

/// One stored event, as compared in these tests.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Row {
    id: i64,
    scope: (String, String),
    kind: String,
    actor: String,
    actor_id: Option<String>,
    text: String,
    activity: Option<String>,
}

fn rows(c: &Connection, table: &str) -> Vec<Row> {
    let scope = if table == "space_events" {
        "space_id, room_id"
    } else {
        "user_id, ''"
    };
    let mut stmt = c
        .prepare(&format!(
            "SELECT id, {scope}, kind, actor, actor_id, text, activity FROM {table} ORDER BY id"
        ))
        .unwrap();
    stmt.query_map([], |r| {
        Ok(Row {
            id: r.get(0)?,
            scope: (r.get(1)?, r.get(2)?),
            kind: r.get(3)?,
            actor: r.get(4)?,
            actor_id: r.get(5)?,
            text: r.get(6)?,
            activity: match r.get_ref(7)? {
                rusqlite::types::ValueRef::Null => None,
                rusqlite::types::ValueRef::Text(t) => Some(String::from_utf8(t.to_vec()).unwrap()),
                other => Some(format!("{other:?}")),
            },
        })
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

/// The pass as it was before it became set-based (a correlated lookup per
/// started event), kept as the reference. It only accepts readable JSON:
/// SQLite rejects anything else in `json_extract`.
fn reference_orphans(c: &Connection, table: &str) -> Vec<i64> {
    let scope = if table == "space_events" {
        "AND l.space_id = e.space_id AND l.room_id = e.room_id"
    } else {
        "AND l.user_id = e.user_id"
    };
    let sql = format!(
        "SELECT e.id FROM {table} e WHERE e.kind = 'activity'
           AND json_extract(e.activity, '$.phase') = 'started'
           AND NOT EXISTS (SELECT 1 FROM {table} l WHERE l.kind = 'activity' AND l.id > e.id {scope}
                           AND json_extract(l.activity, '$.id') = json_extract(e.activity, '$.id'))
         ORDER BY e.id"
    );
    let mut stmt = c.prepare(&sql).unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// What one reopen must add to `before`: for each reference orphan whose
/// activity this server can read, one `interrupted` event in its scope, by
/// its actor, in id order. Nothing else changes.
fn expected_after(before: &[Row], orphans: &[i64]) -> Vec<(Row, Option<String>)> {
    let by_id: BTreeMap<i64, &Row> = before.iter().map(|r| (r.id, r)).collect();
    orphans
        .iter()
        .filter_map(|id| {
            let start = by_id[id];
            let a: Activity = serde_json::from_str(start.activity.as_deref()?).ok()?;
            let json = serde_json::to_string(&a.at(activity::INTERRUPTED)).unwrap();
            Some((start.clone(), Some(json)))
        })
        .collect()
}

fn assert_reconciled(before: &[Row], after: &[Row], orphans: &[i64]) {
    assert_eq!(&after[..before.len()], before, "existing rows never change");
    let added = &after[before.len()..];
    let want = expected_after(before, orphans);
    assert_eq!(
        added.len(),
        want.len(),
        "one interruption per orphaned turn"
    );
    for (got, (start, activity)) in added.iter().zip(&want) {
        assert_eq!(got.scope, start.scope);
        assert_eq!(got.kind, kind::ACTIVITY);
        assert_eq!((&got.actor, &got.actor_id), (&start.actor, &start.actor_id));
        assert_eq!(got.text, "");
        assert_eq!(&got.activity, activity);
    }
}

fn mark(c: &Connection, table: &str) -> Option<String> {
    c.query_row(
        "SELECT value FROM metro_meta WHERE key = ?1",
        [format!("activity_reconciled_through:{table}")],
        |r| r.get(0),
    )
    .optional()
    .unwrap()
}

fn seq(c: &Connection, table: &str) -> i64 {
    c.query_row(
        "SELECT seq FROM sqlite_sequence WHERE name = ?1",
        [table],
        |r| r.get(0),
    )
    .unwrap()
}

/// A small deterministic generator, so the mixed history is reproducible.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

/// Mixed, interleaved history in both scopes: turn ids reused across rooms,
/// Spaces and people; restarted turns; every phase; lifecycle ids on other
/// kinds; legacy rows without activity; ids that are numbers or missing;
/// starts this server can't read back into an activity.
fn seed_mixed(c: &Connection, seed: u64, n: usize) {
    let mut g = Lcg(seed);
    let phases = [
        activity::STARTED,
        activity::STARTED,
        activity::COMPLETED,
        activity::FAILED,
        activity::CANCELLED,
        activity::INTERRUPTED,
        "paused",
    ];
    let actors = [
        ("agent", None),
        ("Alice", Some("alice")),
        ("Bob", Some("bob")),
    ];
    for _ in 0..n {
        let turn = format!("run_{}", g.next(12));
        let phase = phases[g.next(phases.len() as u64) as usize];
        let actor = actors[g.next(3) as usize];
        let activity = match g.next(20) {
            0 => None,
            1 => Some(format!(r#"{{"id":{},"phase":"{phase}"}}"#, g.next(3))),
            2 => Some(format!(
                r#"{{"name":"x","source":"agent","phase":"{phase}"}}"#
            )),
            3 => Some(format!(r#"{{"id":"{turn}","phase":"{phase}"}}"#)),
            4 => Some(format!(r#"{{"id":"{turn}","name":"x","source":"agent"}}"#)),
            _ => Some(act(&turn, phase)),
        };
        let kind = if g.next(8) == 0 {
            kind::TOOL_CALL
        } else {
            kind::ACTIVITY
        };
        if g.next(2) == 0 {
            let sid = [SID, SID2][g.next(2) as usize];
            let room = [LOBBY_ID, "room2"][g.next(2) as usize];
            room_row(c, sid, room, kind, actor, activity.as_deref());
        } else {
            let user = ["alice", "bob"][g.next(2) as usize];
            chat_row(c, user, kind, actor, activity.as_deref());
        }
        if g.next(10) == 0 {
            let user = ["alice", "bob"][g.next(2) as usize];
            chat_row(c, user, kind::MESSAGE, actors[1], None);
        }
    }
}

#[test]
fn matches_the_reference_on_mixed_history_in_both_scopes() {
    for seed in 1..=8 {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh(dir.path());
        let (before, orphans) = {
            let c = Connection::open(&path).unwrap();
            seed_mixed(&c, seed, 400);
            let tables = ["space_events", "personal_events"];
            (
                tables.map(|t| rows(&c, t)),
                tables.map(|t| reference_orphans(&c, t)),
            )
        };
        assert!(
            orphans.iter().all(|o| !o.is_empty()),
            "seed {seed} has orphans in both scopes"
        );
        drop(Db::open(&path).unwrap());
        let c = Connection::open(&path).unwrap();
        let after = ["space_events", "personal_events"].map(|t| rows(&c, t));
        for i in 0..2 {
            assert_reconciled(&before[i], &after[i], &orphans[i]);
            // By the reference too, nothing readable is left open.
            let t = ["space_events", "personal_events"][i];
            let left: Vec<i64> = reference_orphans(&c, t);
            for id in left {
                let r = after[i].iter().find(|r| r.id == id).unwrap();
                let readable = r
                    .activity
                    .as_deref()
                    .and_then(|a| serde_json::from_str::<Activity>(a).ok());
                assert!(readable.is_none(), "seed {seed}: {r:?} left open");
            }
        }
        // Restarting again changes nothing.
        drop(c);
        drop(Db::open(&path).unwrap());
        let c = Connection::open(&path).unwrap();
        assert_eq!(
            ["space_events", "personal_events"].map(|t| rows(&c, t)),
            after
        );
    }
}

/// The same, with the history written across several starts: turn ids
/// recur after each pass, so later passes run above the mark.
#[test]
fn matches_the_reference_across_several_starts() {
    let tables = ["space_events", "personal_events"];
    for seed in 11..=14 {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh(dir.path());
        for round in 0..5 {
            let (before, orphans) = {
                let c = Connection::open(&path).unwrap();
                seed_mixed(&c, seed * 100 + round, 80);
                (
                    tables.map(|t| rows(&c, t)),
                    tables.map(|t| reference_orphans(&c, t)),
                )
            };
            drop(Db::open(&path).unwrap());
            let c = Connection::open(&path).unwrap();
            for (i, t) in tables.iter().enumerate() {
                assert_reconciled(&before[i], &rows(&c, t), &orphans[i]);
                assert_eq!(mark(&c, t), Some(seq(&c, t).to_string()));
            }
        }
    }
}

#[test]
fn only_the_latest_event_of_a_turn_in_its_own_scope_counts() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(dir.path());
    let alice = ("Alice", Some("alice"));
    let agent = ("agent", None);
    let c = Connection::open(&path).unwrap();
    // Space: the same run id in two rooms and two Spaces.
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        alice,
        Some(&act("run_a", "started")),
    );
    room_row(
        &c,
        SID,
        "room2",
        kind::ACTIVITY,
        agent,
        Some(&act("run_a", "started")),
    );
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        agent,
        Some(&act("run_a", "completed")),
    );
    room_row(
        &c,
        SID2,
        LOBBY_ID,
        kind::ACTIVITY,
        alice,
        Some(&act("run_a", "started")),
    );
    // Ended, then started again under the same id: the restart is open.
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        agent,
        Some(&act("run_b", "started")),
    );
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        agent,
        Some(&act("run_b", "failed")),
    );
    let restart = room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        alice,
        Some(&act("run_b", "started")),
    );
    // A later non-lifecycle event of the turn does not end it.
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::TOOL_CALL,
        agent,
        Some(&act("run_b", "completed")),
    );
    // Personal Chat: the same id for two people.
    chat_row(
        &c,
        "alice",
        kind::ACTIVITY,
        agent,
        Some(&act("chat_x", "started")),
    );
    chat_row(
        &c,
        "bob",
        kind::ACTIVITY,
        agent,
        Some(&act("chat_x", "started")),
    );
    chat_row(
        &c,
        "bob",
        kind::ACTIVITY,
        agent,
        Some(&act("chat_x", "cancelled")),
    );
    // An unknown later phase still ends the turn, as before.
    chat_row(
        &c,
        "alice",
        kind::ACTIVITY,
        agent,
        Some(&act("chat_y", "started")),
    );
    chat_row(
        &c,
        "alice",
        kind::ACTIVITY,
        agent,
        Some(&act("chat_y", "paused")),
    );
    drop(c);

    let db = Db::open(&path).unwrap();
    let ended = |events: Vec<Event>| -> Vec<(String, String, Option<String>)> {
        events
            .into_iter()
            .filter(|e| e.activity.as_ref().and_then(|a| a.phase.as_deref()) == Some("interrupted"))
            .map(|e| (e.activity.unwrap().id, e.actor, e.actor_id))
            .collect()
    };
    let a = |s: &str| (s.to_owned(), "Alice".to_owned(), Some("alice".to_owned()));
    let g = |s: &str| (s.to_owned(), "agent".to_owned(), None);
    assert_eq!(
        ended(db.events_after(SID, LOBBY_ID, 0, 100).unwrap()),
        [a("run_b")]
    );
    assert_eq!(
        ended(db.events_after(SID, "room2", 0, 100).unwrap()),
        [g("run_a")]
    );
    assert_eq!(
        ended(db.events_after(SID2, LOBBY_ID, 0, 100).unwrap()),
        [a("run_a")]
    );
    assert!(ended(db.events_after(SID2, "room2", 0, 100).unwrap()).is_empty());
    assert_eq!(
        ended(db.personal_after("alice", 0, 100).unwrap()),
        [g("chat_x")]
    );
    assert!(ended(db.personal_after("bob", 0, 100).unwrap()).is_empty());
    // The interruption follows the restarted start, not the earlier one.
    let lobby = db.events_after(SID, LOBBY_ID, 0, 100).unwrap();
    assert!(lobby.last().unwrap().id > restart);
}

#[test]
fn unreadable_or_legacy_activity_neither_fails_startup_nor_is_made_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(dir.path());
    let agent = ("agent", None);
    let c = Connection::open(&path).unwrap();
    let bad: [Option<&str>; 6] = [
        None,
        Some("not json"),
        Some(r#"{"id":"run_m","phase":"started""#),
        Some(r#"{"phase":"started"}"#),
        Some(r#"{"id":null,"phase":"started"}"#),
        Some("[1,2"),
    ];
    for a in bad {
        room_row(&c, SID, LOBBY_ID, kind::ACTIVITY, agent, a);
        chat_row(&c, "alice", kind::ACTIVITY, agent, a);
    }
    // Readable JSON, but not an activity this server could write.
    let odd = r#"{"id":"run_odd","phase":"started"}"#;
    room_row(&c, SID, LOBBY_ID, kind::ACTIVITY, agent, Some(odd));
    chat_row(&c, "alice", kind::ACTIVITY, agent, Some(odd));
    // A JSONB blob: json_extract reads it, so it still belongs to its turn.
    c.execute(
        "INSERT INTO personal_events (user_id, kind, actor, text, created_at, activity)
         VALUES ('alice', 'activity', 'agent', '', 1, jsonb(?1))",
        [act("chat_blob", "started")],
    )
    .unwrap();
    // Blobs that aren't strictly valid JSONB: a bad header, an object whose
    // header is fine but whose content isn't (passes json_valid's
    // superficial check, yet json_extract rejects it), and a real start cut
    // short.
    c.execute_batch(
        "INSERT INTO personal_events (user_id, kind, actor, text, created_at, activity)
         VALUES ('alice', 'activity', 'agent', '', 1, x'ff00'), ('alice', 'activity', 'agent', '', 1, x'1cff');
         INSERT INTO space_events (space_id, room_id, kind, actor, text, created_at, activity)
         SELECT space_id, room_id, kind, actor, text, created_at,
                substr(jsonb(activity), 1, length(jsonb(activity)) - 3)
         FROM space_events WHERE activity = '{\"id\":\"run_odd\",\"phase\":\"started\"}';",
    )
    .unwrap();
    // A real open turn and a real closed turn whose end is unreadable.
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        agent,
        Some(&act("run_open", "started")),
    );
    chat_row(
        &c,
        "alice",
        kind::ACTIVITY,
        agent,
        Some(&act("chat_open", "started")),
    );
    chat_row(
        &c,
        "alice",
        kind::ACTIVITY,
        agent,
        Some(r#"{"id":"chat_open","#),
    );
    let before = ["space_events", "personal_events"].map(|t| rows(&c, t));
    // The old per-start lookup stopped startup here: SQLite rejects text
    // that isn't JSON in json_extract.
    let old = c.query_row(
        "SELECT COUNT(*) FROM personal_events e WHERE e.kind = 'activity'
           AND json_extract(e.activity, '$.phase') = 'started'",
        [],
        |r| r.get::<_, i64>(0),
    );
    assert!(old.is_err());
    drop(c);

    for _ in 0..2 {
        let db = Db::open(&path).unwrap();
        let c = db.lock();
        let after = ["space_events", "personal_events"].map(|t| rows(&c, t));
        assert_eq!(&after[0][..before[0].len()], before[0]);
        assert_eq!(&after[1][..before[1].len()], before[1]);
        let added = |i: usize| -> Vec<String> {
            after[i][before[i].len()..]
                .iter()
                .map(|r| {
                    let a: Activity = serde_json::from_str(r.activity.as_deref().unwrap()).unwrap();
                    assert_eq!(a.phase.as_deref(), Some("interrupted"));
                    a.id
                })
                .collect()
        };
        assert_eq!(added(0), ["run_open"]);
        // The blob start is read as a turn, but its text can't be read back
        // into an activity here, so like any unreadable start it is left.
        assert_eq!(added(1), ["chat_open"]);
    }
}

#[test]
fn the_high_water_mark_bounds_later_passes_and_a_bad_one_means_a_full_pass() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(dir.path());
    let agent = ("agent", None);
    {
        let c = Connection::open(&path).unwrap();
        room_row(
            &c,
            SID,
            LOBBY_ID,
            kind::ACTIVITY,
            agent,
            Some(&act("run_1", "started")),
        );
        chat_row(
            &c,
            "alice",
            kind::ACTIVITY,
            agent,
            Some(&act("chat_1", "started")),
        );
    }
    drop(Db::open(&path).unwrap());
    let c = Connection::open(&path).unwrap();
    for t in ["space_events", "personal_events"] {
        assert_eq!(mark(&c, t), Some(seq(&c, t).to_string()));
    }
    // Activity after the mark: a turn that started before the last pass and
    // was closed by it gets a later event too (only possible by hand), and a
    // new turn is left open.
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        agent,
        Some(&act("run_1", "started")),
    );
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        agent,
        Some(&act("run_2", "started")),
    );
    chat_row(
        &c,
        "alice",
        kind::ACTIVITY,
        agent,
        Some(&act("chat_2", "started")),
    );
    drop(c);
    drop(Db::open(&path).unwrap());
    let c = Connection::open(&path).unwrap();
    let interrupted = |c: &Connection, t: &str| -> Vec<String> {
        rows(c, t)
            .into_iter()
            .filter_map(|r| serde_json::from_str::<Activity>(r.activity.as_deref()?).ok())
            .filter(|a| a.phase.as_deref() == Some("interrupted"))
            .map(|a| a.id)
            .collect()
    };
    assert_eq!(interrupted(&c, "space_events"), ["run_1", "run_1", "run_2"]);
    assert_eq!(interrupted(&c, "personal_events"), ["chat_1", "chat_2"]);

    // A start hidden below the mark (as if ids had been reused) is found
    // again whenever the mark can't be trusted: unreadable, negative, above
    // the sequence (it was reset), or gone.
    for (i, bad) in [Some("x"), Some("-1"), Some("999999999"), None]
        .into_iter()
        .enumerate()
    {
        let hidden = act(&format!("run_hidden{i}"), "started");
        c.execute_batch("BEGIN").unwrap();
        let id = room_row(&c, SID, "room2", kind::ACTIVITY, agent, Some(&hidden));
        // Move it to an id at or below the mark, then reset the sequence
        // when the case calls for it.
        c.execute(
            "UPDATE space_events SET id = (SELECT MIN(id) FROM space_events) - 1 WHERE id = ?1",
            [id],
        )
        .unwrap();
        c.execute(
            "DELETE FROM sqlite_sequence WHERE name = 'space_events'",
            [],
        )
        .unwrap();
        let max: i64 = c
            .query_row("SELECT MAX(id) FROM space_events", [], |r| r.get(0))
            .unwrap();
        c.execute(
            "INSERT INTO sqlite_sequence (name, seq) VALUES ('space_events', ?1)",
            [max],
        )
        .unwrap();
        match bad {
            Some(v) => c.execute(
                "UPDATE metro_meta SET value = ?1 WHERE key = 'activity_reconciled_through:space_events'",
                [v],
            ),
            None => c.execute(
                "DELETE FROM metro_meta WHERE key = 'activity_reconciled_through:space_events'",
                [],
            ),
        }
        .unwrap();
        c.execute_batch("COMMIT").unwrap();
        let before = interrupted(&c, "space_events").len();
        drop(Db::open(&path).unwrap());
        let now = interrupted(&c, "space_events");
        assert_eq!(now.len(), before + 1, "mark {bad:?}");
        assert_eq!(now.last().unwrap(), &format!("run_hidden{i}"));
        assert_eq!(
            mark(&c, "space_events"),
            Some(seq(&c, "space_events").to_string())
        );
    }
}

#[test]
fn a_pass_that_fails_part_way_leaves_nothing_and_the_next_start_finishes_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(dir.path());
    let agent = ("agent", None);
    {
        let c = Connection::open(&path).unwrap();
        for i in 0..5 {
            room_row(
                &c,
                SID,
                LOBBY_ID,
                kind::ACTIVITY,
                agent,
                Some(&act(&format!("run_{i}"), "started")),
            );
            chat_row(
                &c,
                "alice",
                kind::ACTIVITY,
                agent,
                Some(&act(&format!("chat_{i}"), "started")),
            );
        }
        // Stands in for a crash: the personal pass fails after the Space
        // pass wrote its interruptions.
        c.execute_batch(
            "CREATE TRIGGER fail_pass BEFORE INSERT ON personal_events
             WHEN json_extract(NEW.activity, '$.phase') = 'interrupted'
             BEGIN SELECT RAISE(ABORT, 'simulated crash'); END;",
        )
        .unwrap();
    }
    let tables = ["space_events", "personal_events"];
    let (before, marks) = {
        let c = Connection::open(&path).unwrap();
        (tables.map(|t| rows(&c, t)), tables.map(|t| mark(&c, t)))
    };
    // The marks from the first start of the empty database.
    assert_eq!(marks, [Some("0".to_owned()), Some("0".to_owned())]);
    assert!(Db::open(&path).is_err());
    let c = Connection::open(&path).unwrap();
    // The Space pass's interruptions and both marks were rolled back.
    assert_eq!(tables.map(|t| rows(&c, t)), before);
    assert_eq!(tables.map(|t| mark(&c, t)), marks);
    c.execute_batch("DROP TRIGGER fail_pass").unwrap();
    // The interruptions an older server wrote before it crashed (it wrote
    // them one by one) are not repeated.
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        agent,
        Some(&act("run_0", "interrupted")),
    );
    chat_row(
        &c,
        "alice",
        kind::ACTIVITY,
        agent,
        Some(&act("chat_0", "interrupted")),
    );
    let partial = ["space_events", "personal_events"].map(|t| rows(&c, t));
    let orphans = ["space_events", "personal_events"].map(|t| reference_orphans(&c, t));
    assert_eq!(orphans.each_ref().map(Vec::len), [4, 4]);
    for _ in 0..2 {
        drop(Db::open(&path).unwrap());
        let after = ["space_events", "personal_events"].map(|t| rows(&c, t));
        assert_reconciled(&partial[0], &after[0], &orphans[0]);
        assert_reconciled(&partial[1], &after[1], &orphans[1]);
    }
}

/// Starts of `n` turns, then all their ends, in each scope: the order that
/// made the old per-start lookup quadratic.
fn seed_grouped(path: &Path, n: usize) {
    let mut c = Connection::open(path).unwrap();
    let tx = c.transaction().unwrap();
    for phase in [activity::STARTED, activity::COMPLETED] {
        for i in 0..n {
            room_row(
                &tx,
                SID,
                LOBBY_ID,
                kind::ACTIVITY,
                ("agent", None),
                Some(&act(&format!("run_{i:032}"), phase)),
            );
            chat_row(
                &tx,
                "alice",
                kind::ACTIVITY,
                ("agent", None),
                Some(&act(&format!("chat_{i:032}"), phase)),
            );
        }
    }
    tx.commit().unwrap();
}

/// A guard, not a benchmark: 3,000 grouped turns per scope took the old
/// pass several seconds even in a release build. The bound is generous for
/// slow, shared CI machines and unoptimized test builds.
#[test]
fn grouped_history_reconciles_in_linear_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(dir.path());
    seed_grouped(&path, 3_000);
    {
        // The plan is one grouped scan of the activity index, no lookup per start.
        let c = Connection::open(&path).unwrap();
        for (s, bounded) in ACTIVITY_SCOPES.iter().flat_map(|s| [(s, true), (s, false)]) {
            let sql = orphaned_starts_sql(s, bounded);
            assert_eq!(sql.contains("id > ?1"), bounded);
            let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            let params: Vec<i64> = if bounded { vec![0] } else { vec![] };
            let plan: Vec<String> = stmt
                .query_map(rusqlite::params_from_iter(params), |r| {
                    r.get::<_, String>(3)
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert!(
                plan.iter()
                    .any(|p| p.contains(&format!("{}_activity", s.table))),
                "{plan:?}"
            );
            assert!(
                plan.iter().any(|p| p.contains("INTEGER PRIMARY KEY")),
                "{plan:?}"
            );
            assert!(!plan.iter().any(|p| p.contains("CORRELATED")), "{plan:?}");
        }
    }
    let started = Instant::now();
    drop(Db::open(&path).unwrap());
    let first = started.elapsed();
    assert!(
        first < Duration::from_secs(5),
        "first full pass took {first:?}"
    );
    let c = Connection::open(&path).unwrap();
    for t in ["space_events", "personal_events"] {
        // Every turn was complete: nothing was added.
        let n: i64 = c
            .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 6_000);
        assert_eq!(mark(&c, t), Some(seq(&c, t).to_string()));
    }
}

/// `cargo test --release -p metro-server --lib grouped_history_benchmark -- --ignored --nocapture`
#[test]
#[ignore = "benchmark; run by hand"]
fn grouped_history_benchmark() {
    for n in [1_000, 5_000, 25_000] {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh(dir.path());
        seed_grouped(&path, n);
        let full = {
            let c = Connection::open(&path).unwrap();
            let t = Instant::now();
            interrupt_orphaned_activity(&c).unwrap();
            t.elapsed()
        };
        let bounded = {
            let c = Connection::open(&path).unwrap();
            let t = Instant::now();
            interrupt_orphaned_activity(&c).unwrap();
            t.elapsed()
        };
        let reference = if n <= 5_000 {
            let c = Connection::open(&path).unwrap();
            let t = Instant::now();
            for table in ["space_events", "personal_events"] {
                assert!(reference_orphans(&c, table).is_empty());
            }
            format!("{:?}", t.elapsed())
        } else {
            "skipped".to_owned()
        };
        println!(
            "{n} grouped turns per scope: full pass {full:?}, next start {bounded:?}, old query {reference}"
        );
    }
}

/// A start below the mark (moved there by hand), found only by a full pass.
fn hide_start(c: &Connection, turn: &str) {
    let id = room_row(
        c,
        SID,
        "room2",
        kind::ACTIVITY,
        ("agent", None),
        Some(&act(turn, "started")),
    );
    c.execute(
        "UPDATE space_events SET id = (SELECT MIN(id) FROM space_events) - 1 WHERE id = ?1",
        [id],
    )
    .unwrap();
}

fn interrupted_turns(c: &Connection) -> Vec<String> {
    rows(c, "space_events")
        .into_iter()
        .filter_map(|r| serde_json::from_str::<Activity>(r.activity.as_deref()?).ok())
        .filter(|a| a.phase.as_deref() == Some("interrupted"))
        .map(|a| a.id)
        .collect()
}

const SPACE_MARK: &str = "activity_reconciled_through:space_events";

/// A mark stored as a blob or as text that isn't UTF-8 is untrusted: the
/// start is not refused, the whole table is scanned, and a good mark is
/// written in its place.
#[test]
fn a_mark_that_is_not_text_means_a_full_pass() {
    for (i, bad) in ["x'ff'", "CAST(x'ff' AS TEXT)", "x'3132'", "1.5"]
        .into_iter()
        .enumerate()
    {
        let dir = tempfile::tempdir().unwrap();
        let path = fresh(dir.path());
        let c = Connection::open(&path).unwrap();
        room_row(
            &c,
            SID,
            LOBBY_ID,
            kind::MESSAGE,
            ("Alice", Some("alice")),
            None,
        );
        drop(Db::open(&path).unwrap());
        let turn = format!("run_hidden{i}");
        hide_start(&c, &turn);
        c.execute(
            &format!("UPDATE metro_meta SET value = {bad} WHERE key = ?1"),
            [SPACE_MARK],
        )
        .unwrap();
        drop(Db::open(&path).unwrap());
        assert_eq!(interrupted_turns(&c), [turn], "mark {bad}");
        assert_eq!(
            mark(&c, "space_events"),
            Some(seq(&c, "space_events").to_string())
        );
    }
}

/// A sequence that isn't an integer can't vouch for a mark: full passes,
/// and no mark is kept until the sequence is an integer again.
#[test]
fn a_sequence_that_is_not_an_integer_means_a_full_pass_and_no_mark() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(dir.path());
    let c = Connection::open(&path).unwrap();
    for _ in 0..3 {
        room_row(
            &c,
            SID,
            LOBBY_ID,
            kind::MESSAGE,
            ("Alice", Some("alice")),
            None,
        );
    }
    drop(Db::open(&path).unwrap());
    assert_eq!(mark(&c, "space_events").as_deref(), Some("3"));
    hide_start(&c, "run_hidden");
    // A closed turn on top, so the sequence is above the mark as usual.
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        ("agent", None),
        Some(&act("run_done", "started")),
    );
    room_row(
        &c,
        SID,
        LOBBY_ID,
        kind::ACTIVITY,
        ("agent", None),
        Some(&act("run_done", "completed")),
    );
    c.execute(
        "UPDATE sqlite_sequence SET seq = CAST(seq AS TEXT) WHERE name = 'space_events'",
        [],
    )
    .unwrap();
    let seq_type = |c: &Connection| -> String {
        c.query_row(
            "SELECT typeof(seq) FROM sqlite_sequence WHERE name = 'space_events'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(seq_type(&c), "text");
    // The pass writes an interruption, and with it SQLite stores the
    // sequence as an integer again: a full scan found the hidden start, so
    // a mark is written.
    drop(Db::open(&path).unwrap());
    assert_eq!(interrupted_turns(&c), ["run_hidden"]);
    assert_eq!(seq_type(&c), "integer");
    assert_eq!(
        mark(&c, "space_events"),
        Some(seq(&c, "space_events").to_string())
    );
    // Nothing to write: the sequence stays text, and no mark is kept.
    c.execute(
        "UPDATE sqlite_sequence SET seq = CAST(seq AS TEXT) WHERE name = 'space_events'",
        [],
    )
    .unwrap();
    for _ in 0..2 {
        drop(Db::open(&path).unwrap());
        assert_eq!(seq_type(&c), "text");
        assert_eq!(mark(&c, "space_events"), None);
        assert_eq!(interrupted_turns(&c), ["run_hidden"]);
    }
}

/// A full pass has no lower bound: even a start at the smallest id counts.
#[test]
fn a_full_pass_sees_every_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = fresh(dir.path());
    let c = Connection::open(&path).unwrap();
    c.execute(
        "INSERT INTO space_events (id, space_id, room_id, kind, actor, text, created_at, activity)
         VALUES (?1, ?2, 'lobby', 'activity', 'agent', '', 1, ?3)",
        params![i64::MIN, SID, act("run_min", "started")],
    )
    .unwrap();
    c.execute("DELETE FROM metro_meta WHERE key = ?1", [SPACE_MARK])
        .unwrap();
    drop(Db::open(&path).unwrap());
    assert_eq!(interrupted_turns(&c), ["run_min"]);
}
