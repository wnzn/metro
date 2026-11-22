//! The search pipeline, independent of how model calls are made.
//!
//! [`Models`] is the seam to the provider layer: production uses
//! `upstream::ProviderModels`, tests a recording fake. Every model call
//! names the destination it is made for (`None` for personal Chat, the
//! Space id otherwise) and carries only text of that one source.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::store::{self, EventRow, NewVector, StoreOutcome};
use super::vector::{self, TopK};
use super::*;
use crate::db::Db;

/// A failed model call. The message is shown to the caller, so it must
/// never contain key material; the provider layer's messages are written
/// that way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelError(pub String);

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub type ModelFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ModelError>> + Send + 'a>>;

/// The caller's model calls. `dest` is the destination the call is for:
/// `None` for personal Chat, `Some(space_id)` for a Space.
pub trait Models: Send + Sync {
    /// Identifies the embedding model the connection resolves to for
    /// `dest`; it changes whenever the connection is reconfigured. An error
    /// means the connection may not be used for `dest`.
    fn fingerprint<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
    ) -> ModelFuture<'a, String>;
    fn embed<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
        inputs: Vec<String>,
    ) -> ModelFuture<'a, Vec<Vec<f32>>>;
    /// One System One request; returns the response (or its `answers`).
    fn decide<'a>(
        &'a self,
        dest: Option<&'a str>,
        connection: &'a str,
        state: Value,
        questions: Value,
    ) -> ModelFuture<'a, Value>;
}

/// The caller's chosen connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routing {
    pub embedding: Option<String>,
    pub classifier: Option<String>,
}

pub struct Engine<'a> {
    db: &'a Db,
    user: &'a str,
    models: &'a dyn Models,
    routing: Routing,
}

/// Request-wide bounds.
struct Budget {
    embed_calls: usize,
    scan: usize,
}

#[derive(Default)]
struct Report {
    notices: Vec<String>,
    partial: bool,
    /// Sources whose model could not be used, by first error.
    failed: Vec<String>,
    /// Sources that were searched or indexed.
    served: usize,
    indexed: u64,
    skipped: u64,
    remaining: u64,
    budget_hit: bool,
    full_sources: usize,
}

impl Report {
    fn notice(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !self.notices.contains(&text) {
            self.notices.push(text);
        }
    }

    fn text(&self) -> Option<String> {
        (!self.notices.is_empty()).then(|| self.notices.join(" "))
    }
}

/// How one candidate sorts: score first (descending, applied by
/// [`vector::rank_cmp`]), then Chat before Spaces, then Space id, room id,
/// and newer events first.
type TieKey = (u8, String, String, Reverse<i64>);

fn tie_key(source: &Source, room: &Option<String>, event_id: i64) -> TieKey {
    match source {
        Source::Chat => (0, String::new(), String::new(), Reverse(event_id)),
        Source::Space(sid) => (
            1,
            sid.clone(),
            room.clone().unwrap_or_default(),
            Reverse(event_id),
        ),
    }
}

#[derive(Debug, Clone)]
struct Candidate {
    source: Source,
    room_id: Option<String>,
    event_id: i64,
    cosine: f64,
}

#[derive(Debug, Clone)]
struct Found {
    candidate: Candidate,
    actor: String,
    text: String,
    classifier: Option<f64>,
}

/// One source that passed its model check, with its fingerprint and state.
struct Ready {
    source: Source,
    fingerprint: String,
}

impl<'a> Engine<'a> {
    pub fn new(db: &'a Db, user: &'a str, models: &'a dyn Models, routing: Routing) -> Self {
        Self {
            db,
            user,
            models,
            routing,
        }
    }

    fn embedding(&self) -> Result<&str, SearchError> {
        self.routing
            .embedding
            .as_deref()
            .ok_or(SearchError::NoEmbeddingModel)
    }

