//! The library search pipeline, independent of how files are stored and
//! how model calls are made ([`Library`], [`MediaModels`]).
//!
//! Every model call names the destination it is made for and carries only
//! one source's content: a file's bytes go only to a call for that file's
//! own destination, a query only to calls for sources that hold indexed
//! files, and a document excerpt only to a System One call for its own
//! source.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::protocol::{self, Kind};
use super::store::{self, Indexed, NewAsset, StoreOutcome};
use super::*;
use crate::db::Db;
use crate::search::vector::{self, TopK};

pub struct Engine<'a> {
    db: &'a Db,
    user: &'a str,
    library: &'a dyn Library,
    models: &'a dyn MediaModels,
    routing: Routing,
    selection_revision: Option<i64>,
    embedding_fingerprint: OnceLock<String>,
}

#[derive(Default)]
struct Report {
    notices: Vec<String>,
    partial: bool,
    failed: Vec<String>,
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

/// Score descending (applied by [`vector::rank_cmp`]), then the personal
/// library before Spaces, then Space id, asset id and segment order.
type TieKey = (Source, String, i64);

#[derive(Debug, Clone)]
struct Candidate {
    source: Source,
    asset_id: String,
    seq: i64,
    kind: Kind,
    cosine: f64,
}

#[derive(Debug, Clone)]
struct Found {
    candidate: Candidate,
    info: AssetInfo,
    indexed: Indexed,
    classifier: Option<f64>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn switched() -> LibrarySearchError {
    LibrarySearchError::Unusable(
        "your media embedding connection changed, or its share was revoked, while the file \
         was indexed; nothing was stored"
            .into(),
    )
}

fn same_file(a: &AssetInfo, b: &AssetInfo) -> bool {
    a.id == b.id && a.space_id == b.space_id && a.owner_id == b.owner_id && a.sha256 == b.sha256
}

impl<'a> Engine<'a> {
    pub fn new(
        db: &'a Db,
        user: &'a str,
        library: &'a dyn Library,
        models: &'a dyn MediaModels,
        routing: Routing,
    ) -> Self {
        Self {
            db,
            user,
            library,
            models,
            routing,
            selection_revision: None,
            embedding_fingerprint: OnceLock::new(),
        }
    }

    pub fn bind_selection(mut self, revision: i64) -> Self {
        self.selection_revision = Some(revision);
        self
    }

    async fn check_selection(&self) -> Result<(), LibrarySearchError> {
        if let Some(revision) = self.selection_revision {
            let (user, id) = (self.user.to_owned(), self.embedding()?.to_owned());
            let same = self
                .db
                .call(move |db| Ok(selection_is(&db.lock(), &user, &id, revision)?))
                .await?;
            if !same {
                return Err(LibrarySearchError::SelectionChanged);
            }
        }
        Ok(())
    }

    fn pin_fingerprint(&self, fingerprint: &str) -> Result<(), LibrarySearchError> {
        if self.selection_revision.is_some()
            && self
                .embedding_fingerprint
                .get_or_init(|| fingerprint.to_owned())
                != fingerprint
        {
            return Err(LibrarySearchError::Unusable(
                "the media embedding connection changed during this operation; try again".into(),
            ));
        }
        Ok(())
    }

    fn embedding(&self) -> Result<&str, LibrarySearchError> {
        self.routing
            .embedding
            .as_deref()
            .ok_or(LibrarySearchError::NoMediaModel)
    }

    async fn purge_revoked(&self) -> Result<(), LibrarySearchError> {
        let user = self.user.to_owned();
        self.db
            .call(move |db| Ok(store::purge_revoked(&mut db.lock(), &user)?))
            .await?;
        Ok(())
    }

    /// The write clock now: purges by this request only touch rows written
    /// at or before it (see `store.rs`).
    async fn snapshot(&self) -> Result<i64, LibrarySearchError> {
        Ok(self
            .db
            .call(move |db| Ok(store::snapshot(&db.lock())?))
            .await?)
    }

    /// Drop the caller's rows for one file written at or before `snapshot`:
    /// all of them, or those with another hash than `keep_hash`.
    async fn forget(
        &self,
        asset_id: &str,
        keep_hash: Option<&str>,
        snapshot: i64,
    ) -> Result<(), LibrarySearchError> {
        let (user, id, keep) = (
            self.user.to_owned(),
            asset_id.to_owned(),
            keep_hash.map(str::to_owned),
        );
        self.db
            .call(move |db| {
                Ok(store::forget_stale_asset(
                    &db.lock(),
                    &user,
                    &id,
                    keep.as_deref(),
                    snapshot,
                )?)
            })
            .await?;
        Ok(())
    }

