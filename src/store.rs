use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Memory {
    pub id: String,
    pub scope: String,
    pub key: String,
    pub title: String,
    pub content: String,
    pub kind: String,
    pub source: String,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: Option<i64>,
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<crate::language::Report>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Remember {
    /// Stable repository identity or explicit personal scope. Do not use a folder basename as the repository identity.
    pub scope: String,
    /// Stable fact key. Updates must use the current revision. Different facts must use different keys.
    pub key: String,
    pub title: String,
    /// One supported fact, decision, lesson, or checkpoint. Never store credentials.
    pub content: String,
    /// fact, preference, decision, lesson, or checkpoint.
    pub kind: String,
    /// Evidence URI or path plus line/commit, or an explicit user statement reference.
    pub source: String,
    /// Use zero for a new key. For an update, use the revision from recall or inspect.
    pub expected_revision: i64,
    pub expires_at: Option<i64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    #[serde(serialize_with = "memory_header")]
    pub memory: Memory,
    pub excerpt: String,
    pub score: f32,
    pub lexical: bool,
    pub semantic: bool,
    pub rerank_score: Option<f32>,
}
fn memory_header<S: serde::Serializer>(
    m: &Memory,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serde_json::json!({"id":m.id,"scope":m.scope,"key":m.key,"title":m.title,"kind":m.kind,"source":m.source,"revision":m.revision,"created_at":m.created_at,"updated_at":m.updated_at,"expires_at":m.expires_at,"deleted":m.deleted,"content_bytes":m.content.len(),"language":m.language.as_ref().map(|report| serde_json::json!({"accepted":report.accepted,"versions":report.versions,"advisory_count":report.diagnostics.len()}))}).serialize(serializer)
}
pub struct Chunk {
    pub text: String,
    pub vector: Option<Vec<f32>>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Relation {
    pub scope: String,
    pub from: String,
    pub to: String,
    pub kind: String,
    pub source: String,
    pub from_revision: i64,
    pub to_revision: i64,
}
pub struct Store {
    pub(crate) language: crate::language::Language,
    pub db: Connection,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            version <= 1,
            "The database version is too new for this engine."
        );
        db.execute_batch("BEGIN IMMEDIATE;
          CREATE TABLE IF NOT EXISTS memories(id TEXT PRIMARY KEY, scope TEXT NOT NULL, key TEXT NOT NULL, revision INTEGER NOT NULL, data TEXT NOT NULL, deleted INTEGER NOT NULL, expires_at INTEGER, UNIQUE(scope,key));
          CREATE TABLE IF NOT EXISTS revisions(id TEXT NOT NULL, revision INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY(id,revision));
          CREATE TABLE IF NOT EXISTS chunks(id INTEGER PRIMARY KEY, memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE, text TEXT NOT NULL, vector BLOB);
          CREATE INDEX IF NOT EXISTS chunks_memory ON chunks(memory_id);
          CREATE INDEX IF NOT EXISTS memory_scope ON memories(scope,deleted,expires_at);
          CREATE VIRTUAL TABLE IF NOT EXISTS search USING fts5(text,content='chunks',content_rowid='id',tokenize='porter unicode61');
          CREATE TRIGGER IF NOT EXISTS chunk_insert AFTER INSERT ON chunks BEGIN INSERT INTO search(rowid,text) VALUES(new.id,new.text); END;
          CREATE TRIGGER IF NOT EXISTS chunk_delete AFTER DELETE ON chunks BEGIN INSERT INTO search(search,rowid,text) VALUES('delete',old.id,old.text); END;
          CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS relations(id TEXT PRIMARY KEY,scope TEXT NOT NULL,from_id TEXT NOT NULL REFERENCES memories(id),to_id TEXT NOT NULL REFERENCES memories(id),data TEXT NOT NULL);
          INSERT OR IGNORE INTO metadata VALUES('retrieval_generation','0');
          PRAGMA user_version=1;")?;
        // Global rather than per-scope: FTS5's document frequencies span scopes.
        // Triggers also cover commits from other processes and older binaries.
        for table in ["memories", "chunks", "relations"] {
            for action in ["INSERT", "UPDATE", "DELETE"] {
                db.execute_batch(&format!(
                    "CREATE TRIGGER IF NOT EXISTS cache_{table}_{action} AFTER {action} ON {table}
                     BEGIN UPDATE metadata SET value=CAST(value AS INTEGER)+1
                     WHERE key='retrieval_generation'; END;"
                ))?;
            }
        }
        db.execute_batch("COMMIT;")?;
        Ok(Self {
            db,
            language: crate::language::Language::load(
                &std::env::var_os("ENFOUR_LANGUAGE")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| "language-private/dictionary.json".into()),
            ),
        })
    }
    /// Read in the transaction. The version and candidates must share a snapshot.
    pub(crate) fn retrieval_state(
        &self,
        scope: &str,
        at: i64,
    ) -> Result<(i64, Option<i64>, Option<i64>)> {
        Ok(self.db.query_row(
            "SELECT CAST(value AS INTEGER),
             (SELECT max(expires_at) FROM memories WHERE scope=?1 AND deleted=0 AND expires_at<=?2),
             (SELECT min(expires_at) FROM memories WHERE scope=?1 AND deleted=0 AND expires_at>?2)
             FROM metadata WHERE key='retrieval_generation'",
            params![scope, at],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    }
    pub fn validate(r: &Remember) -> Result<()> {
        for (name, value, max) in [
            ("scope", r.scope.as_str(), 512),
            ("key", r.key.as_str(), 256),
            ("title", r.title.as_str(), 200),
            ("content", r.content.as_str(), 32000),
            ("source", r.source.as_str(), 2000),
        ] {
            ensure!(
                !value.trim().is_empty() && value.len() <= max,
                "{name} must contain 1..{max} bytes"
            );
            ensure!(!value.contains('\0'), "{name} contains NUL");
        }
        ensure!(
            ["fact", "preference", "decision", "lesson", "checkpoint"].contains(&r.kind.as_str()),
            "The memory kind is not supported."
        );
        ensure!(r.expected_revision >= 0, "revision must be nonnegative");
        ensure!(
            r.expires_at.is_none_or(|t| t > now()),
            "The expiry must be after the current time."
        );
        Ok(())
    }
    pub fn validate_memory(&mut self, r: &Remember) -> crate::language::Report {
        self.language.validate(r)
    }
    pub fn set_language(&mut self, language: crate::language::Language) {
        self.language = language;
    }
    pub fn put(&mut self, r: Remember, chunks: &[Chunk]) -> Result<Memory> {
        let language = Some(self.language.require(&r)?);
        ensure!(!chunks.is_empty(), "The memory must have indexed chunks.");
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT data FROM memories WHERE scope=? AND key=?",
                params![r.scope, r.key],
                |row| row.get(0),
            )
            .optional()?;
        let old: Option<Memory> = old.map(|v| serde_json::from_str(&v)).transpose()?;
        let actual = old.as_ref().map_or(0, |m| m.revision);
        ensure!(
            r.expected_revision == actual,
            "Memory revision conflict. Required revision: {}. Current revision: {}. Inspect the memory before replacement.",
            r.expected_revision,
            actual
        );
        let m = Memory {
            language,
            id: old
                .as_ref()
                .map_or_else(|| uuid::Uuid::new_v4().to_string(), |m| m.id.clone()),
            scope: r.scope,
            key: r.key,
            title: r.title,
            content: r.content,
            kind: r.kind,
            source: r.source,
            revision: actual + 1,
            created_at: old.as_ref().map_or(now(), |m| m.created_at),
            updated_at: now(),
            expires_at: r.expires_at,
            deleted: false,
        };
        let data = serde_json::to_string(&m)?;
        tx.execute("INSERT INTO memories VALUES(?1,?2,?3,?4,?5,0,?6) ON CONFLICT(scope,key) DO UPDATE SET revision=excluded.revision,data=excluded.data,deleted=0,expires_at=excluded.expires_at",params![m.id,m.scope,m.key,m.revision,data,m.expires_at])?;
        tx.execute(
            "INSERT INTO revisions VALUES(?,?,?)",
            params![m.id, m.revision, data],
        )?;
        tx.execute("DELETE FROM chunks WHERE memory_id=?", [&m.id])?;
        for c in chunks {
            let bytes = c
                .vector
                .as_ref()
                .map(|v| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
            tx.execute(
                "INSERT INTO chunks(memory_id,text,vector) VALUES(?,?,?)",
                params![m.id, c.text, bytes],
            )?;
        }
        tx.commit()?;
        Ok(m)
    }
    pub fn get(&self, scope: &str, id: &str) -> Result<Memory> {
        let data: String = self
            .db
            .query_row(
                "SELECT data FROM memories WHERE scope=? AND (id=? OR key=?)",
                params![scope, id, id],
                |r| r.get(0),
            )
            .optional()?
            .context("memory not found in this scope")?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn history(&self, scope: &str, id: &str) -> Result<Vec<Memory>> {
        let m = self.get(scope, id)?;
        let mut q = self
            .db
            .prepare("SELECT data FROM revisions WHERE id=? ORDER BY revision DESC")?;
        q.query_map([m.id], |r| r.get::<_, String>(0))?
            .map(|v| Ok(serde_json::from_str(&v?)?))
            .collect()
    }
    pub fn forget(&mut self, scope: &str, id: &str, revision: i64) -> Result<Memory> {
        self.language.require_available()?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let data: String = tx
            .query_row(
                "SELECT data FROM memories WHERE scope=? AND (id=? OR key=?)",
                params![scope, id, id],
                |r| r.get(0),
            )
            .optional()?
            .context("memory not found")?;
        let mut m: Memory = serde_json::from_str(&data)?;
        ensure!(m.revision == revision, "revision conflict");
        m.deleted = true;
        m.revision += 1;
        m.updated_at = now();
        let data = serde_json::to_string(&m)?;
        tx.execute(
            "UPDATE memories SET deleted=1,revision=?,data=? WHERE id=?",
            params![m.revision, data, m.id],
        )?;
        tx.execute(
            "INSERT INTO revisions VALUES(?,?,?)",
            params![m.id, m.revision, data],
        )?;
        tx.execute("DELETE FROM chunks WHERE memory_id=?", [&m.id])?;
        tx.commit()?;
        Ok(m)
    }
    pub fn list(&self, scope: &str, include_deleted: bool) -> Result<Vec<Memory>> {
        let mut q=self.db.prepare("SELECT data FROM memories WHERE scope=? AND (? OR (deleted=0 AND (expires_at IS NULL OR expires_at>?))) ORDER BY rowid DESC")?;
        q.query_map(params![scope, include_deleted, now()], |r| {
            r.get::<_, String>(0)
        })?
        .map(|v| Ok(serde_json::from_str(&v?)?))
        .collect()
    }
    pub fn candidates(&self, scope: &str, query: &str, vector: Option<&[f32]>) -> Result<Vec<Hit>> {
        self.candidates_with_limit(scope, query, vector, 16)
    }
    pub fn candidates_with_limit(
        &self,
        scope: &str,
        query: &str,
        vector: Option<&[f32]>,
        limit: usize,
    ) -> Result<Vec<Hit>> {
        self.candidates_at(scope, query, vector, limit, now())
    }
    pub(crate) fn candidates_at(
        &self,
        scope: &str,
        query: &str,
        vector: Option<&[f32]>,
        limit: usize,
        at: i64,
    ) -> Result<Vec<Hit>> {
        ensure!((1..=128).contains(&limit), "candidate limit must be 1..128");
        let terms: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .take(32)
            .map(|t| format!("\"{t}\""))
            .collect();
        let mut ranks: HashMap<i64, (f32, bool, bool)> = HashMap::new();
        if !terms.is_empty() {
            let mut q=self.db.prepare("SELECT c.id FROM search JOIN chunks c ON c.id=search.rowid JOIN memories m ON m.id=c.memory_id WHERE search MATCH ? AND m.scope=? AND m.deleted=0 AND (m.expires_at IS NULL OR m.expires_at>?) ORDER BY bm25(search) LIMIT 64")?;
            for (rank, id) in q
                .query_map(params![terms.join(" OR "), scope, at], |r| {
                    r.get::<_, i64>(0)
                })?
                .enumerate()
            {
                ranks.insert(id?, (1.0 / (60.0 + rank as f32 + 1.0), true, false));
            }
        }
        if let Some(v) = vector {
            let mut best: Vec<(i64, f32)> = Vec::new();
            let mut q=self.db.prepare("SELECT c.id,c.vector FROM chunks c JOIN memories m ON m.id=c.memory_id WHERE m.scope=? AND m.deleted=0 AND (m.expires_at IS NULL OR m.expires_at>?) AND c.vector IS NOT NULL")?;
            let mut rows = q.query(params![scope, at])?;
            while let Some(row) = rows.next()? {
                let bytes: Vec<u8> = row.get(1)?;
                ensure!(
                    bytes.len() == v.len() * 4,
                    "The embedding dimensions are not equal. Rebuild the index."
                );
                let score: f32 = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(v)
                    .map(|(b, x)| f32::from_le_bytes(*b) * x)
                    .sum();
                if score.is_finite() {
                    best.push((row.get(0)?, score));
                }
                if best.len() > 128 {
                    best.sort_by(|a, b| b.1.total_cmp(&a.1));
                    best.truncate(64);
                }
            }
            best.sort_by(|a, b| b.1.total_cmp(&a.1));
            best.truncate(64);
            for (rank, (id, _)) in best.into_iter().enumerate() {
                let r = ranks.entry(id).or_insert((0., false, false));
                r.0 += 1.0 / (61.0 + rank as f32);
                r.2 = true;
            }
        }
        let mut ranked: Vec<_> = ranks.into_iter().collect();
        ranked.sort_by(|a, b| b.1.0.total_cmp(&a.1.0).then(a.0.cmp(&b.0)));
        ranked.truncate(limit * 3);
        let mut results = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (id, (score, lexical, semantic)) in ranked {
            let (data,excerpt):(String,String)=self.db.query_row("SELECT m.data,c.text FROM chunks c JOIN memories m ON m.id=c.memory_id WHERE c.id=?",[id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let memory: Memory = serde_json::from_str(&data)?;
            if seen.insert(memory.id.clone()) {
                results.push(Hit {
                    memory,
                    excerpt,
                    score,
                    lexical,
                    semantic,
                    rerank_score: None,
                });
            }
            if results.len() == limit {
                break;
            }
        }
        Ok(results)
    }
    pub fn stats(&self) -> Result<serde_json::Value> {
        let count:i64=self.db.query_row("SELECT count(*) FROM memories WHERE deleted=0 AND (expires_at IS NULL OR expires_at>?)",[now()],|r|r.get(0))?;
        let vectors: i64 = self.db.query_row(
            "SELECT count(*) FROM chunks WHERE vector IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        let chunks: i64 = self
            .db
            .query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))?;
        let mut q = self
            .db
            .prepare("SELECT DISTINCT scope FROM memories ORDER BY scope")?;
        let scopes: Vec<String> = q
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(
            serde_json::json!({"active_memories":count,"chunks":chunks,"embedded_chunks":vectors,"scopes":scopes}),
        )
    }
    pub fn relate(&mut self, mut r: Relation) -> Result<serde_json::Value> {
        self.language.require_available()?;
        ensure!(
            [
                "supports",
                "contradicts",
                "derived_from",
                "extends",
                "related_to"
            ]
            .contains(&r.kind.as_str()),
            "The relation kind is not supported."
        );
        ensure!(
            !r.source.trim().is_empty() && r.source.len() <= 2000,
            "The relation must have evidence."
        );
        // Take a write reservation before checking endpoint revisions.
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let a = self.get(&r.scope, &r.from)?;
            let b = self.get(&r.scope, &r.to)?;
            ensure!(a.id != b.id, "A relation must link different records.");
            ensure!(
                !a.deleted
                    && !b.deleted
                    && a.expires_at.is_none_or(|t| t > now())
                    && b.expires_at.is_none_or(|t| t > now()),
                "inactive endpoint"
            );
            ensure!(
                a.revision == r.from_revision && b.revision == r.to_revision,
                "endpoint revision conflict"
            );
            r.from = a.id;
            r.to = b.id;
            let data = serde_json::to_string(&r)?;
            let id = crate::models::digest_hex(data.as_bytes());
            self.db.execute(
                "INSERT OR IGNORE INTO relations VALUES(?,?,?,?,?)",
                params![id, r.scope, r.from, r.to, data],
            )?;
            Ok(serde_json::json!({"id":id,"relation":r}))
        })();
        match result {
            Ok(v) => {
                self.db.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                self.db.execute_batch("ROLLBACK")?;
                Err(e)
            }
        }
    }
    pub fn graph(&self, scope: &str) -> Result<serde_json::Value> {
        let nodes = self.list(scope, false)?;
        let active: HashMap<&str, i64> =
            nodes.iter().map(|m| (m.id.as_str(), m.revision)).collect();
        let mut q = self
            .db
            .prepare("SELECT id,data FROM relations WHERE scope=? ORDER BY id")?;
        let mut edges = Vec::new();
        for row in q.query_map([scope], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (id, data) = row?;
            let r: Relation = serde_json::from_str(&data)?;
            if active.get(r.from.as_str()) == Some(&r.from_revision)
                && active.get(r.to.as_str()) == Some(&r.to_revision)
            {
                edges.push(serde_json::json!({"id":id,"relation":r}));
            }
        }
        Ok(
            serde_json::json!({"schema_version":1,"scope":scope,"generated_at":now(),"nodes":nodes,"edges":edges}),
        )
    }
    pub fn graph_dot(&self, scope: &str) -> Result<String> {
        let graph = self.graph(scope)?;
        let mut out = String::from("digraph memory {\n  node [shape=box];\n");
        // JSON quoting also escapes quotes, backslashes and newlines for DOT strings.
        for n in graph["nodes"].as_array().unwrap() {
            out.push_str(&format!("  {} [label={}];\n", n["id"], n["title"]));
        }
        for e in graph["edges"].as_array().unwrap() {
            let r = &e["relation"];
            out.push_str(&format!(
                "  {} -> {} [label={}];\n",
                r["from"], r["to"], r["kind"]
            ));
        }
        out.push_str("}\n");
        Ok(out)
    }
    pub fn check(&self) -> Result<()> {
        let s: String = self
            .db
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        ensure!(s == "ok", "database integrity: {s}");
        self.db.execute(
            "INSERT INTO search(search,rank) VALUES('integrity-check',1)",
            [],
        )?;
        Ok(())
    }
    pub fn backup(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .context("backup destination exists or cannot be created")?;
        self.db.backup("main", path, None)?;
        Ok(())
    }
}

#[cfg(test)]
mod cache_snapshot_tests {
    use super::*;
    use crate::engine::Engine;
    #[test]
    fn generation_and_candidates_share_a_snapshot_across_external_commit() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let mut writer = Engine::open_test(&db, None).unwrap();
        let mut note = Remember {
            scope: "repo:snapshot".into(),
            key: "a".into(),
            title: "a".into(),
            content: "Keep SQLite evidence.".into(),
            kind: "fact".into(),
            source: "test://snapshot".into(),
            expected_revision: 0,
            expires_at: None,
        };
        writer.remember(note.clone()).unwrap();
        let reader = Store::open(&db).unwrap();
        let tx = reader.db.unchecked_transaction().unwrap();
        let before = reader.retrieval_state(&note.scope, now()).unwrap();
        note.key = "b".into();
        writer.remember(note.clone()).unwrap();
        assert_eq!(reader.retrieval_state(&note.scope, now()).unwrap(), before);
        assert_eq!(
            reader
                .candidates(&note.scope, "SQLite", None)
                .unwrap()
                .len(),
            1
        );
        tx.commit().unwrap();
        assert!(reader.retrieval_state(&note.scope, now()).unwrap().0 > before.0);
        assert_eq!(
            reader
                .candidates(&note.scope, "SQLite", None)
                .unwrap()
                .len(),
            2
        );
    }
}