    /// Fingerprint each source; sources whose connection may not be used
    /// for them are left out and reported.
    async fn ready(
        &self,
        sources: &[Source],
        report: &mut Report,
    ) -> Result<Vec<Ready>, SearchError> {
        let conn = self.embedding()?;
        let user = self.user.to_owned();
        self.db
            .call(move |db| Ok(store::purge_revoked(&mut db.lock(), &user)?))
            .await?;
        let mut ready = Vec::with_capacity(sources.len());
        let mut skipped_spaces = 0usize;
        for source in sources {
            match self.models.fingerprint(source.destination(), conn).await {
                Ok(fp) => ready.push(Ready {
                    source: source.clone(),
                    fingerprint: fp,
                }),
                Err(e) => {
                    tracing::debug!(error = %e, source = %source.key(), "search source skipped");
                    report.partial = true;
                    report.failed.push(e.0.clone());
                    match source {
                        Source::Chat => {
                            report.notice(format!("Your personal Chat was not searched: {e}."))
                        }
                        Source::Space(_) => skipped_spaces += 1,
                    }
                }
            }
        }
        if skipped_spaces > 0 {
            report.notice(format!(
                "{skipped_spaces} Space(s) were not searched because your embedding \
                 connection cannot be used for them."
            ));
        }
        Ok(ready)
    }

    /// Index up to `batches` batches of `source`. Returns false when the
    /// source must not be searched (its model returned unusable vectors).
    async fn index_source(
        &self,
        ready: &Ready,
        batches: usize,
        budget: &mut Budget,
        report: &mut Report,
    ) -> Result<bool, SearchError> {
        let conn = self.embedding()?.to_owned();
        let source = &ready.source;
        for _ in 0..batches {
            let (user, src, fp) = (
                self.user.to_owned(),
                source.clone(),
                ready.fingerprint.clone(),
            );
            let (state, rows) = self
                .db
                .call(move |db| {
                    let mut c = db.lock();
                    let state = store::ensure_source(&mut c, &user, &src, &fp)?;
                    let rows = store::events_after(&c, &user, &src, state.cursor, INDEX_BATCH)?;
                    Ok((state, rows))
                })
                .await?;
            let Some(last) = rows.last().map(|r| r.id) else {
                return Ok(true);
            };
            let mut keep: Vec<(EventRow, String)> = Vec::with_capacity(rows.len());
            for row in rows {
                if indexable(&row.text) {
                    let text = embed_text(&row.text);
                    keep.push((row, text));
                } else {
                    report.skipped += 1;
                }
            }
            let mut vectors = Vec::new();
            let mut dims = state.dims;
            if !keep.is_empty() {
                if budget.embed_calls == 0 {
                    report.budget_hit = true;
                    return Ok(true);
                }
                if !self.still_readable(source).await? {
                    return Ok(false);
                }
                budget.embed_calls -= 1;
                let inputs: Vec<String> = keep.iter().map(|(_, t)| t.clone()).collect();
                let n = inputs.len();
                let out = match tokio::time::timeout(
                    EMBED_TIMEOUT,
                    self.models.embed(source.destination(), &conn, inputs),
                )
                .await
                {
                    Ok(Ok(out)) => out,
                    Ok(Err(e)) => {
                        report.partial = true;
                        report.notice(format!("Some messages could not be indexed: {e}."));
                        return Ok(true);
                    }
                    Err(_) => {
                        report.partial = true;
                        report.notice(
                            "Some messages could not be indexed: the embedding model timed out.",
                        );
                        return Ok(true);
                    }
                };
                if out.len() != n {
                    report.partial = true;
                    report.notice("The embedding model returned the wrong number of vectors; nothing was indexed from that batch.");
                    return Ok(true);
                }
                let out = match vector::normalize_batch(out, state.dims) {
                    Ok(v) => v,
                    Err(e) => {
                        report.partial = true;
                        report.notice(format!("{e}; reconfigure the connection to reindex."));
                        // Mixed dimensions under one fingerprint: never
                        // compare these vectors with the stored ones.
                        return Ok(!matches!(e, vector::VectorError::Mismatch { .. }));
                    }
                };
                dims = out.first().map(Vec::len).or(dims);
                vectors = keep
                    .iter()
                    .zip(out)
                    .map(|((row, text), v)| NewVector {
                        event_id: row.id,
                        room_id: row.room_id.clone(),
                        text_hash: store::text_hash(text),
                        vector: vector::encode(&v),
                    })
                    .collect();
            }
            let (user, src, fp) = (
                self.user.to_owned(),
                source.clone(),
                ready.fingerprint.clone(),
            );
            let from = state.cursor;
            let (outcome, stored) = self
                .db
                .call(move |db| {
                    Ok(store::store_batch(
                        &mut db.lock(),
                        &user,
                        &src,
                        &fp,
                        from,
                        last,
                        dims,
                        &vectors,
                        MAX_VECTORS_PER_SOURCE,
                    )?)
                })
                .await?;
            report.indexed += stored as u64;
            match outcome {
                // The source is full; `waiting` reports it.
                StoreOutcome::Stored if stored < keep.len() => return Ok(true),
                StoreOutcome::Stored => {}
                // Someone else's request moved this source on; theirs wins.
                StoreOutcome::Raced => return Ok(true),
                StoreOutcome::Revoked => return Ok(false),
            }
        }
        Ok(true)
    }

