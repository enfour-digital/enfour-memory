use crate::{
    models::Models,
    store::{Chunk, Hit, Memory, Remember, Store},
};
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;
use std::path::Path;

pub const DEFAULT_RERANK_CANDIDATES: usize = 64;

pub struct Engine {
    pub store: Store,
    pub models: Option<Models>,
    _index_lock: std::fs::File,
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
                "embedding model changed; stop the service and run reindex first"
            );
            store.db.execute(
                "INSERT OR IGNORE INTO metadata VALUES('embedding_identity',?)",
                [&m.identity],
            )?;
        }
        Ok(Self {
            store,
            models,
            _index_lock: index_lock,
        })
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
        tx.commit()?;
        Ok(count)
    }
    pub fn remember(&mut self, r: Remember) -> Result<Memory> {
        Store::validate(&r)?;
        // Check the actual tokenizer budget; preserve the complete source record.
        let heading = format!("{} — {}", r.key, r.title);
        let texts = if let Some(m) = self.models.as_mut() {
            m.chunks(&heading, &r.content)?
        } else {
            r.content
                .chars()
                .collect::<Vec<_>>()
                .chunks(700)
                .map(|c| format!("{}\n{}", heading, c.iter().collect::<String>()))
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
        let vector = if let Some(m) = self.models.as_mut() {
            Some(m.embed(vec![query.into()], true)?.remove(0))
        } else {
            None
        };
        let mut hits =
            self.store
                .candidates_with_limit(scope, query, vector.as_deref(), candidates)?;
        if !hits.is_empty()
            && let Some(m) = self.models.as_mut()
        {
            let docs: Vec<_> = hits.iter().map(|h| h.excerpt.clone()).collect();
            for (i, score) in m.rerank(query, &docs)? {
                if let Some(h) = hits.get_mut(i) {
                    h.rerank_score = Some(score)
                }
            }
            hits.sort_by(|a, b| {
                b.rerank_score
                    .unwrap_or(f32::NEG_INFINITY)
                    .total_cmp(&a.rerank_score.unwrap_or(f32::NEG_INFINITY))
                    .then(a.memory.id.cmp(&b.memory.id))
            });
        }
        hits.truncate(limit);
        Ok(hits)
    }
}