    /// The connection is definitively unusable for `source`: drop the
    /// caller's rows there written at or before `snapshot`.
    async fn drop_source(
        &self,
        source: &Source,
        conn: &str,
        snapshot: i64,
    ) -> Result<usize, LibrarySearchError> {
        let (user, key, conn) = (self.user.to_owned(), source.key(), conn.to_owned());
        let revision = self.selection_revision;
        Ok(self
            .db
            .call(move |db| {
                Ok(store::purge_source(
                    &mut db.lock(),
                    &user,
                    &key,
                    (&conn, revision),
                    snapshot,
                )?)
            })
            .await?)
    }

    /// A connection failure while indexing: definitive ones drop the
    /// source's rows first.
    async fn unusable(
        &self,
        e: MediaError,
        source: &Source,
        conn: &str,
        snapshot: i64,
    ) -> LibrarySearchError {
        if e.kind == MediaFailure::Internal {
            return LibrarySearchError::Internal(e.message);
        }
        if e.kind == MediaFailure::Gone
            && let Err(err) = self.drop_source(source, conn, snapshot).await
        {
            return err;
        }
        LibrarySearchError::Unusable(e.message)
    }

    /// The library's answer, kept only when it agrees with Metro's own
    /// access rules: a personal file only for its owner, a Space file only
    /// for a current member of that Space.
    async fn readable(&self, asset_id: &str) -> Result<Option<AssetInfo>, LibrarySearchError> {
        if !crate::auth::valid_id(asset_id) {
            return Ok(None);
        }
        let Some(info) = self.library.visible(self.user, asset_id).await? else {
            return Ok(None);
        };
        self.authorized(info).await
    }

    async fn authorized(&self, info: AssetInfo) -> Result<Option<AssetInfo>, LibrarySearchError> {
        let ok = match &info.space_id {
            None => info.owner_id == self.user,
            Some(sid) => self.member(sid).await?,
        };
        Ok(ok.then_some(info))
    }

    async fn member(&self, space: &str) -> Result<bool, LibrarySearchError> {
        let (user, sid) = (self.user.to_owned(), space.to_owned());
        Ok(self
            .db
            .call(move |db| Ok(store::is_member(&db.lock(), &sid, &user)?))
            .await?)
    }

    async fn check_failed_search_scope(
        &self,
        sources: &[Source],
    ) -> Result<(), LibrarySearchError> {
        // Resolve explicit Space access again instead of inferring it from
        // the provider's failure. All-sources searches also include Personal.
        if let [Source::Space(sid)] = sources
            && !self.member(sid).await?
        {
            return Err(LibrarySearchError::NotFound("space"));
        }
        Ok(())
    }

    // ---- Indexing ----

    /// `POST /v1/library/index`. One id: errors are the response. Several:
    /// each file gets its own status.
    pub async fn index(
        &self,
        ids: &[String],
        bulk: bool,
    ) -> Result<IndexResponse, LibrarySearchError> {
        self.embedding()?;
        self.purge_revoked().await?;
        let mut items = Vec::with_capacity(ids.len());
        for id in ids {
            self.check_selection().await?;
            match self.index_one(id).await {
                Ok(item) => items.push(item),
                Err(e) if !bulk => return Err(e),
                Err(
                    e @ (LibrarySearchError::NoMediaModel
                    | LibrarySearchError::SelectionChanged
                    | LibrarySearchError::Busy
                    | LibrarySearchError::Full
                    | LibrarySearchError::Internal(_)),
                ) => return Err(e),
                Err(e) => {
                    let status = match &e {
                        LibrarySearchError::NotFound(_) => ItemStatus::NotFound,
                        LibrarySearchError::Unsupported(_)
                        | LibrarySearchError::NotMediaCapable => ItemStatus::Unsupported,
                        LibrarySearchError::TooLarge => ItemStatus::TooLarge,
                        _ => ItemStatus::Failed,
                    };
                    items.push(IndexItem {
                        asset_id: id.clone(),
                        status,
                        kind: None,
                        indexed_segments: 0,
                        partial: false,
                        error: Some(e.message()),
                    });
                }
            }
        }
        let indexed_segments = items.iter().map(|i| i.indexed_segments).sum();
        let partial = items
            .iter()
            .any(|i| i.partial || i.status != ItemStatus::Indexed);
        let notice = items.iter().any(|i| i.partial).then(|| {
            "The media model did not cover all of some files; only the parts it embedded are searchable."
                .to_owned()
        });
        Ok(IndexResponse {
            indexed_segments,
            partial,
            items,
            notice,
        })
    }