    async fn still_readable(&self, source: &Source) -> Result<bool, SearchError> {
        let (user, src) = (self.user.to_owned(), source.clone());
        Ok(self
            .db
            .call(move |db| Ok(store::can_read(&db.lock(), &user, &src)?))
            .await?)
    }

    /// Record what is still waiting in `ready`'s source: a count the client
    /// can index by calling again, or a full source that it cannot.
    async fn waiting(&self, ready: &Ready, report: &mut Report) -> Result<(), SearchError> {
        let (user, src, fp) = (
            self.user.to_owned(),
            ready.source.clone(),
            ready.fingerprint.clone(),
        );
        let (waiting, stored) = self
            .db
            .call(move |db| {
                let mut c = db.lock();
                let state = store::ensure_source(&mut c, &user, &src, &fp)?;
                let waiting = store::count_after(&c, &user, &src, state.cursor)?;
                Ok((waiting, store::vector_count(&c, &user, &src)?))
            })
            .await?;
        if waiting > 0 && stored >= MAX_VECTORS_PER_SOURCE {
            report.full_sources += 1;
        } else {
            report.remaining += waiting;
        }
        Ok(())
    }

    fn finish_index_report(&self, report: &mut Report, left_out: usize) {
        if left_out > 0 {
            report.partial = true;
            report.notice(format!(
                "{left_out} more Space(s) were not included; search one Space at a time to reach them."
            ));
        }
        if report.full_sources > 0 {
            report.partial = true;
            report.notice(format!(
                "The search index is full ({MAX_VECTORS_PER_SOURCE} messages per Chat or Space); older messages beyond that are not searched."
            ));
        }
        if report.remaining > 0 {
            report.partial = true;
            report.notice(format!(
                "{} message(s) are not indexed yet; search again, or index them with POST /v1/search/index.",
                report.remaining
            ));
        }
    }

    /// `POST /v1/search/index`.
    pub async fn index(
        &self,
        sources: &[Source],
        left_out: usize,
    ) -> Result<IndexResponse, SearchError> {
        let mut report = Report::default();
        let ready = self.ready(sources, &mut report).await?;
        if ready.is_empty() {
            return Err(SearchError::Upstream(first_failure(&report)));
        }
        let mut budget = Budget {
            embed_calls: INDEX_EMBED_CALLS,
            scan: 0,
        };
        for r in &ready {
            if self
                .index_source(r, INDEX_BATCHES_PER_SOURCE, &mut budget, &mut report)
                .await?
            {
                self.waiting(r, &mut report).await?;
            }
        }
        self.finish_index_report(&mut report, left_out);
        let done = report.remaining == 0 && report.failed.is_empty() && left_out == 0;
        Ok(IndexResponse {
            indexed: report.indexed,
            skipped: report.skipped,
            remaining: report.remaining,
            done,
            notice: report.text(),
        })
    }

