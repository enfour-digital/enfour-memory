use crate::cache::ExactCache;
use anyhow::{Context, Result, ensure};
use fastembed::{
    InitOptionsUserDefined, Pooling, QuantizationMode, RerankInitOptionsUserDefined, TextEmbedding,
    TextRerank, TokenizerFiles, UserDefinedEmbeddingModel, UserDefinedRerankingModel,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Hash, PartialEq, Eq)]
struct TextGroup {
    query: String,
    texts: Vec<String>,
}

impl TextGroup {
    fn bytes(&self) -> usize {
        self.query.len()
            + self
                .texts
                .iter()
                .map(|s| s.len() + size_of::<String>())
                .sum::<usize>()
    }
}
pub fn digest_hex(data: impl AsRef<[u8]>) -> String {
    Sha256::digest(data.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub repository: String,
    pub revision: String,
    pub files: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_prefix: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub embedding: ModelSpec,
    pub reranker: ModelSpec,
}
pub struct Models {
    root: PathBuf,
    pub identity: String,
    manifest: Manifest,
    embedding: Option<TextEmbedding>,
    reranker: Option<TextRerank>,
    dimensions: usize,
    pub(crate) cache_identity: String,
    embeddings: ExactCache<String, Arc<Vec<f32>>>,
    chunks_cache: ExactCache<TextGroup, Arc<Vec<String>>>,
    batches: ExactCache<TextGroup, Arc<Vec<(usize, f32)>>>,
}
impl Models {
    /// Public model provenance for diagnostics; never contains memory text.
    pub fn info(&self) -> serde_json::Value {
        serde_json::json!({
            "embedding": {
                "repository": self.manifest.embedding.repository,
                "revision": self.manifest.embedding.revision,
                "dimensions": self.dimensions,
                "identity": self.identity
            },
            "reranker": {
                "repository": self.manifest.reranker.repository,
                "revision": self.manifest.reranker.revision
            }
        })
    }
    pub fn open(root: &Path) -> Result<Self> {
        let data = std::fs::read(root.join("manifest.json"))
            .context("run scripts/download-models first, or use --lexical-only")?;
        let manifest: Manifest = serde_json::from_slice(&data)?;
        let identity = format!(
            "bge-cls-dynamic-prefix-keys-v2:{}",
            digest_hex(serde_json::to_vec(&manifest.embedding)?)
        );
        let config_data = std::fs::read(root.join("embedding/config.json"))?;
        ensure!(
            manifest
                .embedding
                .files
                .get("config.json")
                .is_some_and(|hash| *hash == digest_hex(&config_data)),
            "embedding config checksum mismatch"
        );
        let config: serde_json::Value = serde_json::from_slice(&config_data)?;
        let dimensions = config["hidden_size"]
            .as_u64()
            .context("embedding hidden_size missing")? as usize;
        ensure!(
            (1..=4096).contains(&dimensions),
            "unsupported embedding dimension"
        );
        Ok(Self {
            root: root.into(),
            identity,
            cache_identity: digest_hex(serde_json::to_vec(&manifest)?),
            manifest,
            embedding: None,
            reranker: None,
            dimensions,
            embeddings: ExactCache::new(|k: &String, v: &Arc<Vec<f32>>| k.len() + v.len() * 4),
            chunks_cache: ExactCache::new(|k: &TextGroup, v: &Arc<Vec<String>>| {
                k.bytes()
                    + v.iter()
                        .map(|s| s.len() + size_of::<String>())
                        .sum::<usize>()
            }),
            batches: ExactCache::new(|k: &TextGroup, v: &Arc<Vec<(usize, f32)>>| {
                k.bytes() + v.len() * size_of::<(usize, f32)>()
            }),
        })
    }
    pub fn set_cache_enabled(&mut self, enabled: bool) {
        self.embeddings.set_enabled(enabled);
        self.chunks_cache.set_enabled(enabled);
        self.batches.set_enabled(enabled);
    }
    pub fn cache_info(&self) -> serde_json::Value {
        serde_json::json!({"embeddings":self.embeddings.info(),"chunking":self.chunks_cache.info(),"rerank_batches":self.batches.info()})
    }
    fn verified(&self, role: &str, spec: &ModelSpec, name: &str) -> Result<Vec<u8>> {
        let data = std::fs::read(self.root.join(role).join(name))?;
        let expected = spec
            .files
            .get(name)
            .context("file missing from model manifest")?;
        ensure!(
            digest_hex(&data) == *expected,
            "model checksum mismatch: {role}/{name}"
        );
        Ok(data)
    }
    fn tokenizer(&self, role: &str, spec: &ModelSpec) -> Result<TokenizerFiles> {
        Ok(TokenizerFiles {
            tokenizer_file: self.verified(role, spec, "tokenizer.json")?,
            config_file: self.verified(role, spec, "config.json")?,
            special_tokens_map_file: self.verified(role, spec, "special_tokens_map.json")?,
            tokenizer_config_file: self.verified(role, spec, "tokenizer_config.json")?,
        })
    }
    pub fn embed(&mut self, texts: Vec<String>, query: bool) -> Result<Vec<Vec<f32>>> {
        if self.embedding.is_none() {
            let s = &self.manifest.embedding;
            let model = UserDefinedEmbeddingModel::new(
                self.verified("embedding", s, "model.onnx")?,
                self.tokenizer("embedding", s)?,
            )
            .with_pooling(Pooling::Cls)
            .with_quantization(QuantizationMode::Dynamic);
            self.embedding = Some(TextEmbedding::try_new_from_user_defined(
                model,
                InitOptionsUserDefined::new()
                    .with_max_length(512)
                    .with_intra_threads(2),
            )?);
        }
        let prefix = self
            .manifest
            .embedding
            .query_prefix
            .as_deref()
            .unwrap_or("Represent this sentence for searching relevant passages: ");
        let texts: Vec<_> = texts
            .into_iter()
            .map(|t| if query { format!("{prefix}{t}") } else { t })
            .collect();
        // Dynamic quantization depends on batch ranges. Keep document and query
        // batch shape consistent and bound activation memory independently of note size.
        let mut vectors = Vec::with_capacity(texts.len());
        for text in texts {
            if let Some(vector) = self.embeddings.get(&text) {
                vectors.push((*vector).clone());
            } else {
                let mut output = self
                    .embedding
                    .as_mut()
                    .unwrap()
                    .embed(vec![text.clone()], None)?;
                ensure!(output.len() == 1, "invalid embedding count");
                let vector = output.remove(0);
                ensure!(
                    vector.len() == self.dimensions && vector.iter().all(|v| v.is_finite()),
                    "invalid embedding output"
                );
                self.embeddings.insert(text, Arc::new(vector.clone()));
                vectors.push(vector);
            }
        }
        ensure!(
            vectors
                .iter()
                .all(|v| v.len() == self.dimensions && v.iter().all(|x| x.is_finite())),
            "invalid embedding output"
        );
        Ok(vectors)
    }
    pub fn chunks(&mut self, title: &str, content: &str) -> Result<Vec<String>> {
        let key = TextGroup {
            query: title.into(),
            texts: vec![content.into()],
        };
        if let Some(chunks) = self.chunks_cache.get(&key) {
            return Ok((*chunks).clone());
        }
        if self.embedding.is_none() {
            self.embed(vec![title.into()], false)?;
        }
        let mut tokenizer = self.embedding.as_ref().unwrap().tokenizer.clone();
        tokenizer
            .with_truncation(None)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        tokenizer.with_padding(None);
        let mut pending = vec![content.to_string()];
        let mut out = Vec::new();
        while let Some(part) = pending.pop() {
            let text = format!("{title}\n{part}");
            let length = tokenizer
                .encode(text.as_str(), true)
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
                .len();
            if length <= 480 {
                out.push(text);
                continue;
            }
            let mid = part
                .char_indices()
                .nth(part.chars().count() / 2)
                .map(|(i, _)| i)
                .unwrap_or(0);
            ensure!(mid > 0, "title leaves no token budget for content");
            pending.push(part[mid..].to_string());
            pending.push(part[..mid].to_string());
        }
        self.chunks_cache.insert(key, Arc::new(out.clone()));
        Ok(out)
    }
    pub fn rerank(&mut self, query: &str, docs: &[String]) -> Result<Vec<(usize, f32)>> {
        if self.reranker.is_none() {
            let s = &self.manifest.reranker;
            let model = UserDefinedRerankingModel::new(
                self.verified("reranker", s, "model.onnx")?,
                self.tokenizer("reranker", s)?,
            );
            self.reranker = Some(TextRerank::try_new_from_user_defined(
                model,
                RerankInitOptionsUserDefined::new()
                    .with_max_length(512)
                    .with_intra_threads(2),
            )?);
        }
        // Keep the original ordered groups of four. Quantization and padding
        // can make a score depend on batch peers: never cache isolated pairs.
        let mut scores = vec![0.; docs.len()];
        for (batch_index, batch) in docs.chunks(4).enumerate() {
            let key = TextGroup {
                query: query.into(),
                texts: batch.to_vec(),
            };
            let result = if let Some(hit) = self.batches.get(&key) {
                hit
            } else {
                let texts: Vec<&str> = batch.iter().map(String::as_str).collect();
                let output =
                    self.reranker
                        .as_mut()
                        .unwrap()
                        .rerank(query, texts, false, Some(4))?;
                ensure!(
                    output.len() == batch.len()
                        && output
                            .iter()
                            .all(|r| r.index < batch.len() && r.score.is_finite()),
                    "invalid reranker output"
                );
                let result = Arc::new(
                    output
                        .into_iter()
                        .map(|r| (r.index, r.score))
                        .collect::<Vec<_>>(),
                );
                self.batches.insert(key, result.clone());
                result
            };
            for &(index, score) in result.iter() {
                scores[batch_index * 4 + index] = score;
            }
        }
        let mut output: Vec<_> = scores.into_iter().enumerate().collect();
        output.sort_by(|a, b| b.1.total_cmp(&a.1));
        Ok(output)
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    #[ignore = "requires explicitly provisioned local models"]
    fn cached_inference_matches_original_batches_bit_for_bit() {
        let root = std::env::var("ENFOUR_MODELS").unwrap_or_else(|_| "models".into());
        let mut m = Models::open(Path::new(&root)).unwrap();
        let query = "Where is the project evidence stored?";
        let docs:Vec<String>=["SQLite holds source evidence.","The sea is blue.","Use local backups.","An old architectural decision is recorded.","The memory database preserves revisions.","Coffee and tea are served.","Local vectors aid retrieval.","A graph links related facts.","Another longer unrelated passage describes a train crossing the countryside on a rainy morning."].iter().map(|s|s.to_string()).collect();
        let cold = m.rerank(query, &docs).unwrap();
        let original = m
            .reranker
            .as_mut()
            .unwrap()
            .rerank(
                query,
                docs.iter().map(String::as_str).collect::<Vec<_>>(),
                false,
                Some(4),
            )
            .unwrap();
        let bits = |rows: &[(usize, f32)]| {
            rows.iter()
                .map(|(i, s)| (*i, s.to_bits()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            bits(&cold),
            original
                .iter()
                .map(|r| (r.index, r.score.to_bits()))
                .collect::<Vec<_>>()
        );
        let warm = m.rerank(query, &docs).unwrap();
        assert_eq!(bits(&cold), bits(&warm));
        let before = m.cache_info();
        let mut changed = docs.clone();
        changed[8] = "The last passage changed.".into();
        m.rerank(query, &changed).unwrap();
        let after = m.cache_info();
        assert_eq!(
            after["rerank_batches"]["hits"].as_u64().unwrap()
                - before["rerank_batches"]["hits"].as_u64().unwrap(),
            2
        );
        assert_eq!(
            after["rerank_batches"]["misses"].as_u64().unwrap()
                - before["rerank_batches"]["misses"].as_u64().unwrap(),
            1
        );
        // Changing batch membership must recompute that group, not reuse pair scores.
        let moved = vec![docs[1].clone(), docs[0].clone(), docs[2].clone()];
        let cached = m.rerank(query, &moved).unwrap();
        let oracle = m
            .reranker
            .as_mut()
            .unwrap()
            .rerank(
                query,
                moved.iter().map(String::as_str).collect::<Vec<_>>(),
                false,
                Some(4),
            )
            .unwrap();
        assert_eq!(
            bits(&cached),
            oracle
                .iter()
                .map(|r| (r.index, r.score.to_bits()))
                .collect::<Vec<_>>()
        );
        let embedded = m.embed(vec![query.into()], true).unwrap();
        assert_eq!(embedded, m.embed(vec![query.into()], true).unwrap());
        let chunks = m
            .chunks("Evidence", &"SQLite keeps records. ".repeat(180))
            .unwrap();
        assert_eq!(
            chunks,
            m.chunks("Evidence", &"SQLite keeps records. ".repeat(180))
                .unwrap()
        );
        assert_eq!(m.cache_info()["chunking"]["hits"], 1);
        m.set_cache_enabled(false);
        assert_eq!(embedded, m.embed(vec![query.into()], true).unwrap());
        assert_eq!(
            chunks,
            m.chunks("Evidence", &"SQLite keeps records. ".repeat(180))
                .unwrap()
        );
        assert_eq!(bits(&cold), bits(&m.rerank(query, &docs).unwrap()));
        println!(
            "PASS: exact embedding/chunk reuse and original ordered rerank batch scores; partial-batch reuse preserves score bits"
        );
    }
}
