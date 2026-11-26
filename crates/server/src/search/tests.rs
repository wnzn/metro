//! Search pipeline tests with a recording fake model layer.
//!
//! The fake embeds by keywords (deliberately unnormalized, so ranking only
//! works if vectors are normalized), records every text it receives with
//! the destination of the call, and answers System One `noul` questions
//! from a table. Assertions look at what reached the "upstream" as much as
//! at what came back.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::{Value, json};

use super::engine::{Engine, ModelError, ModelFuture, Models, Routing};
use super::vector::{self, TopK};
use super::*;
use crate::db::{Addressed, Author, Db, LOBBY_ID, kind};

#[derive(Default)]
struct Calls {
    /// (destination, text) of every text embedded.
    embedded: Vec<(Option<String>, String)>,
    /// (destination, candidate text) of every System One request.
    decided: Vec<(Option<String>, String)>,
    embed_calls: usize,
}

struct Fake {
    calls: Mutex<Calls>,
    /// Fingerprint per destination key (`chat` or the Space id); missing
    /// means the connection may not be used there.
    fingerprints: Mutex<HashMap<String, String>>,
    /// Extra dimensions appended to every vector (to simulate a model
    /// that changed its dimension without a new fingerprint).
    pad: Mutex<usize>,
    /// Noul per candidate text; missing means "fail this call".
    nouls: Mutex<HashMap<String, f64>>,
    decide_fails: Mutex<bool>,
    /// Run once, inside the first embed call (between the batch's read and
    /// its store).
    on_embed: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

const WORDS: [&str; 4] = ["deploy", "lunch", "invoice", "holiday"];

fn dest_key(dest: Option<&str>) -> String {
    dest.unwrap_or("chat").to_owned()
}

impl Fake {
    fn new() -> Self {
        Self {
            calls: Mutex::default(),
            fingerprints: Mutex::default(),
            pad: Mutex::new(0),
            nouls: Mutex::default(),
            decide_fails: Mutex::new(false),
            on_embed: Mutex::default(),
        }
    }

    fn allow(&self, dest: Option<&str>, fp: &str) {
        self.fingerprints
            .lock()
            .unwrap()
            .insert(dest_key(dest), fp.to_owned());
    }

    fn noul(&self, text: &str, n: f64) {
        self.nouls.lock().unwrap().insert(text.to_owned(), n);
    }

    fn embedded(&self) -> Vec<(Option<String>, String)> {
        self.calls.lock().unwrap().embedded.clone()
    }

    fn decided(&self) -> Vec<(Option<String>, String)> {
        self.calls.lock().unwrap().decided.clone()
    }