    /// The query's vector for one source, cached per caller, model and
    /// destination.
    async fn query_vector(
        &self,
        ready: &Ready,
        query: &str,
        budget: &mut Budget,
    ) -> Result<Arc<Vec<f32>>, ModelError> {
        let key = cache_key(
            self.user,
            &ready.fingerprint,
            ready.source.destination(),
            query,
        );
        if let Some(v) = cache_get(&key) {
            return Ok(v);
        }
        if budget.embed_calls == 0 {
            return Err(ModelError(
                "this search made as many model calls as one search may".into(),
            ));
        }
        budget.embed_calls -= 1;
        let conn = self
            .embedding()
            .map_err(|_| ModelError("no embedding model".into()))?;
        let out = tokio::time::timeout(
            EMBED_TIMEOUT,
            self.models
                .embed(ready.source.destination(), conn, vec![query.to_owned()]),
        )
        .await
        .map_err(|_| ModelError("the embedding model timed out".into()))??;
        let [v] = <[Vec<f32>; 1]>::try_from(out).map_err(|_| {
            ModelError("the embedding model returned the wrong number of vectors".into())
        })?;
        let v = Arc::new(vector::normalize(v).map_err(|e| ModelError(e.to_string()))?);
        cache_put(key, v.clone());
        Ok(v)
    }

    /// Exact cosine top-`k` of one source's stored vectors.
    async fn scan(
        &self,
        ready: &Ready,
        query: &[f32],
        k: usize,
        budget: &mut Budget,
        report: &mut Report,
    ) -> Result<Vec<Candidate>, SearchError> {
        let mut top: TopK<TieKey, Candidate> = TopK::new(k);
        let mut before = i64::MAX;
        let dims = query.len();
        loop {
            if budget.scan == 0 {
                report.partial = true;
                report.notice("Not every message was compared in this search; narrow it to one Chat or Space.");
                break;
            }
            let page = SCAN_PAGE.min(budget.scan);
            let (user, src, fp) = (
                self.user.to_owned(),
                ready.source.clone(),
                ready.fingerprint.clone(),
            );
            let rows = self
                .db
                .call(move |db| {
                    Ok(store::vectors_page(
                        &db.lock(),
                        &user,
                        &src,
                        &fp,
                        before,
                        page,
                    )?)
                })
                .await?;
            let Some(last) = rows.last() else { break };
            before = last.event_id;
            budget.scan -= rows.len();
            let n = rows.len();
            for row in rows {
                let Some(v) = vector::decode(&row.vector, dims) else {
                    continue;
                };
                let score = vector::cosine(query, &v);
                let key = tie_key(&ready.source, &row.room_id, row.event_id);
                top.push(
                    score,
                    key,
                    Candidate {
                        source: ready.source.clone(),
                        room_id: row.room_id,
                        event_id: row.event_id,
                        cosine: score,
                    },
                );
            }
            if n < page {
                break;
            }
        }
        Ok(top.into_sorted().into_iter().map(|(_, _, c)| c).collect())
    }

    /// Current text of `candidates` the caller may still read, in the same
    /// order; the rest are dropped.
    async fn verify(&self, candidates: Vec<Candidate>) -> Result<Vec<Found>, SearchError> {
        let mut by_source: BTreeMap<Source, Vec<i64>> = BTreeMap::new();
        for c in &candidates {
            by_source
                .entry(c.source.clone())
                .or_default()
                .push(c.event_id);
        }
        let user = self.user.to_owned();
        let current: HashMap<(Source, i64), EventRow> = self
            .db
            .call(move |db| {
                let c = db.lock();
                let mut out = HashMap::new();
                for (src, ids) in by_source {
                    for row in store::current_events(&c, &user, &src, &ids)? {
                        out.insert((src.clone(), row.id), row);
                    }
                }
                Ok(out)
            })
            .await?;
        Ok(candidates
            .into_iter()
            .filter_map(|c| {
                let row = current.get(&(c.source.clone(), c.event_id))?;
                Some(Found {
                    actor: row.actor.clone(),
                    text: row.text.clone(),
                    candidate: c,
                    classifier: None,
                })
            })
            .collect())
    }

