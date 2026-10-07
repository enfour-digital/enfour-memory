#![cfg(feature = "test-support")]
use enfour_memory::{
    engine::Engine,
    store::{Relation, Remember, Store},
};
fn note(key: &str) -> Remember {
    Remember {
        scope: "repo:example/a".into(),
        key: key.into(),
        title: key.into(),
        content: "Use SQLite with WAL for atomic project memory".into(),
        kind: "decision".into(),
        source: "test://user/1".into(),
        expected_revision: 0,
        expires_at: None,
    }
}
#[test]
fn larger_candidate_pool_preserves_scope_and_current_revision_filters() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open_test(&dir.path().join("candidates.db"), None).unwrap();
    for i in 0..40 {
        engine.remember(note(&format!("candidate-{i}"))).unwrap();
    }
    let hidden = engine.store.get("repo:example/a", "candidate-0").unwrap();
    engine.store.forget(&hidden.scope, &hidden.id, 1).unwrap();
    let mut other = note("other-scope");
    other.scope = "repo:other".into();
    engine.remember(other).unwrap();
    assert_eq!(
        engine
            .store
            .candidates("repo:example/a", "SQLite", None)
            .unwrap()
            .len(),
        16
    );
    let wider = engine
        .store
        .candidates_with_limit("repo:example/a", "SQLite", None, 64)
        .unwrap();
    assert_eq!(wider.len(), 39);
    assert!(
        wider
            .iter()
            .all(|h| h.memory.scope == "repo:example/a" && h.memory.key != "candidate-0")
    );
    assert!(
        engine
            .store
            .candidates_with_limit("repo:example/a", "SQLite", None, 129)
            .is_err()
    );
}
#[test]
fn revisions_isolation_graph_and_backup() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("memory.db");
    let mut e = Engine::open_test(&db, None).unwrap();
    let a = e.remember(note("database")).unwrap();
    let b = e.remember(note("reliability")).unwrap();
    assert_eq!(e.recall("repo:example/a", "SQLite", 5).unwrap().len(), 2);
    let wire = serde_json::to_value(e.recall("repo:example/a", "SQLite", 5).unwrap()).unwrap();
    assert!(wire[0]["memory"].get("content").is_none());
    assert!(!e.store.get(&a.scope, &a.id).unwrap().content.is_empty());
    assert!(e.recall("repo:example/b", "SQLite", 5).unwrap().is_empty());
    assert!(e.store.get("repo:example/b", &a.id).is_err());
    e.store
        .relate(Relation {
            scope: a.scope.clone(),
            from: a.id.clone(),
            to: b.id.clone(),
            kind: "supports".into(),
            source: "test://evidence".into(),
            from_revision: 1,
            to_revision: 1,
        })
        .unwrap();
    assert_eq!(
        e.store.graph(&a.scope).unwrap()["edges"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut other = Engine::open_test(&db, None).unwrap();
    let mut updated = note("database");
    updated.expected_revision = 1;
    updated.content = "Use SQLite plus consistent online backups".into();
    assert_eq!(other.remember(updated.clone()).unwrap().revision, 2);
    assert!(e.remember(updated).is_err());
    assert_eq!(e.store.history(&a.scope, &a.id).unwrap().len(), 2);
    assert!(
        e.store.graph(&a.scope).unwrap()["edges"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    e.store.forget(&b.scope, &b.id, 1).unwrap();
    assert_eq!(e.recall(&a.scope, "SQLite", 5).unwrap().len(), 1);
    e.store.check().unwrap();
    let backup = d.path().join("backup.db");
    e.store.backup(&backup).unwrap();
    let restored = Store::open(&backup).unwrap();
    restored.check().unwrap();
    assert_eq!(restored.history(&a.scope, &a.id).unwrap().len(), 2);
    assert!(restored.get(&b.scope, &b.id).unwrap().deleted);
    assert!(e.store.backup(&backup).is_err());
}
#[test]
fn rejects_invalid_writes_and_expires_recall() {
    let d = tempfile::tempdir().unwrap();
    let mut e = Engine::open_test(&d.path().join("memory.db"), None).unwrap();
    let mut r = note("temporary");
    r.expires_at = Some(1);
    assert!(e.remember(r).is_err());
    let a = e.remember(note("temporary")).unwrap();
    e.store
        .db
        .execute("UPDATE memories SET expires_at=1 WHERE id=?", [&a.id])
        .unwrap();
    assert!(e.recall(&a.scope, "SQLite", 5).unwrap().is_empty());
    assert!(
        e.store.graph(&a.scope).unwrap()["nodes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(e.recall(&a.scope, "", 5).is_err());
    // Punctuation is not executable FTS query syntax.
    assert!(e.recall(&a.scope, "\" OR * NEAR()", 5).is_ok());
}

#[test]
fn lexical_recall_handles_identifiers_and_inflections() {
    let d = tempfile::tempdir().unwrap();
    let mut e = Engine::open_test(&d.path().join("memory.db"), None).unwrap();
    let mut r = note("rustc-error-E0502");
    r.content = "Compile with the shared cache after fixing the borrow conflict.".into();
    let m = e.remember(r).unwrap();
    for query in ["E0502", "compilation"] {
        assert_eq!(e.recall(&m.scope, query, 3).unwrap()[0].memory.id, m.id);
    }
}

#[test]
fn fresh_rag_database_has_no_git_outbox() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open_test(&dir.path().join("memory.db"), None).unwrap();
    let count: i64 = engine
        .store
        .db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE 'agent_git_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn rag_retires_legacy_git_triggers_without_losing_memory_or_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.db");
    let mut engine = Engine::open_test(&path, None).unwrap();
    // Model the schema from the experimental branch, including every event trigger.
    engine
        .store
        .db
        .execute_batch(
            "CREATE TABLE agent_git_events(
        seq INTEGER PRIMARY KEY AUTOINCREMENT, scope TEXT NOT NULL,
        kind TEXT NOT NULL, object_id TEXT NOT NULL, data TEXT NOT NULL);",
        )
        .unwrap();
    for (table, kind) in [("memories", "memory"), ("relations", "relation")] {
        for action in ["INSERT", "UPDATE"] {
            engine
                .store
                .db
                .execute_batch(&format!(
                    "CREATE TRIGGER agent_git_{table}_{action} AFTER {action} ON {table}
                 BEGIN INSERT INTO agent_git_events(scope,kind,object_id,data)
                 VALUES(NEW.scope,'{kind}',NEW.id,NEW.data); END;"
                ))
                .unwrap();
        }
    }
    engine
        .store
        .db
        .execute_batch(
            "CREATE TRIGGER agent_git_relation_delete AFTER DELETE ON relations
        BEGIN INSERT INTO agent_git_events(scope,kind,object_id,data)
        VALUES(OLD.scope,'remove_relation',OLD.id,OLD.data); END;",
        )
        .unwrap();
    let a = engine.remember(note("database")).unwrap();
    let b = engine.remember(note("reliability")).unwrap();
    engine
        .store
        .relate(Relation {
            scope: a.scope.clone(),
            from: a.id.clone(),
            to: b.id.clone(),
            kind: "supports".into(),
            source: "test://evidence".into(),
            from_revision: 1,
            to_revision: 1,
        })
        .unwrap();
    let graph = engine.store.graph(&a.scope).unwrap();
    let history = serde_json::to_value(engine.store.history(&a.scope, &a.id).unwrap()).unwrap();
    let events: Vec<String> = engine
        .store
        .db
        .prepare("SELECT data FROM agent_git_events ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(events.len(), 3);
    drop(engine);

    let mut engine = Engine::open_test(&path, None).unwrap();
    assert_eq!(engine.store.graph(&a.scope).unwrap(), graph);
    assert_eq!(
        serde_json::to_value(engine.store.history(&a.scope, &a.id).unwrap()).unwrap(),
        history
    );
    assert_eq!(engine.recall(&a.scope, "SQLite", 5).unwrap().len(), 2);
    let triggers: i64 = engine
        .store
        .db
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'agent_git_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(triggers, 0);
    let mut update = note("database");
    update.expected_revision = 1;
    update.content = "Use SQLite WAL for local storage".into();
    assert_eq!(engine.remember(update).unwrap().revision, 2);
    let after: Vec<String> = engine
        .store
        .db
        .prepare("SELECT data FROM agent_git_events ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(after, events);
    assert_eq!(engine.store.history(&a.scope, &a.id).unwrap().len(), 2);
    engine.store.check().unwrap();
}
