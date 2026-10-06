#![cfg(feature = "test-support")]
use enfour_memory::{
    engine::Engine,
    store::{Relation, Remember, now},
};
use std::{thread, time::Duration};

fn note(key: &str) -> Remember {
    Remember {
        scope: "repo:cache".into(),
        key: key.into(),
        title: key.into(),
        content: "SQLite stores evidence".into(),
        kind: "fact".into(),
        source: "test://cache".into(),
        expected_revision: 0,
        expires_at: None,
    }
}
fn count(e: &Engine, name: &str) -> u64 {
    e.cache_info()["results"][name].as_u64().unwrap()
}

#[test]
fn exact_keys_prefix_limits_and_validation() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open_test(&dir.path().join("db"), None).unwrap();
    e.remember(note("a")).unwrap();
    e.remember(note("b")).unwrap();
    let first = e.recall("repo:cache", "SQLite", 1).unwrap();
    let all = e.recall("repo:cache", "SQLite", 2).unwrap();
    assert_eq!(first[0].memory.id, all[0].memory.id);
    assert_eq!(all.len(), 2);
    assert_eq!(count(&e, "hits"), 1);
    assert!(e.recall("repo:elsewhere", "SQLite", 2).unwrap().is_empty());
    e.recall("repo:cache", "sqlite", 2).unwrap();
    e.recall_with_candidates("repo:cache", "SQLite", 2, 1)
        .unwrap();
    assert_eq!(count(&e, "misses"), 4);
    assert!(e.recall("repo:cache", "SQLite", 17).is_err());
    assert!(
        e.recall_with_candidates("repo:cache", "SQLite", 2, 0)
            .is_err()
    );
    e.set_cache_enabled(false);
    assert_eq!(
        serde_json::to_value(all).unwrap(),
        serde_json::to_value(e.recall("repo:cache", "SQLite", 2).unwrap()).unwrap()
    );
}

#[test]
fn external_commits_deletion_empty_results_and_cross_scope_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let mut reader = Engine::open_test(&db, None).unwrap();
    let mut writer = Engine::open_test(&db, None).unwrap();
    assert!(reader.recall("repo:cache", "SQLite", 5).unwrap().is_empty());
    let mut r = note("a");
    let saved = writer.remember(r.clone()).unwrap();
    assert_eq!(
        reader.recall("repo:cache", "SQLite", 5).unwrap()[0]
            .memory
            .revision,
        1
    );
    r.expected_revision = 1;
    r.content = "Postgres now holds the evidence".into();
    writer.remember(r).unwrap();
    assert!(reader.recall("repo:cache", "SQLite", 5).unwrap().is_empty());
    assert_eq!(
        reader.recall("repo:cache", "Postgres", 5).unwrap()[0]
            .memory
            .revision,
        2
    );
    let before = count(&reader, "misses");
    let mut other = note("other");
    other.scope = "repo:other".into();
    writer.remember(other).unwrap();
    reader.recall("repo:cache", "Postgres", 5).unwrap();
    assert_eq!(
        count(&reader, "misses"),
        before + 1,
        "cross-scope FTS changes must invalidate"
    );
    writer.store.forget(&saved.scope, &saved.id, 2).unwrap();
    assert!(
        reader
            .recall("repo:cache", "Postgres", 5)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn relation_commits_invalidate_but_failed_writes_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open_test(&dir.path().join("db"), None).unwrap();
    let a = e.remember(note("a")).unwrap();
    let b = e.remember(note("b")).unwrap();
    e.recall("repo:cache", "SQLite", 5).unwrap();
    assert!(e.remember(note("a")).is_err());
    e.recall("repo:cache", "SQLite", 5).unwrap();
    assert_eq!(count(&e, "hits"), 1);
    e.store
        .relate(Relation {
            scope: a.scope.clone(),
            from: a.id,
            to: b.id,
            kind: "supports".into(),
            source: "test://link".into(),
            from_revision: 1,
            to_revision: 1,
        })
        .unwrap();
    e.recall("repo:cache", "SQLite", 5).unwrap();
    assert_eq!(count(&e, "misses"), 2);
}

#[test]
fn expiry_without_a_write_invalidates_even_a_nonreturned_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open_test(&dir.path().join("db"), None).unwrap();
    e.remember(note("permanent")).unwrap();
    let mut r = note("temporary");
    let expiry = now() + 2;
    r.expires_at = Some(expiry);
    e.remember(r).unwrap();
    e.recall("repo:cache", "SQLite", 1).unwrap();
    while now() < expiry {
        thread::sleep(Duration::from_millis(20));
    }
    let hits = e.recall("repo:cache", "SQLite", 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].memory.key, "permanent");
    assert_eq!(count(&e, "misses"), 2);
    e.set_cache_enabled(false);
    assert_eq!(
        serde_json::to_value(hits).unwrap(),
        serde_json::to_value(e.recall("repo:cache", "SQLite", 5).unwrap()).unwrap()
    );
}

#[test]
fn queued_identical_requests_share_completed_computation() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open_test(&dir.path().join("db"), None).unwrap();
    e.remember(note("a")).unwrap();
    let shared = std::sync::Arc::new(std::sync::Mutex::new(e));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let shared = shared.clone();
            thread::spawn(move || {
                shared
                    .lock()
                    .unwrap()
                    .recall("repo:cache", "SQLite", 5)
                    .unwrap()[0]
                    .memory
                    .id
                    .clone()
            })
        })
        .collect();
    let ids: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(ids.iter().all(|id| *id == ids[0]));
    let e = shared.lock().unwrap();
    assert_eq!(count(&e, "misses"), 1);
    assert_eq!(count(&e, "hits"), 7);
}

#[test]
fn model_switch_cannot_reuse_lexical_results_or_cache_inference_errors() {
    use enfour_memory::models::{Models, digest_hex};
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open_test(&dir.path().join("db"), None).unwrap();
    e.remember(note("a")).unwrap();
    e.recall_with_candidates("repo:cache", "SQLite", 5, 16)
        .unwrap();
    let models = dir.path().join("models");
    std::fs::create_dir_all(models.join("embedding")).unwrap();
    let config = b"{\"hidden_size\":384}";
    std::fs::write(models.join("embedding/config.json"), config).unwrap();
    let manifest = serde_json::json!({"embedding":{"repository":"test","revision":"test","files":{"config.json":digest_hex(config)}},"reranker":{"repository":"test","revision":"test","files":{}}});
    std::fs::write(models.join("manifest.json"), manifest.to_string()).unwrap();
    e.models = Some(Models::open(&models).unwrap());
    for _ in 0..2 {
        assert!(
            e.recall_with_candidates("repo:cache", "SQLite", 5, 16)
                .is_err()
        );
    }
    assert_eq!(count(&e, "hits"), 0);
    assert_eq!(count(&e, "misses"), 3);
    assert!(
        e.store.db.is_autocommit(),
        "failed inference must release its read snapshot"
    );
}