    /// Rerank the first [`SHORTLIST`] of `found` with System One. Returns
    /// how many were scored.
    async fn rerank(
        &self,
        query: &str,
        found: &mut [Found],
        report: &mut Report,
    ) -> Result<usize, SearchError> {
        let Some(classifier) = self.routing.classifier.as_deref() else {
            report.notice(
                "No System One connection is chosen, so results are ranked by embeddings only.",
            );
            return Ok(0);
        };
        let n = found.len().min(SHORTLIST);
        if n == 0 {
            return Ok(0);
        }
        // Re-check access right before any text leaves for System One.
        let mut readable: HashMap<Source, bool> = HashMap::new();
        for f in &found[..n] {
            let src = &f.candidate.source;
            if !readable.contains_key(src) {
                let ok = self.still_readable(src).await?;
                readable.insert(src.clone(), ok);
            }
        }
        let models = self.models;
        let mut calls = Vec::with_capacity(n);
        for (i, f) in found[..n].iter().enumerate() {
            if !readable[&f.candidate.source] {
                continue;
            }
            let dest = f.candidate.source.destination().map(str::to_owned);
            let state = rerank::state(query, &clip(f.text.trim(), MAX_RERANK_CHARS, ""));
            calls.push(Box::pin(async move {
                let out = tokio::time::timeout(
                    DECIDE_TIMEOUT,
                    models.decide(dest.as_deref(), classifier, state, rerank::questions()),
                )
                .await;
                (i, out)
            }));
        }
        let answers: Vec<_> = futures::stream::iter(calls)
            .buffer_unordered(RERANK_CONCURRENCY)
            .collect()
            .await;
        let mut scored = 0;
        let mut failed = 0;
        for (i, out) in answers {
            match out {
                Ok(Ok(value)) => match rerank::read_noul(&value) {
                    Some(noul) => {
                        found[i].classifier = Some(noul);
                        scored += 1;
                    }
                    None => failed += 1,
                },
                Ok(Err(e)) => {
                    tracing::debug!(error = %e, "System One rerank failed");
                    failed += 1;
                }
                Err(_) => failed += 1,
            }
        }
        if failed > 0 && scored == 0 {
            report.notice(
                "System One could not rerank the results, so they are ranked by embeddings only.",
            );
        } else if failed > 0 {
            report.notice(format!(
                "System One could not rerank {failed} result(s); those keep their embedding order after the reranked ones."
            ));
        }
        Ok(scored)
    }

