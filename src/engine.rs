use crate::{
    cache::ExactCache,
    models::Models,
    store::{Chunk, Hit, Memory, Remember, Store},
};
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;
use std::{path::Path, sync::Arc};

pub const DEFAULT_RERANK_CANDIDATES: usize = 64;
pub const CHUNKING_IDENTITY: &str = "markdown-sentence-480-v1";

#[derive(Hash, PartialEq, Eq)]
struct RecallKey {
    scope: String,
    query: String,
    models: Option<String>,
    candidates: usize,
    generation: i64,
    expiry_epoch: Option<i64>,
}
fn hit_bytes(h: &Hit) -> usize {
    let m = &h.memory;
    size_of::<Hit>()
        + h.excerpt.len()
        + m.language.as_ref().map_or(0, |r| {
            size_of::<crate::language::Report>()
                + r.review.len()
                + r.versions.policy.len()
                + r.versions.validator.len()
                + r.versions.dictionary.len()
                + r.versions.glossary.len()
                + r.diagnostics
                    .iter()
                    .map(|d| {
                        size_of::<crate::language::Diagnostic>()
                            + d.field.len()
                            + d.rule.len()
                            + d.guidance.len()
                    })
                    .sum::<usize>()
        })
        + [
            &m.id, &m.scope, &m.key, &m.title, &m.content, &m.kind, &m.source,
        ]
        .iter()
        .map(|s| s.len())
        .sum::<usize>()
}

