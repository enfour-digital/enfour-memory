#![cfg(feature = "test-support")]
use enfour_memory::{
    engine::Engine,
    language::{Language, Severity, text},
    store::{Chunk, Remember},
};
fn note(text: &str) -> Remember {
    Remember {
        scope: "repo:test".into(),
        key: "source".into(),
        title: "Source record".into(),
        content: text.into(),
        source: "test://source".into(),
        kind: "fact".into(),
        expected_revision: 0,
        expires_at: None,
    }
}
#[test]
fn counting_preserves_measurements_quotes_parentheses_and_unicode() {
    for (s, n) in [
        ("Use 3.5 mm of tape.", 4),
        ("Read the UTF-8 file.", 4),
        ("Use the cache (if the server is active).", 4),
        ("Read e.g. the file.", 4),
        ("Enfour Memory stores 384 numbers.", 4),
        ("Use café-data.", 2),
        ("Read 1,024 bytes.", 2),
        ("Use 10 degrees Celsius.", 2),
        ("Read at 10 a.m.", 3),
        ("Inspect No. 1.", 2),
        ("Wait two seconds.", 2),
        ("Use 3.5 ms.", 2),
        ("Read source‑data.", 2),
    ] {
        assert_eq!(text::count_words(s), n, "{s}");
    }
    let s = "Keep \u{60}don't; x=3.5\u{60} unchanged. Read “one; two; three” first.";
    let view = text::View::new(s);
    assert_eq!(view.prose.len(), s.len());
    assert_eq!(text::sentences(&view.prose).len(), 2);
    assert!(!view.prose.contains(';'));
    assert_eq!(text::count_words(&view.prose), 6);
    assert_eq!(text::sentences("Use 3.5 mm. Read e.g. the label.").len(), 2);
}
#[test]
fn diagnostics_use_source_bytes_and_protected_evidence_is_unchanged() {
    let s = "Use \u{60}é; don't\u{60} with the cache; do not stop.";
    let mut l = Language::test_fixture();
    let r = l.validate(&note(s));
    let errors: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| matches!(d.severity, Severity::Error))
        .collect();
    assert_eq!(errors.len(), 1, "{r:?}");
    assert_eq!(&s[errors[0].range.clone()], ";");
    assert_eq!(s.as_bytes()[errors[0].range.start], b';');
    let mut accepted = note("Keep the exact error.\n\n~~~text\né; don't rewrite E0502.\n~~~");
    assert!(l.validate(&accepted).accepted);
    accepted.content = "~~~text\nexact evidence\n~~~".into();
    assert!(!l.validate(&accepted).accepted);
    let r = l.validate(&note("Do **not** remove the source; keep it."));
    assert!(r.diagnostics.iter().any(|d| d.rule == "8.1"));
}
#[test]
fn rejects_before_inference_or_any_database_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open_test(&dir.path().join("db"), None).unwrap();
    let before = e.store.stats().unwrap();
    let generation: String = e
        .store
        .db
        .query_row(
            "SELECT value FROM metadata WHERE key='retrieval_generation'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let bad = note("Don't remove the source; keep it.");
    let report = e.store.validate_memory(&bad);
    assert!(!report.accepted);
    assert!(e.remember(bad.clone()).is_err());
    assert!(
        e.store
            .put(
                bad,
                &[Chunk {
                    text: "irrelevant".into(),
                    vector: None
                }]
            )
            .is_err()
    );
    assert_eq!(before, e.store.stats().unwrap());
    assert_eq!(
        generation,
        e.store
            .db
            .query_row::<String, _, _>(
                "SELECT value FROM metadata WHERE key='retrieval_generation'",
                [],
                |r| r.get(0)
            )
            .unwrap()
    );
    assert_eq!(e.cache_info()["models"], serde_json::Value::Null);
    let saved = e.remember(note("Keep the exact source.")).unwrap();
    assert!(saved.language.unwrap().accepted);
}
#[test]
fn missing_dictionary_blocks_writes_but_keeps_reads() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let mut e = Engine::open_test(&db, None).unwrap();
    let m = e.remember(note("Keep the exact source.")).unwrap();
    e.store
        .set_language(Language::load(&dir.path().join("missing")));
    let r = e.store.validate_memory(&note("Keep the exact source."));
    assert!(!r.accepted);
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.rule == "enfour.language_data")
    );
    assert!(e.remember(note("Keep the exact source.")).is_err());
    let rejected = e.store.forget(&m.scope, &m.id, m.revision).unwrap_err();
    assert!(
        rejected
            .downcast_ref::<enfour_memory::language::Rejected>()
            .is_some()
    );
    let rejected = e
        .store
        .relate(enfour_memory::store::Relation {
            scope: m.scope.clone(),
            from: m.id.clone(),
            to: "other".into(),
            kind: "supports".into(),
            source: "test://relation".into(),
            from_revision: 1,
            to_revision: 1,
        })
        .unwrap_err();
    assert!(
        rejected
            .downcast_ref::<enfour_memory::language::Rejected>()
            .is_some()
    );
    assert_eq!(e.store.get(&m.scope, &m.id).unwrap().id, m.id);
}
#[test]
fn exact_cache_and_advisory_findings_do_not_change_acceptance() {
    let mut l = Language::test_fixture();
    let n = note("The source was changed.");
    let first = l.validate(&n);
    assert!(first.accepted, "{first:?}");
    assert!(first.diagnostics.iter().any(|d| d.rule == "3.6"));
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(l.validate(&n)).unwrap()
    );
    assert!(l.info()["cache"]["hits"].as_u64().unwrap() >= 2);
    assert!(
        !l.validate(&note("The source was changed; do not stop."))
            .accepted
    );
    l.set_cache_enabled(false);
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(l.validate(&n)).unwrap()
    );
}
#[test]
fn sentence_and_paragraph_limits_are_independent() {
    let mut l = Language::test_fixture();
    assert!(
        l.validate(&note("DON'T remove the source."))
            .diagnostics
            .iter()
            .any(|d| d.rule == "4.2" && matches!(d.severity, Severity::Error))
    );
    let long = format!("{}.", vec!["source"; 21].join(" "));
    assert!(
        l.validate(&note(&long))
            .diagnostics
            .iter()
            .any(|d| d.rule == "enfour.20")
    );
    let long = format!("Keep the source ({}).", vec!["source"; 21].join(" "));
    assert!(
        l.validate(&note(&long))
            .diagnostics
            .iter()
            .any(|d| d.rule == "8.5")
    );
    assert!(
        l.validate(&note(&"Keep the source. ".repeat(7)))
            .diagnostics
            .iter()
            .any(|d| d.rule == "6.6")
    );
}
#[test]
fn chunks_pack_sentences_and_never_drop_literal_bytes() {
    let s = "Keep the source. Do not remove it.\n\n~~~text\né=3.5; ".to_string()
        + &"🦀exact\n".repeat(100)
        + "~~~";
    let parts = text::pack(&s, |p| Ok(p.chars().count() <= 40)).unwrap();
    assert_eq!(parts.concat(), s);
    assert!(parts.iter().all(|p| p.chars().count() <= 40));
    assert!(parts[0].contains("Keep the source. Do not remove it."));
    assert!(text::pack("🦀", |_| Ok(false)).is_err());
}

#[test]
fn a_rejected_write_does_not_load_a_model_session() {
    // Only model metadata is present. Any attempt to infer would fail on missing weights.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("models");
    std::fs::create_dir_all(root.join("embedding")).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_str(include_str!("../model-manifest.json")).unwrap();
    manifest["embedding"]["files"]["config.json"] =
        enfour_memory::language::dictionary::digest(r#"{"hidden_size":384}"#).into();
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(root.join("embedding/config.json"), r#"{"hidden_size":384}"#).unwrap();
    let mut engine = Engine::open_test(&dir.path().join("db"), Some(&root)).unwrap();
    let before = engine.models.as_ref().unwrap().cache_info();
    let error = engine
        .remember(note("Do not remove the source; keep it."))
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<enfour_memory::language::Rejected>()
            .is_some()
    );
    assert_eq!(before, engine.models.as_ref().unwrap().cache_info());
    assert_eq!(engine.store.stats().unwrap()["active_memories"], 0);
}
