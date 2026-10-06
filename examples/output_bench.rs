//! Small return-adapter microbenchmark; never loads models or runs retrieval.
use enfour_memory::output::OutputFormat;
use serde_json::{Value, json};
use std::{hint::black_box, time::Instant};

fn measure(name: &str, value: &Value, iterations: u32) {
    for format in [
        OutputFormat::Toon,
        OutputFormat::UglifyJson,
        OutputFormat::None,
    ] {
        let bytes = format.encode(value).unwrap().len();
        for _ in 0..20 {
            black_box(format.encode(black_box(value)).unwrap());
        }
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(format.encode(black_box(value)).unwrap());
        }
        println!(
            "{name},{},{bytes},{:.3}",
            format.as_str(),
            start.elapsed().as_secs_f64() * 1e6 / f64::from(iterations)
        );
    }
}

fn main() {
    println!("fixture,format,bytes,microseconds_per_encode");
    for count in [5, 16] {
        let hits: Vec<Value> = (0..count).map(|i| json!({
            "memory": {"id": format!("memory-{i}"), "scope": "repo:enfour", "key": format!("fact-{i}"),
                "title": "SQLite storage decision", "kind": "decision", "source": "test://benchmark",
                "revision": 1, "created_at": 1791288000, "updated_at": 1791288000,
                "expires_at": null, "deleted": false, "content_bytes": 240},
            "excerpt": "Use SQLite WAL for durable memory. Keep source evidence and immutable revisions. Reuse exact computations only; generation and expiry checks preserve snapshot consistency. Unicode remains intact: 雪 🦀.\nQuoted text: \"SQLite\".",
            "score": 0.032786883413791656, "lexical": true, "semantic": true, "rerank_score": 0.9375
        })).collect();
        measure(&format!("recall-{count}"), &json!(hits), 2000);
    }
    let graph: Value = serde_json::from_str(include_str!("../tests/fixtures/example-graph.json")).unwrap();
    measure("synthetic-example-graph", &graph, 2000);
}
