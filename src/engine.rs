use crate::{
    models::Models,
    store::{Chunk, Hit, Memory, Remember, Store},
};
use anyhow::{Result, ensure};
use rusqlite::OptionalExtension;
use std::path::Path;

pub struct Engine {
    pub store: Store,
    pub models: Option<Models>,
}
impl Engine {
    pub fn open(db: &Path, models: Option<&Path>) -> Result<Self> {
        let store = Store::open(db)?;
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
                "embedding model changed; export and rebuild the derived index first"
            );
            store.db.execute(
                "INSERT OR IGNORE INTO metadata VALUES('embedding_identity',?)",
                [&m.identity],
            )?;
        }
        Ok(Self { store, models })
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
        let mut hits = self.store.candidates(scope, query, vector.as_deref())?;
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