    async fn index_one(&self, asset_id: &str) -> Result<IndexItem, LibrarySearchError> {
        let conn = self.embedding()?;
        let snapshot = self.snapshot().await?;
        let info = self
            .readable(asset_id)
            .await?
            .ok_or(LibrarySearchError::NotFound("file"))?;
        let kind = Kind::of_mime(&info.mime_type).ok_or_else(|| {
            LibrarySearchError::Unsupported(protocol::unsupported_reason(&info.mime_type))
        })?;
        if info.size > MAX_ASSET_BYTES {
            return Err(LibrarySearchError::TooLarge);
        }
        let dest = info.space_id.clone();
        let source = Source::of(&info);
        match self.models.media_capable(dest.as_deref(), conn).await {
            Ok(true) => {}
            Ok(false) => return Err(LibrarySearchError::NotMediaCapable),
            Err(e) => return Err(self.unusable(e, &source, conn, snapshot).await),
        }
        let fingerprint = match self.models.fingerprint(dest.as_deref(), conn).await {
            Ok(fp) => fp,
            Err(e) => return Err(self.unusable(e, &source, conn, snapshot).await),
        };
        self.pin_fingerprint(&fingerprint)?;
        // What the connection resolves to now; the store transaction
        // requires the same.
        let grant = self.models.grant();
        let grant_key = {
            let (g, d, c) = (grant.clone(), dest.clone(), conn.to_owned());
            self.db
                .call(move |db| Ok(g(&db.lock(), d.as_deref(), &c)))
                .await?
        };
        let grant_key = match grant_key {
            Ok(k) => k,
            Err(e) => return Err(self.unusable(e, &source, conn, snapshot).await),
        };

        // Read the bytes as the caller, then check they are the file that
        // was authorized above.
        let (read, bytes) = match self.library.bytes(self.user, asset_id).await? {
            Some((read, bytes)) => match self.authorized(read).await? {
                Some(read) => (read, bytes),
                None => return Err(LibrarySearchError::NotFound("file")),
            },
            None => return Err(LibrarySearchError::NotFound("file")),
        };
        if !same_file(&info, &read) {
            return Err(LibrarySearchError::Unusable(
                "the file changed while it was read; index it again".into(),
            ));
        }
        if bytes.len() as u64 > MAX_ASSET_BYTES {
            return Err(LibrarySearchError::TooLarge);
        }
        let content_hash = sha256_hex(&bytes);
        if content_hash != read.sha256 {
            return Err(LibrarySearchError::Internal(
                "library bytes do not match their recorded hash".into(),
            ));
        }
        // An uploaded text file's own text, to derive excerpts from.
        let source_text = if protocol::base_mime(&read.mime_type).starts_with("text/") {
            Some(protocol::clean_text(&bytes).ok_or_else(|| {
                LibrarySearchError::Unsupported("text files must be UTF-8".into())
            })?)
        } else {
            None
        };

        // Right before the bytes leave: still readable, still the same file.
        self.still_same(&read).await?;
        self.check_selection().await?;
        let input = protocol::asset_request(&read.name, &read.mime_type, &bytes);
        drop(bytes);
        let result = tokio::time::timeout(
            INDEX_TIMEOUT,
            self.models.embed(
                dest.as_deref(),
                conn,
                input,
                self.selection_revision
                    .map(|rev| (rev, fingerprint.as_str())),
            ),
        )
        .await;

        // Check access before interpreting even an unsuccessful call: a
        // withheld reply after membership loss is a missing file, not a
        // broken connection. An access-check fault is internal.
        if let Err(e) = self.still_same(&read).await {
            if matches!(e, LibrarySearchError::NotFound(_)) {
                self.forget(asset_id, None, snapshot).await?;
            }
            return Err(e);
        }
        self.check_selection().await?;
        let answer = match result {
            Ok(Ok(a)) => a,
            Ok(Err(e)) if e.kind == MediaFailure::Upstream => {
                return Err(LibrarySearchError::Upstream(e.message));
            }
            Ok(Err(e)) => return Err(self.unusable(e, &source, conn, snapshot).await),
            Err(_) => {
                return Err(LibrarySearchError::Upstream(
                    "the media embedding model timed out".into(),
                ));
            }
        };
        let parsed = protocol::read_asset(&answer, kind, source_text.as_deref())
            .map_err(|e| LibrarySearchError::Upstream(e.0))?;

        // Before storing, the connection must still compute the same thing
        // for this destination.
        match self.models.fingerprint(dest.as_deref(), conn).await {
            Ok(fp) if fp == fingerprint => {}
            Ok(_) => return Err(switched()),
            Err(e) => return Err(self.unusable(e, &source, conn, snapshot).await),
        }
        // The store transaction revalidates the setting, the file and the
        // grant once more, atomically with the write.
        let (user, id, key, sid, c) = (
            self.user.to_owned(),
            asset_id.to_owned(),
            source.key(),
            read.space_id.clone(),
            conn.to_owned(),
        );
        let n = parsed.segments.len();
        let partial = parsed.partial;
        let selection_revision = self.selection_revision;
        let outcome = self
            .db
            .call(move |db| {
                store::store_asset(
                    &mut db.lock(),
                    &user,
                    &NewAsset {
                        selection_revision,
                        asset_id: &id,
                        source: &key,
                        space_id: sid.as_deref(),
                        connection_id: &c,
                        kind,
                        fingerprint: &fingerprint,
                        content_hash: &content_hash,
                        model: &parsed.model,
                        dims: parsed.dims,
                        partial: parsed.partial,
                        segments: &parsed.segments,
                    },
                    &grant,
                    &grant_key,
                    MAX_SEGMENTS_PER_USER,
                )
            })
            .await?;
        match outcome {
            StoreOutcome::Stored => Ok(IndexItem {
                asset_id: asset_id.to_owned(),
                status: ItemStatus::Indexed,
                kind: Some(kind),
                indexed_segments: n,
                partial,
                error: None,
            }),
            StoreOutcome::Revoked => Err(LibrarySearchError::NotFound("file")),
            StoreOutcome::Changed => Err(LibrarySearchError::Unusable(
                "the file changed while it was indexed; index it again".into(),
            )),
            StoreOutcome::Switched => Err(switched()),
            StoreOutcome::SelectionChanged => Err(LibrarySearchError::SelectionChanged),
            StoreOutcome::Full => Err(LibrarySearchError::Full),
        }
    }

