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
fn revisions_isolation_graph_and_backup() {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("memory.db");
    let mut e = Engine::open(&db, None).unwrap();
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
    let mut other = Engine::open(&db, None).unwrap();
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
    let mut e = Engine::open(&d.path().join("memory.db"), None).unwrap();
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
    let mut e = Engine::open(&d.path().join("memory.db"), None).unwrap();
    let mut r = note("rustc-error-E0502");
    r.content = "Compile with the shared cache after fixing the borrow conflict.".into();
    let m = e.remember(r).unwrap();
    for query in ["E0502", "compilation"] {
        assert_eq!(e.recall(&m.scope, query, 3).unwrap()[0].memory.id, m.id);
    }
}