    /// Keyword counts, scaled by text length so magnitudes differ wildly.
    fn vector(&self, text: &str) -> Vec<f32> {
        let lower = text.to_lowercase();
        let scale = text.len() as f32;
        let mut v: Vec<f32> = WORDS
            .iter()
            .map(|w| lower.matches(w).count() as f32 * scale)
            .collect();
        // A small constant component so no vector is zero.
        v.push(0.01 * scale);
        v.extend(std::iter::repeat_n(1.0, *self.pad.lock().unwrap()));
        v
    }
}

impl Models for Fake {
    fn fingerprint<'a>(&'a self, dest: Option<&'a str>, _c: &'a str) -> ModelFuture<'a, String> {
        let fp = self
            .fingerprints
            .lock()
            .unwrap()
            .get(&dest_key(dest))
            .cloned();
        Box::pin(async move {
            fp.ok_or_else(|| ModelError("this connection is not shared with that Space".into()))
        })
    }

    fn embed<'a>(
        &'a self,
        dest: Option<&'a str>,
        _c: &'a str,
        inputs: Vec<String>,
    ) -> ModelFuture<'a, Vec<Vec<f32>>> {
        if let Some(hook) = self.on_embed.lock().unwrap().take() {
            hook();
        }
        let out = {
            let mut calls = self.calls.lock().unwrap();
            calls.embed_calls += 1;
            for t in &inputs {
                calls.embedded.push((dest.map(str::to_owned), t.clone()));
            }
            inputs.iter().map(|t| self.vector(t)).collect()
        };
        Box::pin(async move { Ok(out) })
    }

    fn decide<'a>(
        &'a self,
        dest: Option<&'a str>,
        _c: &'a str,
        state: Value,
        questions: Value,
    ) -> ModelFuture<'a, Value> {
        let text = state["candidate_message"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert_eq!(questions[rerank::QUESTION]["type"], "noul");
        assert!(state["search_query"].is_string());
        // One candidate per request, nothing else in state.
        assert_eq!(state.as_object().unwrap().len(), 2);
        self.calls
            .lock()
            .unwrap()
            .decided
            .push((dest.map(str::to_owned), text.clone()));
        let fails = *self.decide_fails.lock().unwrap();
        let noul = self.nouls.lock().unwrap().get(&text).copied();
        Box::pin(async move {
            match (fails, noul) {
                (false, Some(n)) => Ok(json!({
                    "model": "jev-latest",
                    "answers": { rerank::QUESTION: { "type": "noul", "noul": n } },
                    "usage": { "input_tokens": 10, "output_tokens": 1 }
                })),
                _ => Err(ModelError("System One returned 529".into())),
            }
        })
    }
}

struct World {
    db: Db,
}

fn person(id: &str) -> Author {
    Author {
        id: Some(id.to_owned()),
        name: id.to_owned(),
    }
}

impl World {
    fn new() -> Self {
        let db = Db::open_in_memory().unwrap();
        store::init(&db.lock()).unwrap();
        Self { db }
    }

    fn space(&self, owner: &str, members: &[&str]) -> String {
        let s = self.db.create_space(owner, "space", 10).unwrap();
        for m in members {
            self.db.add_member(&s.id, m, 10).unwrap();
        }
        s.id
    }

    fn post(&self, space: &str, who: &str, text: &str) -> i64 {
        self.post_kind(space, LOBBY_ID, kind::MESSAGE, who, text)
    }

    fn post_kind(&self, space: &str, room: &str, k: &str, who: &str, text: &str) -> i64 {
        self.db
            .append_event(space, room, k, &person(who), text, &Addressed::room())
            .unwrap()
            .id
    }

    fn chat(&self, who: &str, k: &str, text: &str) -> i64 {
        self.db
            .append_personal(who, k, &person(who), text, &Addressed::room())
            .unwrap()
            .id
    }

    fn routing(classifier: bool) -> Routing {
        Routing {
            embedding: Some("emb".into()),
            classifier: classifier.then(|| "s1".to_owned()),
        }
    }

    async fn search(
        &self,
        fake: &Fake,
        user: &str,
        classifier: bool,
        scope: Scope,
        space: Option<&str>,
        query: &str,
    ) -> Result<SearchResponse, SearchError> {
        let (sources, left) =
            resolve_sources(&self.db, user, scope, space.map(str::to_owned)).await?;
        Engine::new(&self.db, user, fake, Self::routing(classifier))
            .search(query, &sources, left, 10)
            .await
    }

    async fn index(&self, fake: &Fake, user: &str, scope: Scope) -> IndexResponse {
        let (sources, left) = resolve_sources(&self.db, user, scope, None).await.unwrap();
        Engine::new(&self.db, user, fake, Self::routing(false))
            .index(&sources, left)
            .await
            .unwrap()
    }
}

fn texts(r: &SearchResponse) -> Vec<&str> {
    r.results.iter().map(|x| x.text.as_str()).collect()
}

