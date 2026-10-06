use enfour_memory::{engine::Engine, store::Remember};
use std::{path::Path, time::Instant};
#[test]
#[ignore = "requires explicitly provisioned local models"]
fn local_inference_retrieval_and_footprint() {
    let dir = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let model_dir = std::env::var("ENFOUR_MODELS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "models".into());
    let mut engine =
        Engine::open(&dir.path().join("memory.db"), Some(Path::new(&model_dir))).unwrap();
    let notes = [
        (
            "database",
            "Storage decision",
            "Enfour uses SQLite with WAL and FTS5. Backups use the SQLite online backup API.",
        ),
        (
            "build",
            "Build cache",
            "Reuse Rust compilation results through sccache and a local Valkey compiler cache.",
        ),
        (
            "auth",
            "Access",
            "HTTP requests require a bearer token. The memory service is private to the homelab.",
        ),
        (
            "embed",
            "Embedding model",
            "BGE small is the local embedding model. Its dense vectors have 384 dimensions.",
        ),
        (
            "graph",
            "Graph export",
            "Memory nodes and typed evidence-backed edges can be exported as versioned JSON and Graphviz DOT.",
        ),
        (
            "editor",
            "Editor preference",
            "Use Neovim for editing Rust source code.",
        ),
        (
            "port",
            "Network listener",
            "The default Enfour Memory HTTP listener is 127.0.0.1 port 7463.",
        ),
        (
            "delete",
            "Deletion",
            "Forgetting hides a note from current retrieval but preserves its historical revisions.",
        ),
    ];
    for (key, title, content) in notes {
        engine
            .remember(Remember {
                scope: "repo:test".into(),
                key: key.into(),
                title: title.into(),
                content: content.into(),
                kind: "fact".into(),
                source: format!("test://{key}"),
                expected_revision: 0,
                expires_at: None,
            })
            .unwrap();
    }
    println!(
        "cold_load_and_eight_writes_ms={}",
        start.elapsed().as_millis()
    );
    let cases = [
        ("How are compilation results reused?", "build"),
        (
            "How do I draw the relationships between remembered facts?",
            "graph",
        ),
        ("Which database is used?", "database"),
        (
            "What happens to old versions when a note is removed?",
            "delete",
        ),
    ];
    let mut elapsed = Vec::new();
    let mut correct = 0;
    let sanity = engine
        .models
        .as_mut()
        .unwrap()
        .rerank(
            "What is the capital of France?",
            &[
                "Paris is the capital of France.".into(),
                "Bananas grow in tropical climates.".into(),
            ],
        )
        .unwrap();
    assert_eq!(sanity[0].0, 0);
    let mut retrieved = 0;
    for (query, expected) in cases {
        let t = Instant::now();
        let hits = engine.recall("repo:test", query, 3).unwrap();
        elapsed.push(t.elapsed().as_millis());
        println!(
            "query={query:?} first={} elapsed_ms={} score={:?}",
            hits[0].memory.key,
            elapsed.last().unwrap(),
            hits[0].rerank_score
        );
        println!(
            "ranking={:?}",
            hits.iter()
                .map(|h| (&h.memory.key, h.rerank_score))
                .collect::<Vec<_>>()
        );
        if hits.iter().any(|h| h.memory.key == expected) {
            retrieved += 1;
        }
        if hits[0].memory.key == expected {
            correct += 1;
        }
    }
    assert!(
        engine
            .recall("repo:other", "database", 3)
            .unwrap()
            .is_empty()
    );
    println!("latency_ms={elapsed:?}");
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status
            .lines()
            .filter(|l| l.starts_with("VmHWM:") || l.starts_with("VmRSS:"))
        {
            println!("{line}")
        }
    }
    println!("top1={correct}/4 recall_at_3={retrieved}/4");
    // The API returns evidence candidates for an agent reader; require coverage
    // within the requested budget and report top-one ranking separately.
    assert_eq!(
        retrieved, 4,
        "evidence missing from the three-candidate budget"
    );
    // Compare completed-result caching against a fresh computation in the same index.
    let query = "Which database is used?";
    let cached = engine.recall("repo:test", query, 3).unwrap();
    let warm = Instant::now();
    let repeated = engine.recall("repo:test", query, 3).unwrap();
    println!("cached_recall_us={}", warm.elapsed().as_micros());
    engine.set_cache_enabled(false);
    let uncached = engine.recall("repo:test", query, 3).unwrap();
    assert_eq!(
        serde_json::to_value(&cached).unwrap(),
        serde_json::to_value(&repeated).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&cached).unwrap(),
        serde_json::to_value(&uncached).unwrap()
    );
}
