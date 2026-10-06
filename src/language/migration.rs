//! Offline, reviewed revision migration. No rewrite model and no best-effort writes.
use super::{Language, POLICY, Report, dictionary::digest};
use crate::{
    engine::CHUNKING_IDENTITY,
    models::Models,
    store::{Memory, Relation, Remember, Store, now},
};
use anyhow::{Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
#[derive(Serialize, Deserialize)]
pub struct Reviewed {
    pub id: String,
    pub original_sha256: String,
    pub expected_revision: i64,
    pub title: String,
    pub content: String,
    pub meaning_reviewed: bool,
}
#[derive(Serialize, Deserialize)]
pub struct Plan {
    pub policy: String,
    pub dictionary: String,
    pub records: Vec<Reviewed>,
    /// Only explicitly reviewed, currently valid graph edges can advance.
    pub relations: Vec<String>,
}
fn current(store: &Store) -> Result<Vec<Memory>> {
    let mut q=store.db.prepare("SELECT data FROM memories WHERE deleted=0 AND (expires_at IS NULL OR expires_at>?1) ORDER BY id")?;
    q.query_map([now()], |r| r.get::<_, String>(0))?
        .map(|r| Ok(serde_json::from_str(&r?)?))
        .collect()
}
fn request(m: &Memory) -> Remember {
    Remember {
        scope: m.scope.clone(),
        key: m.key.clone(),
        title: m.title.clone(),
        content: m.content.clone(),
        kind: m.kind.clone(),
        source: m.source.clone(),
        expected_revision: m.revision,
        expires_at: m.expires_at,
    }
}
pub fn audit(db: &Path, language: &Path) -> Result<serde_json::Value> {
    let mut store = Store::open(db)?;
    store.set_language(Language::load(language));
    let mut records = Vec::new();
    for m in current(&store)? {
        records.push(serde_json::json!({"id":m.id,"revision":m.revision,"original_sha256":digest(serde_json::to_vec(&m)?),"memory":m,"report":store.validate_memory(&request(&m))}));
    }
    Ok(serde_json::json!({"policy":POLICY,"language":store.language.info(),"records":records}))
}
/// Check every proposal without inference or revision changes.
pub fn check(db: &Path, language: &Path, plan: &Plan) -> Result<serde_json::Value> {
    let mut store = Store::open(db)?;
    store.set_language(Language::load(language));
    let old: HashMap<_, _> = current(&store)?
        .into_iter()
        .map(|m| (m.id.clone(), m))
        .collect();
    let mut reports = Vec::new();
    for proposed in &plan.records {
        let m = old
            .get(&proposed.id)
            .ok_or_else(|| anyhow::anyhow!("The plan contains an inactive or unknown memory."))?;
        ensure!(
            digest(serde_json::to_vec(m)?) == proposed.original_sha256
                && m.revision == proposed.expected_revision,
            "A memory changed after review. Audit it again."
        );
        let mut r = request(m);
        r.title = proposed.title.clone();
        r.content = proposed.content.clone();
        reports.push(serde_json::json!({"id":m.id,"report":store.validate_memory(&r)}));
    }
    let accepted = reports.iter().all(|r| r["report"]["accepted"] == true);
    Ok(serde_json::json!({"accepted":accepted,"reports":reports}))
}
pub fn apply(
    db: &Path,
    language: &Path,
    model_path: Option<&Path>,
    plan: &Plan,
    backup: &Path,
) -> Result<usize> {
    apply_with_language(db, Language::load(language), model_path, plan, backup)
}
fn apply_with_language(
    db: &Path,
    language: Language,
    model_path: Option<&Path>,
    plan: &Plan,
    backup: &Path,
) -> Result<usize> {
    let mut store = Store::open(db)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(db)?;
    lock.try_lock()
        .map_err(|e| anyhow::anyhow!("Stop the service before migration.\n{e}"))?;
    store.set_language(language);
    ensure!(
        plan.policy == POLICY,
        "The migration uses a previous writing policy."
    );
    let before = current(&store)?;
    ensure!(
        plan.records.len() == before.len(),
        "Review all current memories before migration."
    );
    let mut proposals: HashMap<_, _> = plan.records.iter().map(|r| (&r.id, r)).collect();
    ensure!(
        proposals.len() == plan.records.len(),
        "The plan contains repeated records."
    );
    let mut accepted: Vec<(Memory, Report)> = Vec::new();
    for old in &before {
        let p = proposals
            .remove(&old.id)
            .ok_or_else(|| anyhow::anyhow!("A current memory is missing from the plan."))?;
        ensure!(
            p.meaning_reviewed,
            "Review each new revision before migration."
        );
        ensure!(
            old.revision == p.expected_revision
                && digest(serde_json::to_vec(old)?) == p.original_sha256,
            "A memory changed after review. Audit it again."
        );
        // Code and explicit quotations are exact evidence. They cannot disappear.
        for evidence in super::text::evidence(&old.content) {
            ensure!(
                p.content.contains(evidence),
                "A reviewed revision changed exact evidence."
            );
        }
        let mut m = old.clone();
        m.title = p.title.clone();
        m.content = p.content.clone();
        Store::validate(&request(&m))?;
        let report = store.language.require(&request(&m))?;
        ensure!(
            report.versions.dictionary == plan.dictionary,
            "The dictionary changed after review."
        );
        m.revision += 1;
        m.updated_at = now();
        m.language = Some(report.clone());
        accepted.push((m, report));
    }
    let old_revisions: HashMap<_, _> = before.iter().map(|m| (m.id.clone(), m.revision)).collect();
    let mut edges = Vec::new();
    let edge_ids: HashSet<_> = plan.relations.iter().collect();
    ensure!(
        edge_ids.len() == plan.relations.len(),
        "The plan contains repeated relations."
    );
    {
        let mut q = store.db.prepare("SELECT id,data FROM relations")?;
        for row in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, data) = row?;
            let mut edge: Relation = serde_json::from_str(&data)?;
            let valid = old_revisions.get(&edge.from) == Some(&edge.from_revision)
                && old_revisions.get(&edge.to) == Some(&edge.to_revision);
            ensure!(
                !valid || edge_ids.contains(&id),
                "Review each current relation before migration."
            );
            if edge_ids.contains(&id) {
                ensure!(
                    valid,
                    "Do not restore a stale or inactive relation during migration."
                );
                edge.from_revision += 1;
                edge.to_revision += 1;
                edges.push((id, edge));
            }
        }
    }
    ensure!(
        edges.len() == edge_ids.len(),
        "The plan contains an unknown relation."
    );
    store.backup(backup)?;
    // Prepare all inference before starting the write transaction. The exclusive
    // maintenance lock prevents other supported writers from changing the snapshot.
    let mut models = model_path.map(Models::open).transpose()?;
    let mut indexed = Vec::new();
    for (m, _) in &accepted {
        let heading = format!("{} — {}", m.key, m.title);
        let texts = if let Some(models) = &mut models {
            models.chunks(&heading, &m.content)?
        } else {
            super::text::pack(&m.content, |s| Ok(s.chars().count() <= 700))?
                .into_iter()
                .map(|s| format!("{heading}\n{s}"))
                .collect()
        };
        let vectors = if let Some(models) = &mut models {
            Some(models.embed(texts.clone(), false)?)
        } else {
            None
        };
        indexed.push((texts, vectors));
    }
    let tx = store
        .db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let active: i64 = tx.query_row(
        "SELECT count(*) FROM memories WHERE deleted=0 AND (expires_at IS NULL OR expires_at>?)",
        [now()],
        |r| r.get(0),
    )?;
    ensure!(
        active == before.len() as i64,
        "The active memory set changed before commit. Audit it again."
    );
    for old in &before {
        let data: String =
            tx.query_row("SELECT data FROM memories WHERE id=?", [&old.id], |r| {
                r.get(0)
            })?;
        let live: Memory = serde_json::from_str(&data)?;
        ensure!(
            digest(serde_json::to_vec(&live)?) == digest(serde_json::to_vec(old)?),
            "A memory changed before commit."
        );
    }
    for ((m, _), (texts, vectors)) in accepted.iter().zip(indexed) {
        let data = serde_json::to_string(m)?;
        let changed = tx.execute(
            "UPDATE memories SET revision=?1,data=?2 WHERE id=?3 AND revision=?4",
            params![m.revision, data, m.id, m.revision - 1],
        )?;
        ensure!(changed == 1, "A memory changed during maintenance.");
        tx.execute(
            "INSERT INTO revisions VALUES(?,?,?)",
            params![m.id, m.revision, data],
        )?;
        tx.execute("DELETE FROM chunks WHERE memory_id=?", [&m.id])?;
        for (i, text) in texts.iter().enumerate() {
            let vector = vectors.as_ref().map(|v| {
                v[i].iter()
                    .flat_map(|f| f.to_le_bytes())
                    .collect::<Vec<_>>()
            });
            tx.execute(
                "INSERT INTO chunks(memory_id,text,vector) VALUES(?,?,?)",
                params![m.id, text, vector],
            )?;
        }
    }
    // Expired memories keep their revisions and cannot be recalled. Remove their
    // old derived chunks so the new chunk identity describes every searchable row.
    tx.execute("DELETE FROM chunks WHERE memory_id IN (SELECT id FROM memories WHERE deleted=1 OR expires_at<=?)",[now()])?;
    for (id, edge) in edges {
        tx.execute(
            "UPDATE relations SET data=? WHERE id=?",
            params![serde_json::to_string(&edge)?, id],
        )?;
    }
    tx.execute("INSERT INTO metadata VALUES('chunking_identity',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[CHUNKING_IDENTITY])?;
    if let Some(models) = models {
        tx.execute("INSERT INTO metadata VALUES('embedding_identity',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[models.identity])?;
    } else {
        tx.execute("DELETE FROM metadata WHERE key='embedding_identity'", [])?;
    }
    tx.commit()?;
    store.check()?;
    Ok(accepted.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    fn note(key: &str) -> Remember {
        Remember {
            scope: "repo:migration".into(),
            key: key.into(),
            title: "Source record".into(),
            content: "Keep the exact source.\n\n~~~text\nE0502; do not edit this evidence.\n~~~"
                .into(),
            kind: "fact".into(),
            source: "test://source".into(),
            expected_revision: 0,
            expires_at: None,
        }
    }
    #[test]
    fn reviewed_migration_preserves_history_evidence_graph_and_backup() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let mut e = Engine::open_test(&db, None).unwrap();
        let a = e.remember(note("a")).unwrap();
        let b = e.remember(note("b")).unwrap();
        e.store
            .relate(Relation {
                scope: a.scope.clone(),
                from: a.id.clone(),
                to: b.id.clone(),
                kind: "supports".into(),
                source: "test://relation".into(),
                from_revision: 1,
                to_revision: 1,
            })
            .unwrap();
        let edge: String = e
            .store
            .db
            .query_row("SELECT id FROM relations", [], |r| r.get(0))
            .unwrap();
        let mut plan = Plan {
            policy: POLICY.into(),
            dictionary: "original-test-fixture".into(),
            records: vec![],
            relations: vec![edge],
        };
        for m in [&a, &b] {
            plan.records.push(Reviewed {
                id: m.id.clone(),
                original_sha256: digest(serde_json::to_vec(m).unwrap()),
                expected_revision: 1,
                title: m.title.clone(),
                content: format!("Do not remove the source. {}", m.content),
                meaning_reviewed: true,
            });
        }
        let backup = dir.path().join("before.db");
        assert!(apply_with_language(&db, Language::test_fixture(), None, &plan, &backup).is_err());
        drop(e);
        plan.records[0].content = "Keep the source.".into();
        assert!(apply_with_language(&db, Language::test_fixture(), None, &plan, &backup).is_err());
        assert!(!backup.exists());
        plan.records[0].content = format!("Do not remove the source. {}", a.content);
        assert_eq!(
            apply_with_language(&db, Language::test_fixture(), None, &plan, &backup).unwrap(),
            2
        );
        let store = Store::open(&db).unwrap();
        let updated = store.get(&a.scope, &a.id).unwrap();
        assert_eq!(updated.id, a.id);
        assert_eq!(updated.scope, a.scope);
        assert_eq!(updated.source, a.source);
        assert_eq!(updated.expires_at, a.expires_at);
        assert_eq!(updated.created_at, a.created_at);
        assert_eq!(updated.revision, 2);
        let history = store.history(&a.scope, &a.id).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].content, a.content);
        assert_eq!(
            store.graph(&a.scope).unwrap()["edges"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let restored = Store::open(&backup).unwrap();
        restored.check().unwrap();
        assert_eq!(restored.get(&a.scope, &a.id).unwrap().revision, 1);
        assert_eq!(
            restored.graph(&a.scope).unwrap()["edges"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            apply_with_language(
                &db,
                Language::test_fixture(),
                None,
                &plan,
                &dir.path().join("stale.db")
            )
            .is_err()
        );
    }
}