    /// The file is still readable by the caller and is the same file.
    async fn still_same(&self, was: &AssetInfo) -> Result<(), LibrarySearchError> {
        match self.readable(&was.id).await? {
            Some(now) if same_file(was, &now) => Ok(()),
            _ => Err(LibrarySearchError::NotFound("file")),
        }
    }

    // ---- Searching ----

    async fn query_vector(
        &self,
        source: &Source,
        fingerprint: &str,
        query: &str,
    ) -> Result<Arc<Vec<f32>>, MediaError> {
        let key = cache_key(self.user, fingerprint, source.destination(), query);
        if let Some(v) = cache_get(&key) {
            return Ok(v);
        }
        let conn = self
            .embedding()
            .map_err(|_| MediaError::transient("no media embedding model"))?;
        let answer = tokio::time::timeout(
            QUERY_TIMEOUT,
            self.models.embed(
                source.destination(),
                conn,
                protocol::query_request(query),
                self.selection_revision.map(|rev| (rev, fingerprint)),
            ),
        )
        .await
        .map_err(|_| MediaError::transient("the media embedding model timed out"))??;
        let v = Arc::new(protocol::read_query(&answer).map_err(|e| MediaError::transient(e.0))?);
        cache_put(key, v.clone());
        Ok(v)
    }

    async fn indexed_count(&self, source: &Source) -> Result<usize, LibrarySearchError> {
        let (user, key) = (self.user.to_owned(), source.key());
        Ok(self
            .db
            .call(move |db| Ok(store::indexed_count(&db.lock(), &user, &key)?))
            .await?)
    }

    /// Drop rows of `source` built with another connection, fingerprint or
    /// (`dims`) dimension, written at or before `snapshot`.
    async fn purge_stale(
        &self,
        source: &Source,
        fingerprint: &str,
        dims: Option<usize>,
        snapshot: i64,
    ) -> Result<usize, LibrarySearchError> {
        let conn = self.embedding()?.to_owned();
        let revision = self.selection_revision;
        let (user, key, fp) = (self.user.to_owned(), source.key(), fingerprint.to_owned());
        Ok(self
            .db
            .call(move |db| {
                Ok(store::purge_stale(
                    &mut db.lock(),
                    &user,
                    &key,
                    (&conn, revision),
                    &fp,
                    dims,
                    snapshot,
                )?)
            })
            .await?)
    }

