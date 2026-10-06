//! Offline retrieval evaluator input boundary: never reads answers or relevance labels.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use enfour_memory::{
    engine::Engine,
    models::digest_hex,
    store::{Chunk, Remember},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    time::Instant,
};

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    db: PathBuf,
    #[arg(long)]
    models: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Index {
        corpus: PathBuf,
        /// Keep all lexical documents, but embed only these independent scopes.
        #[arg(long)]
        embed_scope: Vec<String>,
    },
    Query {
        queries: PathBuf,
        output: PathBuf,
        #[arg(long, default_value = "baseline")]
        strategy: String,
        #[arg(long, default_value_t = 16)]
        candidates: usize,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    id: String,
    scope: String,
    query: String,
}
fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut engine = Engine::open(&cli.db, cli.models.as_deref())?;
    match cli.command {
        Command::Index {
            corpus,
            embed_scope,
        } => {
            let digest = digest_hex(std::fs::read(&corpus)?);
            let existing: Option<String> = engine
                .store
                .db
                .query_row(
                    "SELECT value FROM metadata WHERE key='benchmark_corpus'",
                    [],
                    |r| r.get(0),
                )
                .ok();
            if let Some(old) = existing {
                ensure!(old == digest, "corpus identity changed");
            } else {
                engine.store.db.execute(
                    "INSERT INTO metadata VALUES('benchmark_corpus',?)",
                    [&digest],
                )?;
            }
            let start = Instant::now();
            let mut count = 0;
            for line in BufReader::new(File::open(corpus)?).lines() {
                let note: Remember = serde_json::from_str(&line?)?;
                let needs_vectors = embed_scope.is_empty() || embed_scope.contains(&note.scope);
                if let Ok(old) = engine.store.get(&note.scope, &note.key) {
                    ensure!(
                        old.content == note.content
                            && old.title == note.title
                            && old.source == note.source,
                        "existing benchmark document differs"
                    );
                    if needs_vectors && let Some(models) = engine.models.as_mut() {
                        // Fill only missing vectors; preserve FTS row IDs and source revisions.
                        let missing: Vec<(i64, String)> = {
                            let mut q=engine.store.db.prepare("SELECT id,text FROM chunks WHERE memory_id=? AND vector IS NULL ORDER BY id")?;
                            q.query_map([&old.id], |r| Ok((r.get(0)?, r.get(1)?)))?
                                .collect::<std::result::Result<_, _>>()?
                        };
                        for (id, text) in missing {
                            let v = models.embed(vec![text], false)?.remove(0);
                            let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
                            engine.store.db.execute(
                                "UPDATE chunks SET vector=? WHERE id=?",
                                rusqlite::params![bytes, id],
                            )?;
                        }
                    }
                } else if !needs_vectors && let Some(models) = engine.models.as_mut() {
                    let texts =
                        models.chunks(&format!("{} — {}", note.key, note.title), &note.content)?;
                    let chunks: Vec<_> = texts
                        .into_iter()
                        .map(|text| Chunk { text, vector: None })
                        .collect();
                    engine.store.put(note, &chunks)?;
                } else {
                    engine.remember(note)?;
                }
                count += 1;
                if count % 250 == 0 {
                    eprintln!(
                        "indexed={count} elapsed_s={:.1}",
                        start.elapsed().as_secs_f64()
                    );
                }
            }
            engine.store.db.execute("INSERT INTO metadata VALUES('benchmark_corpus_complete',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [&digest])?;
            engine.store.db.execute("INSERT INTO metadata VALUES('benchmark_corpus_count',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [count.to_string()])?;
            eprintln!(
                "{}",
                json!({"indexed":count,"seconds":start.elapsed().as_secs_f64(),"stats":engine.store.stats()?})
            );
        }
        Command::Query {
            queries,
            output,
            strategy,
            candidates,
        } => {
            ensure!(
                ["baseline", "hybrid", "lexical"].contains(&strategy.as_str()),
                "unknown strategy"
            );
            ensure!(
                strategy == "lexical" || engine.models.is_some(),
                "semantic benchmark strategies require --models"
            );
            let query_hash = digest_hex(std::fs::read(&queries)?);
            let complete:bool=engine.store.db.query_row("SELECT (SELECT value FROM metadata WHERE key='benchmark_corpus_complete')=(SELECT value FROM metadata WHERE key='benchmark_corpus') AND (SELECT value FROM metadata WHERE key='benchmark_corpus_count')=CAST((SELECT count(*) FROM memories WHERE deleted=0) AS TEXT)",[],|r|r.get::<_,Option<bool>>(0))?.unwrap_or(false);
            ensure!(
                complete,
                "corpus import incomplete; run index to completion first"
            );
            if strategy != "lexical" {
                // A development-only embedding cache must never silently score held-out scopes.
                let mut scopes = std::collections::HashSet::new();
                for line in BufReader::new(File::open(&queries)?).lines() {
                    scopes.insert(serde_json::from_str::<Query>(&line?)?.scope);
                }
                for scope in scopes {
                    let missing:i64=engine.store.db.query_row("SELECT count(*) FROM chunks c JOIN memories m ON m.id=c.memory_id WHERE m.scope=? AND c.vector IS NULL",[scope],|r|r.get(0))?;
                    ensure!(
                        missing == 0,
                        "query scope has missing embeddings; finish indexing first"
                    );
                }
            }
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output)
                .context("output must be new")?;
            for (index, line) in BufReader::new(File::open(queries)?).lines().enumerate() {
                let q: Query = serde_json::from_str(&line?)?;
                let start = Instant::now();
                let hits = if strategy == "baseline" {
                    engine.recall_with_candidates(&q.scope, &q.query, 16, candidates)?
                } else {
                    let vector = if strategy == "hybrid" {
                        Some(
                            engine
                                .models
                                .as_mut()
                                .context("hybrid needs models")?
                                .embed(vec![q.query.clone()], true)?
                                .remove(0),
                        )
                    } else {
                        None
                    };
                    engine.store.candidates_with_limit(
                        &q.scope,
                        &q.query,
                        vector.as_deref(),
                        candidates,
                    )?
                };
                let rows:Vec<_>=hits.iter().map(|h|json!({"key":h.memory.key,"score":h.score,"rerank_score":h.rerank_score,"bytes":h.excerpt.len()})).collect();
                writeln!(
                    file,
                    "{}",
                    json!({"id":q.id,"scope":q.scope,"strategy":strategy,"candidates":candidates,"query_file_sha256":query_hash,"ms":start.elapsed().as_secs_f64()*1000.,"hits":rows})
                )?;
                file.flush()?;
                if (index + 1) % 50 == 0 {
                    eprintln!("queries={}", index + 1);
                }
            }
        }
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for l in status.lines().filter(|l| l.starts_with("VmHWM:")) {
            eprintln!("{l}");
        }
    }
    Ok(())
}