#[tokio::test]
async fn normalized_vectors_rank_by_meaning_not_length() {
    let w = World::new();
    let fake = Fake::new();
    fake.allow(None, "fp1");
    // Long text with one "lunch" and a short pure "deploy" text: an
    // unnormalized dot product would put the long one first.
    let long = format!("deploy {} lunch lunch lunch lunch", "x".repeat(400));
    w.chat("alice", kind::MESSAGE, &long);
    w.chat("alice", kind::AGENT, "deploy");
    w.chat("alice", kind::MESSAGE, "invoice due");

    let r = w
        .search(&fake, "alice", false, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert_eq!(texts(&r)[0], "deploy");
    assert_eq!(texts(&r)[1], long.as_str());
    assert_eq!(r.ranking, Ranking::Embeddings);
    assert!(!r.partial);
    assert!(
        r.notice
            .as_deref()
            .unwrap()
            .contains("No System One connection")
    );
    assert!(
        r.results
            .iter()
            .all(|x| x.space_id.is_none() && x.room_id.is_none())
    );
    // Exact cosine of unit vectors: the identical text scores 1.
    assert!((r.results[0].score - 1.0).abs() < 1e-6);
    assert!(r.results.windows(2).all(|p| p[0].score >= p[1].score));
}

#[tokio::test]
async fn classifier_reorders_the_shortlist() {
    let w = World::new();
    let fake = Fake::new();
    fake.allow(None, "fp1");
    w.chat("alice", kind::MESSAGE, "deploy deploy");
    w.chat("alice", kind::MESSAGE, "deploy lunch");
    w.chat("alice", kind::MESSAGE, "deploy lunch lunch lunch");
    fake.noul("deploy deploy", 0.1);
    fake.noul("deploy lunch", 0.5);
    fake.noul("deploy lunch lunch lunch", 0.9);

    let before = w
        .search(&fake, "alice", false, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert_eq!(
        texts(&before),
        ["deploy deploy", "deploy lunch", "deploy lunch lunch lunch"]
    );

    let after = w
        .search(&fake, "alice", true, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert_eq!(after.ranking, Ranking::Classifier);
    assert_eq!(
        texts(&after),
        ["deploy lunch lunch lunch", "deploy lunch", "deploy deploy"]
    );
    assert_eq!(after.results[0].score, 0.9);
    // Pointwise: one request per candidate.
    assert_eq!(fake.decided().len(), 3);
}

#[tokio::test]
async fn rerank_failure_falls_back_visibly() {
    let w = World::new();
    let fake = Fake::new();
    fake.allow(None, "fp1");
    w.chat("alice", kind::MESSAGE, "deploy deploy");
    w.chat("alice", kind::MESSAGE, "deploy lunch");
    *fake.decide_fails.lock().unwrap() = true;
    let r = w
        .search(&fake, "alice", true, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert_eq!(r.ranking, Ranking::Embeddings);
    assert_eq!(texts(&r), ["deploy deploy", "deploy lunch"]);
    assert!(r.notice.unwrap().contains("could not rerank"));

    // One candidate fails: reranked ones first, the other after, "mixed".
    *fake.decide_fails.lock().unwrap() = false;
    fake.noul("deploy lunch", 0.8);
    let r = w
        .search(&fake, "alice", true, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert_eq!(r.ranking, Ranking::Mixed);
    assert_eq!(texts(&r), ["deploy lunch", "deploy deploy"]);
    assert!(r.notice.unwrap().contains("could not rerank 1"));
}

#[tokio::test]
async fn malformed_system_one_answers_are_not_scores() {
    assert_eq!(
        rerank::read_noul(&json!({"answers": {"relevant": {"type": "noul", "noul": 0.4}}})),
        Some(0.4)
    );
    assert_eq!(
        rerank::read_noul(&json!({"relevant": {"type": "noul", "noul": 1.0}})),
        Some(1.0)
    );
    assert_eq!(
        rerank::read_noul(&json!({"answers": {"relevant": {"type": "score", "score": 0.4}}})),
        None
    );
    assert_eq!(
        rerank::read_noul(&json!({"answers": {"relevant": {"type": "noul", "noul": 1.5}}})),
        None
    );
    assert_eq!(
        rerank::read_noul(&json!({"answers": {"other": {"type": "noul", "noul": 0.5}}})),
        None
    );
    assert_eq!(rerank::read_noul(&json!("0.9")), None);
}

#[tokio::test]
async fn other_people_and_other_spaces_never_reach_the_model() {
    let w = World::new();
    let fake = Fake::new();
    let a = w.space("alice", &["bob"]);
    let b = w.space("bob", &[]);
    fake.allow(None, "fp");
    fake.allow(Some(&a), "fp");
    fake.allow(Some(&b), "fp");
    w.post(&a, "bob", "deploy in space a");
    w.post(&b, "bob", "deploy secret in space b");
    w.chat("bob", kind::MESSAGE, "deploy bob private chat");
    w.chat("alice", kind::MESSAGE, "deploy alice chat");
    for t in ["deploy in space a", "deploy alice chat"] {
        fake.noul(t, 0.5);
    }

    let r = w
        .search(&fake, "alice", true, Scope::All, None, "deploy")
        .await
        .unwrap();
    let mut got = texts(&r);
    got.sort();
    assert_eq!(got, ["deploy alice chat", "deploy in space a"]);

    // Every text sent upstream belongs to the destination it was sent for.
    let mine: HashMap<&str, Option<String>> = [
        ("deploy alice chat", None),
        ("deploy in space a", Some(a.clone())),
        ("deploy", None),
    ]
    .into();
    for (dest, text) in fake.embedded() {
        if text == "deploy" {
            continue; // the query itself, embedded once per destination
        }
        assert_eq!(
            mine.get(text.as_str()),
            Some(&dest),
            "{text} sent for {dest:?}"
        );
    }
    for (dest, text) in fake.decided() {
        assert_eq!(
            mine.get(text.as_str()),
            Some(&dest),
            "{text} reranked for {dest:?}"
        );
    }
    let sent: HashSet<String> = fake
        .embedded()
        .into_iter()
        .chain(fake.decided())
        .map(|(_, t)| t)
        .collect();
    assert!(
        !sent
            .iter()
            .any(|t| t.contains("space b") || t.contains("bob private"))
    );

    // A Space alice is not in is a 404, like every Space route.
    assert_eq!(
        w.search(&fake, "alice", true, Scope::Space, Some(&b), "deploy")
            .await
            .unwrap_err(),
        SearchError::NotFound
    );
}

#[tokio::test]
async fn revoked_membership_hides_results_and_purges_vectors() {
    let w = World::new();
    let fake = Fake::new();
    let s = w.space("bob", &["alice"]);
    fake.allow(None, "fp");
    fake.allow(Some(&s), "fp");
    w.post(&s, "bob", "deploy plan");
    let r = w
        .search(&fake, "alice", false, Scope::All, None, "deploy")
        .await
        .unwrap();
    assert_eq!(texts(&r), ["deploy plan"]);
    assert_eq!(r.results[0].space_id.as_deref(), Some(s.as_str()));
    assert_eq!(r.results[0].room_id.as_deref(), Some(LOBBY_ID));

    w.db.remove_member(&s, "alice").unwrap();
    w.post(&s, "bob", "deploy after alice left");
    let before = fake.embedded().len();
    let r = w
        .search(&fake, "alice", false, Scope::All, None, "deploy")
        .await
        .unwrap();
    assert!(r.results.is_empty());
    assert!(
        fake.embedded()[before..]
            .iter()
            .all(|(_, t)| !t.contains("after alice left"))
    );
    let left: i64 = w
        .db
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM search_vectors WHERE user_id = 'alice' AND source LIKE 'space:%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn revocation_between_retrieval_and_results_is_honoured() {
    // Results are re-checked against the database: an indexed vector whose
    // reader lost access is never returned, even before purging.
    let w = World::new();
    let fake = Fake::new();
    let s = w.space("bob", &["alice"]);
    fake.allow(Some(&s), "fp");
    let id = w.post(&s, "bob", "deploy");
    let r = w
        .search(&fake, "alice", false, Scope::Space, Some(&s), "deploy")
        .await
        .unwrap();
    assert_eq!(r.results.len(), 1);
    let src = Source::Space(s.clone());
    assert_eq!(
        store::current_events(&w.db.lock(), "alice", &src, &[id])
            .unwrap()
            .len(),
        1
    );
    w.db.remove_member(&s, "alice").unwrap();
    let c = w.db.lock();
    // The vector is still stored, but nothing reads through it any more.
    assert_eq!(store::vector_count(&c, "alice", &src).unwrap(), 1);
    assert!(
        store::current_events(&c, "alice", &src, &[id])
            .unwrap()
            .is_empty()
    );
    assert!(
        store::events_after(&c, "alice", &src, 0, 10)
            .unwrap()
            .is_empty()
    );
    assert!(!store::can_read(&c, "alice", &src).unwrap());
}

#[tokio::test]
async fn destination_restricted_connection_skips_the_space() {
    let w = World::new();
    let fake = Fake::new();
    let a = w.space("alice", &[]);
    let b = w.space("alice", &[]);
    fake.allow(None, "fp");
    fake.allow(Some(&a), "fp"); // not usable for b
    w.post(&a, "alice", "deploy a");
    w.post(&b, "alice", "deploy b");
    let r = w
        .search(&fake, "alice", false, Scope::All, None, "deploy")
        .await
        .unwrap();
    assert_eq!(texts(&r), ["deploy a"]);
    assert!(r.partial);
    assert!(r.notice.unwrap().contains("1 Space(s) were not searched"));
    assert!(
        fake.embedded()
            .iter()
            .all(|(d, t)| d.as_deref() != Some(b.as_str()) && t != "deploy b")
    );

    // Searching only b fails as a whole, with the provider's own message.
    let e = w
        .search(&fake, "alice", false, Scope::Space, Some(&b), "deploy")
        .await
        .unwrap_err();
    assert!(matches!(e, SearchError::Upstream(m) if m.contains("not shared")));
}

#[tokio::test]
async fn fingerprint_change_reindexes_and_dimension_change_is_refused() {
    let w = World::new();
    let fake = Fake::new();
    fake.allow(None, "fp-reindex-1");
    w.chat("alice", kind::MESSAGE, "deploy");
    w.search(&fake, "alice", false, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert_eq!(
        fake.embedded()
            .iter()
            .filter(|(_, t)| t == "deploy")
            .count(),
        2
    );

    // Same fingerprint: nothing is embedded again but the query, which is cached.
    w.search(&fake, "alice", false, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert_eq!(fake.embedded().len(), 2);

    // Reconfigured connection: old vectors are discarded, everything is re-embedded.
    fake.allow(None, "fp-reindex-2");
    *fake.pad.lock().unwrap() = 3; // and the new model has more dimensions
    let r = w
        .search(&fake, "alice", false, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert_eq!(texts(&r), ["deploy"]);
    assert_eq!(fake.embedded().len(), 4);
    let fps: Vec<String> =
        w.db.lock()
            .prepare("SELECT DISTINCT fingerprint FROM search_vectors")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
    assert_eq!(fps, ["fp-reindex-2"]);

    // The model changes dimension without a new fingerprint: the batch is
    // refused, never mixed with the stored vectors.
    *fake.pad.lock().unwrap() = 7;
    w.chat("alice", kind::MESSAGE, "deploy again");
    let r = w
        .search(&fake, "alice", false, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert!(
        r.results.is_empty(),
        "a source with mixed dimensions is not searched"
    );
    assert!(r.partial);
    assert!(r.notice.unwrap().contains("dimensions"));
    let dims: Vec<i64> =
        w.db.lock()
            .prepare("SELECT length(vector) / 4 FROM search_vectors")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
    assert!(dims.iter().all(|d| *d == 8), "{dims:?}");
}

#[tokio::test]
async fn indexing_is_incremental_with_a_cursor() {
    let w = World::new();
    let fake = Fake::new();
    fake.allow(None, "fp");
    let total = INDEX_BATCH * 3 + 5;
    for i in 0..total {
        w.chat("alice", kind::MESSAGE, &format!("lunch note {i}"));
    }
    w.chat("alice", kind::MESSAGE, "deploy now");

    let r = w
        .search(&fake, "alice", false, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert!(r.partial);
    let notice = r.notice.unwrap();
    assert!(
        notice.contains("not indexed yet") && notice.contains("/v1/search/index"),
        "{notice}"
    );

    let mut rounds = 0;
    loop {
        let i = w.index(&fake, "alice", Scope::All).await;
        rounds += 1;
        if i.done {
            assert_eq!(i.remaining, 0);
            break;
        }
        assert!(rounds < 10);
    }
    let r = w
        .search(&fake, "alice", false, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    assert!(!r.partial, "{:?}", r.notice);
    assert_eq!(texts(&r)[0], "deploy now");
    // Every event was embedded exactly once.
    let embedded: Vec<String> = fake
        .embedded()
        .into_iter()
        .map(|(_, t)| t)
        .filter(|t| t != "deploy")
        .collect();
    assert_eq!(embedded.len(), total + 1);
    assert_eq!(embedded.iter().collect::<HashSet<_>>().len(), total + 1);
}

#[tokio::test]
async fn a_deletion_during_embedding_skips_its_vector_without_stopping_indexing() {
    let w = World::new();
    let fake = Fake::new();
    fake.allow(None, "fp");
    let ids: Vec<i64> = (0..INDEX_BATCH * 2)
        .map(|i| w.chat("alice", kind::MESSAGE, &format!("lunch note {i}")))
        .collect();
    let (db, gone) = (w.db.clone(), ids[3]);
    *fake.on_embed.lock().unwrap() = Some(Box::new(move || {
        let who = crate::auth::Human {
            id: "alice".into(),
            name: "alice".into(),
        };
        crate::deletion::delete_in_chat(&db, gone, &who)
            .unwrap()
            .unwrap();
    }));
    // One call indexes both batches: the skipped event isn't "source full".
    let i = w.index(&fake, "alice", Scope::Chat).await;
    assert!(i.done, "{i:?}");
    assert_eq!(i.remaining, 0);
    let stored: Vec<i64> = w
        .db
        .lock()
        .prepare("SELECT event_id FROM search_vectors WHERE user_id = 'alice' ORDER BY event_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let expected: Vec<i64> = ids.iter().copied().filter(|id| *id != gone).collect();
    assert_eq!(
        stored, expected,
        "the deleted event's vector never comes back"
    );
}

#[tokio::test]
async fn only_messages_and_agent_replies_are_indexed() {
    let w = World::new();
    let fake = Fake::new();
    let s = w.space("alice", &[]);
    fake.allow(None, "fp");
    fake.allow(Some(&s), "fp");
    let thread = w.db.create_room(&s, "t", Some(LOBBY_ID), 10, 10).unwrap();
    w.post_kind(&s, &thread.id, kind::MESSAGE, "alice", "deploy in thread");
    w.post_kind(&s, LOBBY_ID, kind::AGENT, "agent", "deploy agent reply");
    for (k, t) in [
        (kind::TOOL_CALL, "deploy tool call"),
        (kind::TOOL_RESULT, "deploy tool result"),
        (kind::ERROR, "deploy error"),
        (kind::THREAD, "deploy thread mirror"),
        (kind::STATUS, "deploy status"),
    ] {
        w.post_kind(&s, LOBBY_ID, k, "runner", t);
    }
    w.post(&s, "alice", "deploy token metro_rt_abcdef");
    w.post(&s, "alice", "   ");
    w.chat("alice", kind::TOOL_RESULT, "deploy chat tool result");

    let r = w
        .search(&fake, "alice", false, Scope::All, None, "deploy")
        .await
        .unwrap();
    let mut got = texts(&r);
    got.sort();
    assert_eq!(got, ["deploy agent reply", "deploy in thread"]);
    let thread_hit = r
        .results
        .iter()
        .find(|x| x.text == "deploy in thread")
        .unwrap();
    assert_eq!(thread_hit.room_id.as_deref(), Some(thread.id.as_str()));
    for (_, t) in fake.embedded() {
        assert!(
            ["deploy", "deploy agent reply", "deploy in thread"].contains(&t.as_str()),
            "{t} was embedded"
        );
    }
}

#[tokio::test]
async fn ties_are_deterministic() {
    let w = World::new();
    let fake = Fake::new();
    fake.allow(None, "fp");
    let first = w.chat("alice", kind::MESSAGE, "deploy");
    let second = w.chat("alice", kind::AGENT, "deploy");
    let r = w
        .search(&fake, "alice", false, Scope::Chat, None, "deploy")
        .await
        .unwrap();
    let ids: Vec<i64> = r.results.iter().map(|x| x.event_id).collect();
    assert_eq!(ids, [second, first], "newer first among equal scores");
}

#[tokio::test]
async fn requests_are_validated() {
    assert!(matches!(
        check_query("   "),
        Err(SearchError::BadRequest(_))
    ));
    assert!(matches!(
        check_query(&"x".repeat(MAX_QUERY_CHARS + 1)),
        Err(SearchError::BadRequest(_))
    ));
    assert_eq!(check_query("  hi ").unwrap(), "hi");
    assert_eq!(check_limit(None).unwrap(), DEFAULT_LIMIT);
    assert!(check_limit(Some(0)).is_err());
    assert!(check_limit(Some(MAX_LIMIT + 1)).is_err());
    let w = World::new();
    assert!(matches!(
        resolve_sources(&w.db, "alice", Scope::Space, None).await,
        Err(SearchError::BadRequest(_))
    ));
    assert!(matches!(
        resolve_sources(&w.db, "alice", Scope::Chat, Some("s-x".into())).await,
        Err(SearchError::BadRequest(_))
    ));
    let fake = Fake::new();
    let e = Engine::new(
        &w.db,
        "alice",
        &fake,
        Routing {
            embedding: None,
            classifier: None,
        },
    )
    .search("deploy", &[Source::Chat], 0, 10)
    .await
    .unwrap_err();
    assert_eq!(e, SearchError::NoEmbeddingModel);
    assert!(fake.embedded().is_empty());
}

#[test]
fn vectors_are_checked_and_top_k_is_ordered() {
    assert_eq!(vector::normalize(vec![]), Err(vector::VectorError::Empty));
    assert_eq!(
        vector::normalize(vec![0.0, 0.0]),
        Err(vector::VectorError::Zero)
    );
    assert_eq!(
        vector::normalize(vec![f32::NAN]),
        Err(vector::VectorError::NotFinite)
    );
    let v = vector::normalize(vec![3.0, 4.0]).unwrap();
    assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
    assert!(matches!(
        vector::normalize_batch(vec![vec![1.0, 0.0], vec![1.0]], None),
        Err(vector::VectorError::Mismatch {
            expected: 2,
            got: 1
        })
    ));
    let blob = vector::encode(&v);
    assert_eq!(vector::decode(&blob, 2).unwrap(), v);
    assert!(vector::decode(&blob, 3).is_none());

    let mut top = TopK::new(3);
    for (s, k) in [(0.1, 5), (0.9, 2), (0.5, 1), (0.9, 1), (0.3, 9)] {
        top.push(s, k, ());
    }
    let order: Vec<(f64, i32)> = top
        .into_sorted()
        .into_iter()
        .map(|(s, k, _)| (s, k))
        .collect();
    assert_eq!(order, [(0.9, 1), (0.9, 2), (0.5, 1)]);
    assert_eq!(clip("héllo", 2, "…"), "hé…");
}