    /// `POST /v1/search`.
    pub async fn search(
        &self,
        query: &str,
        sources: &[Source],
        left_out: usize,
        limit: usize,
    ) -> Result<SearchResponse, SearchError> {
        let query = check_query(query)?;
        let mut report = Report::default();
        let ready = self.ready(sources, &mut report).await?;
        let mut budget = Budget {
            embed_calls: SEARCH_EMBED_CALLS,
            scan: MAX_SCAN_PER_REQUEST,
        };
        // Over-fetch so results dropped by the final access check can be
        // replaced; at least the shortlist, so reranking has candidates.
        let k = (limit * 2).max(SHORTLIST);
        let mut merged: TopK<TieKey, Candidate> = TopK::new(k);
        for r in &ready {
            if !self
                .index_source(r, SEARCH_BATCHES_PER_SOURCE, &mut budget, &mut report)
                .await?
            {
                continue;
            }
            self.waiting(r, &mut report).await?;
            let q = match self.query_vector(r, &query, &mut budget).await {
                Ok(q) => q,
                Err(e) => {
                    report.partial = true;
                    report.failed.push(e.0.clone());
                    report.notice(match &r.source {
                        Source::Chat => format!("Your personal Chat was not searched: {e}."),
                        Source::Space(_) => format!("A Space was not searched: {e}."),
                    });
                    continue;
                }
            };
            report.served += 1;
            for c in self.scan(r, &q, k, &mut budget, &mut report).await? {
                let key = tie_key(&c.source, &c.room_id, c.event_id);
                merged.push(c.cosine, key, c);
            }
        }
        if report.served == 0 && !report.failed.is_empty() {
            return Err(SearchError::Upstream(first_failure(&report)));
        }
        if report.budget_hit {
            report.partial = true;
        }
        self.finish_index_report(&mut report, left_out);

        let candidates: Vec<Candidate> = merged
            .into_sorted()
            .into_iter()
            .map(|(_, _, c)| c)
            .collect();
        let mut found = self.verify(candidates).await?;
        found.truncate(limit);
        let scored = self.rerank(&query, &mut found, &mut report).await?;

        // Reranked first, by noul; the rest in embedding order.
        let (mut ranked, rest): (Vec<Found>, Vec<Found>) =
            found.into_iter().partition(|f| f.classifier.is_some());
        ranked.sort_by(|a, b| {
            let ka = tie_key(
                &a.candidate.source,
                &a.candidate.room_id,
                a.candidate.event_id,
            );
            let kb = tie_key(
                &b.candidate.source,
                &b.candidate.room_id,
                b.candidate.event_id,
            );
            vector::rank_cmp(
                (a.classifier.unwrap_or(0.0), &ka),
                (b.classifier.unwrap_or(0.0), &kb),
            )
        });
        ranked.extend(rest);

        // Final access check on exactly what is returned.
        let order: Vec<Candidate> = ranked.iter().map(|f| f.candidate.clone()).collect();
        let still: std::collections::HashSet<(Source, i64)> = self
            .verify(order)
            .await?
            .into_iter()
            .map(|f| (f.candidate.source, f.candidate.event_id))
            .collect();
        ranked.retain(|f| still.contains(&(f.candidate.source.clone(), f.candidate.event_id)));
        let reranked = ranked.iter().filter(|f| f.classifier.is_some()).count();
        let ranking = if reranked == 0 {
            Ranking::Embeddings
        } else if reranked == ranked.len() {
            Ranking::Classifier
        } else {
            Ranking::Mixed
        };
        if ranking == Ranking::Mixed && scored == SHORTLIST {
            report.notice(format!(
                "System One reranked the top {SHORTLIST}; the rest follow in embedding order."
            ));
        }
        let results: Vec<SearchResult> = ranked
            .into_iter()
            .map(|f| SearchResult {
                space_id: f.candidate.source.space_id(),
                room_id: f.candidate.room_id,
                event_id: f.candidate.event_id,
                actor: f.actor,
                text: clip(&f.text, MAX_RESULT_CHARS, "…"),
                score: f.classifier.unwrap_or(f.candidate.cosine.clamp(0.0, 1.0)),
            })
            .collect();
        Ok(SearchResponse {
            results,
            ranking,
            partial: report.partial,
            notice: report.text(),
        })
    }
}

fn first_failure(report: &Report) -> String {
    match report.failed.first() {
        Some(e) => format!("search is unavailable: {e}"),
        None => "search is unavailable".to_owned(),
    }
}

// Query embeddings, cached per caller, model fingerprint, destination and
// query; only the vector is kept, never the query text.
const CACHE_ENTRIES: usize = 256;
const CACHE_TTL: Duration = Duration::from_secs(600);

type Cache = Mutex<HashMap<[u8; 32], (Arc<Vec<f32>>, Instant)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(user: &str, fingerprint: &str, dest: Option<&str>, query: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    for part in [user, fingerprint, dest.unwrap_or("\u{0}chat"), query] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().into()
}

fn cache_get(key: &[u8; 32]) -> Option<Arc<Vec<f32>>> {
    let mut c = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match c.get(key) {
        Some((v, at)) if at.elapsed() < CACHE_TTL => Some(v.clone()),
        Some(_) => {
            c.remove(key);
            None
        }
        None => None,
    }
}

fn cache_put(key: [u8; 32], v: Arc<Vec<f32>>) {
    let mut c = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.len() >= CACHE_ENTRIES {
        c.retain(|_, (_, at)| at.elapsed() < CACHE_TTL);
        if c.len() >= CACHE_ENTRIES
            && let Some(oldest) = c.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| *k)
        {
            c.remove(&oldest);
        }
    }
    c.insert(key, (v, Instant::now()));
}