    #[allow(clippy::too_many_arguments)]
    async fn scan(
        &self,
        source: &Source,
        fingerprint: &str,
        query: &[f32],
        kind: Option<Kind>,
        k: usize,
        budget: &mut usize,
        report: &mut Report,
    ) -> Result<Vec<Candidate>, LibrarySearchError> {
        let mut top: TopK<TieKey, Candidate> = TopK::new(k);
        let mut after = 0i64;
        loop {
            if *budget == 0 {
                report.partial = true;
                report.notice(
                    "Not every indexed file was compared in this search; narrow it to one library.",
                );
                break;
            }
            let page = SCAN_PAGE.min(*budget);
            let (user, key, fp) = (self.user.to_owned(), source.key(), fingerprint.to_owned());
            let rows = self
                .db
                .call(move |db| {
                    Ok(store::segments_page(
                        &db.lock(),
                        &user,
                        &key,
                        &fp,
                        kind,
                        after,
                        page,
                    )?)
                })
                .await?;
            let Some(last) = rows.last() else { break };
            after = last.rowid;
            *budget -= rows.len();
            let n = rows.len();
            for row in rows {
                let Some(v) = vector::decode(&row.vector, query.len()) else {
                    continue;
                };
                let score = vector::cosine(query, &v);
                top.push(
                    score,
                    (source.clone(), row.asset_id.clone(), row.seq),
                    Candidate {
                        source: source.clone(),
                        asset_id: row.asset_id,
                        seq: row.seq,
                        kind: row.kind,
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

    /// The candidates the caller may still read, in order, with the
    /// library's current description and the stored locator. A file that
    /// is gone, moved, changed or no longer readable is dropped and its
    /// index rows are deleted.
    async fn verify(
        &self,
        candidates: Vec<Candidate>,
        snapshot: i64,
    ) -> Result<Vec<Found>, LibrarySearchError> {
        let mut infos: BTreeMap<String, Option<AssetInfo>> = BTreeMap::new();
        for c in &candidates {
            if !infos.contains_key(&c.asset_id) {
                let info = self.readable(&c.asset_id).await?;
                infos.insert(c.asset_id.clone(), info);
            }
        }
        let mut out = Vec::with_capacity(candidates.len());
        // Asset id -> the hash to keep (`None`: drop every row).
        let mut dropped: HashMap<String, Option<String>> = HashMap::new();
        for c in candidates {
            let Some(Some(info)) = infos.get(&c.asset_id) else {
                dropped.insert(c.asset_id.clone(), None);
                continue;
            };
            if Source::of(info) != c.source {
                dropped.insert(c.asset_id.clone(), None);
                continue;
            }
            let (user, id, seq) = (self.user.to_owned(), c.asset_id.clone(), c.seq);
            let indexed = self
                .db
                .call(move |db| Ok(store::segment(&db.lock(), &user, &id, seq)?))
                .await?;
            match indexed {
                Some(ix) if ix.content_hash == info.sha256 && ix.source == c.source.key() => out
                    .push(Found {
                        info: info.clone(),
                        indexed: ix,
                        candidate: c,
                        classifier: None,
                    }),
                Some(_) => {
                    dropped.insert(c.asset_id.clone(), Some(info.sha256.clone()));
                }
                None => {}
            }
        }
        for (id, keep) in dropped {
            self.forget(&id, keep.as_deref(), snapshot).await?;
        }
        Ok(out)
    }

    /// Rerank real document text excerpts among the first [`SHORTLIST`]
    /// documents. Images, audio and video are never sent. Returns how many
    /// were scored.
    async fn rerank(
        &self,
        query: &str,
        found: &mut [Found],
        report: &mut Report,
    ) -> Result<usize, LibrarySearchError> {
        let eligible: Vec<usize> = found
            .iter()
            .enumerate()
            .filter(|(_, f)| f.candidate.kind == Kind::Document && f.indexed.excerpt.is_some())
            .map(|(i, _)| i)
            .take(SHORTLIST)
            .collect();
        let Some(classifier) = self.routing.classifier.as_deref() else {
            if !eligible.is_empty() {
                report.notice(
                    "No System One connection is chosen, so results are ranked by embeddings only.",
                );
            }
            return Ok(0);
        };
        if eligible.is_empty() {
            return Ok(0);
        }
        // Re-check every file right before its excerpt leaves.
        let mut calls = Vec::with_capacity(eligible.len());
        for i in eligible {
            let f = &found[i];
            match self.readable(&f.info.id).await? {
                Some(now) if same_file(&f.info, &now) => {}
                _ => continue,
            }
            let dest = f.candidate.source.destination().map(str::to_owned);
            let text = crate::search::clip(
                f.indexed.excerpt.as_deref().unwrap_or_default(),
                MAX_RERANK_CHARS,
                "",
            );
            let state = rerank_state(query, &text);
            let models = self.models;
            let selection = self
                .selection_revision
                .zip(self.routing.embedding.as_deref())
                .map(|(rev, id)| (id, rev));
            calls.push(Box::pin(async move {
                let out = tokio::time::timeout(
                    DECIDE_TIMEOUT,
                    models.decide(
                        dest.as_deref(),
                        classifier,
                        state,
                        rerank_questions(),
                        selection,
                    ),
                )
                .await;
                (i, out)
            }));
        }
        let answers: Vec<_> = futures::stream::iter(calls)
            .buffer_unordered(RERANK_CONCURRENCY)
            .collect()
            .await;
        let (mut scored, mut failed) = (0, 0);
        for (i, out) in answers {
            match out.ok().and_then(Result::ok).as_ref().and_then(read_noul) {
                Some(n) => {
                    found[i].classifier = Some(n);
                    scored += 1;
                }
                None => failed += 1,
            }
        }
        if failed > 0 && scored == 0 {
            report.notice(
                "System One could not rerank the document excerpts, so results are ranked by embeddings only.",
            );
        } else if failed > 0 {
            report.notice(format!(
                "System One could not rerank {failed} document excerpt(s); those keep their embedding order."
            ));
        }
        Ok(scored)
    }

    /// `POST /v1/library/search`.
    pub async fn search(
        &self,
        query: &str,
        sources: &[Source],
        left_out: usize,
        limit: usize,
        kind: Option<Kind>,
    ) -> Result<SearchResponse, LibrarySearchError> {
        let query = check_query(query)?;
        self.embedding()?;
        self.purge_revoked().await?;
        let snapshot = self.snapshot().await?;
        let mut report = Report::default();
        let mut budget = MAX_SCAN_PER_REQUEST;
        let k = (limit * MAX_PER_ASSET).max(SHORTLIST) + SHORTLIST;
        let mut merged: TopK<TieKey, Candidate> = TopK::new(k);
        let (mut served, mut searchable, mut stale, mut skipped_spaces, mut removed) =
            (0, 0, 0, 0, 0);
        for source in sources {
            self.check_selection().await?;
            // Nothing is sent anywhere for a library with no indexed files.
            if self.indexed_count(source).await? == 0 {
                continue;
            }
            searchable += 1;
            let conn = self.embedding()?;
            let fp = match self.models.fingerprint(source.destination(), conn).await {
                Ok(fp) => fp,
                Err(e) if e.kind == MediaFailure::Internal => {
                    self.check_failed_search_scope(sources).await?;
                    return Err(LibrarySearchError::Internal(e.message));
                }
                Err(e) => {
                    report.partial = true;
                    report.failed.push(e.message.clone());
                    if e.kind == MediaFailure::Gone {
                        removed += self.drop_source(source, conn, snapshot).await?;
                    }
                    match source {
                        Source::Personal => {
                            report.notice(format!("Your personal files were not searched: {e}."))
                        }
                        Source::Space(_) => skipped_spaces += 1,
                    }
                    continue;
                }
            };
            self.pin_fingerprint(&fp)?;
            stale += self.purge_stale(source, &fp, None, snapshot).await?;
            if self.indexed_count(source).await? == 0 {
                continue;
            }
            let q = match self.query_vector(source, &fp, &query).await {
                Ok(q) => q,
                Err(e) if e.kind == MediaFailure::Internal => {
                    self.check_failed_search_scope(sources).await?;
                    return Err(LibrarySearchError::Internal(e.message));
                }
                Err(e) => {
                    report.partial = true;
                    report.failed.push(e.message.clone());
                    if e.kind == MediaFailure::Gone {
                        removed += self.drop_source(source, conn, snapshot).await?;
                    }
                    report.notice(match source {
                        Source::Personal => format!("Your personal files were not searched: {e}."),
                        Source::Space(_) => format!("A Space's files were not searched: {e}."),
                    });
                    continue;
                }
            };
            // A model that changed its dimension under the same fingerprint.
            stale += self
                .purge_stale(source, &fp, Some(q.len()), snapshot)
                .await?;
            served += 1;
            for c in self
                .scan(source, &fp, &q, kind, k, &mut budget, &mut report)
                .await?
            {
                let key = (c.source.clone(), c.asset_id.clone(), c.seq);
                merged.push(c.cosine, key, c);
            }
        }
        self.check_selection().await?;
        if served == 0 && !report.failed.is_empty() {
            self.check_failed_search_scope(sources).await?;
            return Err(LibrarySearchError::Upstream(format!(
                "file search is unavailable: {}",
                report.failed[0]
            )));
        }
        if skipped_spaces > 0 {
            report.notice(format!(
                "{skipped_spaces} Space(s) were not searched because your media embedding \
                 connection cannot be used for them."
            ));
        }
        if removed > 0 {
            report.notice(format!(
                "{removed} indexed file(s) were removed from your index because your media \
                 embedding connection can no longer be used for them."
            ));
        }
        if stale > 0 {
            report.partial = true;
            report.notice(format!(
                "{stale} file(s) were indexed with a different media model and were removed \
                 from your index; index them again to search them."
            ));
        }
        if left_out > 0 {
            report.partial = true;
            report.notice(format!(
                "{left_out} more Space(s) were not included; search one Space at a time to reach them."
            ));
        }
        if searchable == 0 {
            report.notice(
                "No indexed files here yet. Only files you index are searched; indexing sends \
                 a file to your media embedding model.",
            );
        }

        // At most MAX_PER_ASSET results from one file.
        let mut per_asset: HashMap<String, usize> = HashMap::new();
        let candidates: Vec<Candidate> = merged
            .into_sorted()
            .into_iter()
            .map(|(_, _, c)| c)
            .filter(|c| {
                let n = per_asset.entry(c.asset_id.clone()).or_default();
                *n += 1;
                *n <= MAX_PER_ASSET
            })
            .collect();
        let mut found = self.verify(candidates, snapshot).await?;
        found.truncate(limit);
        let scored = self.rerank(&query, &mut found, &mut report).await?;

        // Reranked excerpts are reordered only among the positions they
        // already hold; every other result keeps its embedding position.
        let slots: Vec<usize> = (0..found.len())
            .filter(|i| found[*i].classifier.is_some())
            .collect();
        let mut reranked: Vec<Found> = slots.iter().map(|i| found[*i].clone()).collect();
        reranked.sort_by(|a, b| {
            let ka = (
                a.candidate.source.clone(),
                a.candidate.asset_id.clone(),
                a.candidate.seq,
            );
            let kb = (
                b.candidate.source.clone(),
                b.candidate.asset_id.clone(),
                b.candidate.seq,
            );
            vector::rank_cmp(
                (a.classifier.unwrap_or(0.0), &ka),
                (b.classifier.unwrap_or(0.0), &kb),
            )
        });
        for (slot, f) in slots.into_iter().zip(reranked) {
            found[slot] = f;
        }

        // Final access check on exactly what is returned.
        let order: Vec<Candidate> = found.iter().map(|f| f.candidate.clone()).collect();
        let still: HashMap<(String, i64), Found> = self
            .verify(order, snapshot)
            .await?
            .into_iter()
            .map(|f| ((f.candidate.asset_id.clone(), f.candidate.seq), f))
            .collect();
        found.retain(|f| still.contains_key(&(f.candidate.asset_id.clone(), f.candidate.seq)));
        // Names and types as the library describes them now.
        for f in &mut found {
            if let Some(now) = still.get(&(f.candidate.asset_id.clone(), f.candidate.seq)) {
                f.info = now.info.clone();
            }
        }

        self.check_selection().await?;
        let n_ranked = found.iter().filter(|f| f.classifier.is_some()).count();
        let ranking = if n_ranked == 0 {
            Ranking::Embeddings
        } else if n_ranked == found.len() {
            Ranking::Classifier
        } else {
            Ranking::Mixed
        };
        if ranking == Ranking::Mixed && scored > 0 {
            report.notice(
                "System One reranked document excerpts among themselves; other results keep \
                 their embedding order and score.",
            );
        }
        let results = found
            .into_iter()
            .map(|f| {
                let (score, score_kind) = match f.classifier {
                    Some(n) => (n, ScoreKind::Classifier),
                    None => (f.candidate.cosine.clamp(0.0, 1.0), ScoreKind::Cosine),
                };
                SearchResult {
                    asset_id: f.info.id,
                    name: f.info.name,
                    mime_type: f.info.mime_type,
                    kind: f.candidate.kind,
                    space_id: f.info.space_id,
                    locator: f.indexed.locator,
                    excerpt: f.indexed.excerpt,
                    score,
                    score_kind,
                }
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

// ---- Settings ----

/// `GET /v1/library/settings`. `prior` is the person's message-search
/// embedding connection; it is suggested only when confirmed media-capable
/// for the personal library, and never used until saved.
pub async fn settings_view(
    db: &Db,
    models: &dyn MediaModels,
    user: &str,
    prior: Option<String>,
) -> Result<SettingsView, LibrarySearchError> {
    let u = user.to_owned();
    let (embedding_id, revision) = db
        .call(move |db| Ok(store::selection(&db.lock(), &u)?))
        .await?;
    let suggested_id = match (&embedding_id, prior) {
        (None, Some(p)) => match models.media_capable(None, &p).await {
            Ok(true) => Some(p),
            _ => None,
        },
        _ => None,
    };
    Ok(SettingsView {
        embedding_id,
        selection_precondition: true,
        selection_revision: revision.to_string(),
        suggested_id,
    })
}

/// A connection is saved only when it is
/// usable by the person and media-capable; changing it discards their
/// library index.
pub async fn save_settings(
    db: &Db,
    models: &dyn MediaModels,
    user: &str,
    embedding_id: Option<String>,
    expected: Option<(Option<String>, Option<i64>)>,
) -> Result<SettingsView, LibrarySearchError> {
    if let Some(id) = &embedding_id {
        if !crate::auth::valid_id(id) {
            return Err(LibrarySearchError::NotFound("connection"));
        }
        match models.media_capable(None, id).await {
            Ok(true) => {}
            Ok(false) => return Err(LibrarySearchError::NotMediaCapable),
            Err(e) if e.kind == MediaFailure::Internal => {
                return Err(LibrarySearchError::Internal(e.message));
            }
            Err(e) => return Err(LibrarySearchError::Unusable(e.message)),
        }
    }
    let (u, id) = (user.to_owned(), embedding_id.clone());
    let grant = models.grant();
    let outcome = db
        .call(move |db| {
            Ok(store::save_selection(
                &mut db.lock(),
                &u,
                id.as_deref(),
                expected.as_ref().map(|(id, _)| id.as_deref()),
                expected.as_ref().and_then(|(_, rev)| *rev),
                &grant,
            )?)
        })
        .await?;
    let revision = match outcome {
        store::SelectionWrite::Saved(revision) => revision,
        store::SelectionWrite::Stale => return Err(LibrarySearchError::SelectionChanged),
        store::SelectionWrite::Unusable(e) if e.kind == MediaFailure::Internal => {
            return Err(LibrarySearchError::Internal(e.message));
        }
        store::SelectionWrite::Unusable(e) => return Err(LibrarySearchError::Unusable(e.message)),
    };
    Ok(SettingsView {
        embedding_id,
        selection_precondition: true,
        selection_revision: revision.to_string(),
        suggested_id: None,
    })
}

// ---- System One rerank of document excerpts ----

pub const RERANK_QUESTION: &str = "relevant";

const RERANK_INSTRUCTIONS: &str = "A person searched their own files with the search query. \
The candidate excerpt is text taken from one of their documents. Is the excerpt relevant to what \
they are looking for: does it contain, discuss or directly answer what the query asks about?";
const RERANK_TRUE: &str = "The excerpt is about what the query asks for, or contains the \
information the query is looking for.";
const RERANK_FALSE: &str = "The excerpt is unrelated, or shares only words or a general topic \
with the query without being what the person is looking for.";

/// `state` for one query and excerpt pair; never more than one excerpt.
pub fn rerank_state(query: &str, excerpt: &str) -> Value {
    json!({ "search_query": query, "candidate_excerpt": excerpt })
}

pub fn rerank_questions() -> Value {
    json!({
        RERANK_QUESTION: {
            "type": "noul",
            "instructions": RERANK_INSTRUCTIONS,
            "criteria": { "true": RERANK_TRUE, "false": RERANK_FALSE },
        }
    })
}

/// A well-formed `noul` in 0..=1, or `None` (never guessed).
pub fn read_noul(answer: &Value) -> Option<f64> {
    let answers = answer.get("answers").unwrap_or(answer);
    let a = answers.get(RERANK_QUESTION)?;
    if a.get("type").and_then(Value::as_str) != Some("noul") {
        return None;
    }
    let n = a.get("noul")?.as_f64()?;
    (n.is_finite() && (0.0..=1.0).contains(&n)).then_some(n)
}

// ---- Query vector cache (vectors only, never the query text) ----

const CACHE_ENTRIES: usize = 256;
const CACHE_TTL: Duration = Duration::from_secs(600);

type Cache = Mutex<HashMap<[u8; 32], (Arc<Vec<f32>>, Instant)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(user: &str, fingerprint: &str, dest: Option<&str>, query: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    for part in [
        "metro.library-query.v1",
        user,
        fingerprint,
        dest.unwrap_or("\u{0}personal"),
        query,
    ] {
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

#[cfg(test)]
pub(super) fn clear_cache() {
    cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}
