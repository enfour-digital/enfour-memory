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
};
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
}
impl Models {
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
            manifest,
            embedding: None,
            reranker: None,
            dimensions,
        })
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
            vectors.extend(self.embedding.as_mut().unwrap().embed(vec![text], None)?);
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
        let docs: Vec<&str> = docs.iter().map(String::as_str).collect();
        Ok(self
            .reranker
            .as_mut()
            .unwrap()
            .rerank(query, docs, false, Some(4))?
            .into_iter()
            .map(|r| (r.index, r.score))
            .collect())
    }
}
