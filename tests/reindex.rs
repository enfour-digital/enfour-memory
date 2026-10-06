use enfour_memory::{engine::Engine, store::Remember};
use std::path::PathBuf;

#[test]
#[ignore = "requires provisioned local models"]
fn reindex_is_exclusive_atomic_and_preserves_history() {
    let root = PathBuf::from(
        std::env::var("ENFOUR_MODELS")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "models".into()),
    );
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("index.db");
    let mut engine = Engine::open(&db, None).unwrap();
    let note = Remember {
        scope: "repo:reindex".into(),
        key: "storage".into(),
        title: "Storage".into(),
        content: "SQLite holds source records and evidence.".into(),
        kind: "fact".into(),
        source: "test://verified".into(),
        expected_revision: 0,
        expires_at: None,
    };
    engine.remember(note.clone()).unwrap();
    let mut updated = note;
    updated.expected_revision = 1;
    updated.content += " WAL protects commits.";
    let saved = engine.remember(updated).unwrap();
    assert_eq!(saved.revision, 2);
    assert!(
        Engine::reindex(&db, &root)
            .unwrap_err()
            .to_string()
            .contains("stop other Enfour processes")
    );
    drop(engine);
    let broken = dir.path().join("broken-models");
    std::fs::create_dir_all(broken.join("embedding")).unwrap();
    std::fs::copy(root.join("manifest.json"), broken.join("manifest.json")).unwrap();
    std::fs::copy(
        root.join("embedding/config.json"),
        broken.join("embedding/config.json"),
    )
    .unwrap();
    // Missing weights fail after the transaction's DELETE, which must roll back.
    assert!(Engine::reindex(&db, &broken).is_err());
    let restored = Engine::open(&db, None).unwrap();
    restored.store.check().unwrap();
    assert_eq!(restored.store.stats().unwrap()["chunks"], 1);
    assert_eq!(
        restored
            .store
            .history(&saved.scope, &saved.id)
            .unwrap()
            .len(),
        2
    );
    drop(restored);
    assert_eq!(Engine::reindex(&db, &root).unwrap(), 1);
    let mut indexed = Engine::open(&db, Some(&root)).unwrap();
    assert_eq!(indexed.store.stats().unwrap()["embedded_chunks"], 1);
    assert_eq!(
        indexed
            .store
            .history(&saved.scope, &saved.id)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        indexed.store.get(&saved.scope, &saved.id).unwrap().content,
        saved.content
    );
    assert_eq!(
        indexed.recall(&saved.scope, "SQLite", 1).unwrap()[0]
            .memory
            .id,
        saved.id
    );
}