pub struct Engine {
    pub store: Store,
    pub models: Option<Models>,
    _index_lock: std::fs::File,
    results: ExactCache<RecallKey, Arc<Vec<Hit>>>,
    cached_generation: Option<i64>,
}
impl Engine {
    pub fn open(db: &Path, models: Option<&Path>) -> Result<Self> {
        let store = Store::open(db)?;
        let index_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(db)?;
        index_lock
            .try_lock_shared()
            .map_err(|e| anyhow::anyhow!("index maintenance in progress: {e}"))?;
        let models = models.map(Models::open).transpose()?;
        if let Some(m) = &models {
            let old: Option<String> = store
                .db
                .query_row(
                    "SELECT value FROM metadata WHERE key='embedding_identity'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            ensure!(
                old.as_ref().is_none_or(|s| s == &m.identity),
                "The embedding model changed. Stop the service and run reindex first."
            );
            store.db.execute(
                "INSERT OR IGNORE INTO metadata VALUES('embedding_identity',?)",
                [&m.identity],
            )?;
        }
        let old_chunker: Option<String> = store
            .db
            .query_row(
                "SELECT value FROM metadata WHERE key='chunking_identity'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let count: i64 = store
            .db
            .query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))?;
        ensure!(
            count == 0 || old_chunker.as_deref() == Some(CHUNKING_IDENTITY),
            "The chunk format changed. Stop the service and rebuild the index."
        );
        store.db.execute(
            "INSERT OR IGNORE INTO metadata VALUES('chunking_identity',?)",
            [CHUNKING_IDENTITY],
        )?;
        Ok(Self {
            store,
            models,
            _index_lock: index_lock,
            results: ExactCache::new(|k: &RecallKey, v: &Arc<Vec<Hit>>| {
                k.scope.len()
                    + k.query.len()
                    + k.models.as_ref().map_or(0, String::len)
                    + v.iter().map(hit_bytes).sum::<usize>()
            }),
            cached_generation: None,
        })
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_test(db: &Path, models: Option<&Path>) -> Result<Self> {
        let mut engine = Self::open(db, models)?;
        engine
            .store
            .set_language(crate::language::Language::test_fixture());
        Ok(engine)
    }
    pub fn set_cache_enabled(&mut self, enabled: bool) {
        self.store.language.set_cache_enabled(enabled);
        self.results.set_enabled(enabled);
        if let Some(models) = self.models.as_mut() {
            models.set_cache_enabled(enabled);
        }
    }
    pub fn cache_info(&self) -> serde_json::Value {
        serde_json::json!({"language":self.store.language.info(),"results":self.results.info(),"models":self.models.as_ref().map(Models::cache_info)})
    }
    /// Offline maintenance: replace derived chunks atomically, retaining all source history.
    pub fn reindex(db: &Path, model_path: &Path) -> Result<usize> {
        let mut store = Store::open(db)?;
        let index_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(db)?;
        index_lock
            .try_lock()
            .map_err(|e| anyhow::anyhow!("stop other Enfour processes before reindexing: {e}"))?;
        let mut models = Models::open(model_path)?;
        let tx = store
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut count = 0;
        {
            let mut q = tx.prepare("SELECT data FROM memories WHERE deleted=0 ORDER BY id")?;
            let mut rows = q.query([])?;
            tx.execute("DELETE FROM chunks", [])?;
            while let Some(row) = rows.next()? {
                let memory: Memory = serde_json::from_str(&row.get::<_, String>(0)?)?;
                let texts = models.chunks(
                    &format!("{} — {}", memory.key, memory.title),
                    &memory.content,
                )?;
                let vectors = models.embed(texts.clone(), false)?;
                for (text, vector) in texts.into_iter().zip(vectors) {
                    let bytes: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
                    tx.execute(
                        "INSERT INTO chunks(memory_id,text,vector) VALUES(?,?,?)",
                        rusqlite::params![memory.id, text, bytes],
                    )?;
                }
                count += 1;
            }
        }
        tx.execute("INSERT INTO metadata VALUES('embedding_identity',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[models.identity])?;
        tx.execute("INSERT INTO metadata VALUES('chunking_identity',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[CHUNKING_IDENTITY])?;
        tx.commit()?;
        Ok(count)
    }
    pub fn remember(&mut self, r: Remember) -> Result<Memory> {
        self.store.language.require(&r)?;
        // Check the actual tokenizer budget; preserve the complete source record.
        let heading = format!("{} — {}", r.key, r.title);
        let texts = if let Some(m) = self.models.as_mut() {
            m.chunks(&heading, &r.content)?
        } else {
            crate::language::text::pack(&r.content, |s| Ok(s.chars().count() <= 700))?
                .into_iter()
                .map(|s| format!("{heading}\n{s}"))
                .collect()
        };
        let vectors = if let Some(m) = self.models.as_mut() {
            Some(m.embed(texts.clone(), false)?)
        } else {
            None
        };
        let chunks: Vec<_> = texts
            .into_iter()
            .enumerate()
            .map(|(i, text)| Chunk {
                text,
                vector: vectors.as_ref().map(|v| v[i].clone()),
            })
            .collect();
        self.store.put(r, &chunks)
    }
    pub fn recall(&mut self, scope: &str, query: &str, limit: usize) -> Result<Vec<Hit>> {
        let candidates = if self.models.is_some() {
            DEFAULT_RERANK_CANDIDATES
        } else {
            16
        };
        self.recall_with_candidates(scope, query, limit, candidates)
    }
    pub fn recall_with_candidates(
        &mut self,
        scope: &str,
        query: &str,
        limit: usize,
        candidates: usize,
    ) -> Result<Vec<Hit>> {
        ensure!(
            !query.trim().is_empty() && query.len() <= 4000,
            "query must contain 1..4000 bytes"
        );
        ensure!((1..=16).contains(&limit), "limit must be 1..16");
        ensure!(
            (1..=128).contains(&candidates),
            "candidate limit must be 1..128"
        );
        ensure!(
            !scope.trim().is_empty() && scope.len() <= 512 && !scope.contains('\0'),
            "invalid scope"
        );
        // Pin the SQLite snapshot before reading its generation. A concurrent
        // commit cannot pair an old generation with new candidates (or vice versa).
        let tx = self.store.db.unchecked_transaction()?;
        for _ in 0..3 {
            let at = crate::store::now();
            let (generation, expiry_epoch, next_expiry) = self.store.retrieval_state(scope, at)?;
            if self.cached_generation != Some(generation) {
                self.results.clear();
                self.cached_generation = Some(generation);
            }
            let key = RecallKey {
                scope: scope.into(),
                query: query.into(),
                models: self.models.as_ref().map(|m| m.cache_identity.clone()),
                candidates,
                generation,
                expiry_epoch,
            };
            let cached = self.results.get(&key);
            let mut hits = if let Some(hits) = cached {
                (*hits).clone()
            } else {
                let vector = if let Some(m) = self.models.as_mut() {
                    Some(m.embed(vec![query.into()], true)?.remove(0))
                } else {
                    None
                };
                let mut hits =
                    self.store
                        .candidates_at(scope, query, vector.as_deref(), candidates, at)?;
                if !hits.is_empty()
                    && let Some(m) = self.models.as_mut()
                {
                    let docs: Vec<_> = hits.iter().map(|h| h.excerpt.clone()).collect();
                    for (i, score) in m.rerank(query, &docs)? {
                        if let Some(h) = hits.get_mut(i) {
                            h.rerank_score = Some(score);
                        }
                    }
                    hits.sort_by(|a, b| {
                        b.rerank_score
                            .unwrap_or(f32::NEG_INFINITY)
                            .total_cmp(&a.rerank_score.unwrap_or(f32::NEG_INFINITY))
                            .then(a.memory.id.cmp(&b.memory.id))
                    });
                }
                // All allowed result limits are prefixes of this same ranking.
                hits.truncate(16);
                self.results.insert(key, Arc::new(hits.clone()));
                hits
            };
            let finished = crate::store::now();
            if finished < at || next_expiry.is_some_and(|expiry| finished >= expiry) {
                continue;
            }
            tx.commit()?;
            hits.truncate(limit);
            return Ok(hits);
        }
        anyhow::bail!("The memory expiry changed during retrieval. Try again.")
    }
}
