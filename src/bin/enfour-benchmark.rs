//! Offline retrieval evaluator input boundary: never reads answers or relevance labels.
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use enfour_memory::{engine::Engine, models::digest_hex, store::Remember};
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
    },
    Query {
        queries: PathBuf,
        output: PathBuf,
        #[arg(long, default_value = "baseline")]
        strategy: String,
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
        Command::Index { corpus } => {
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
                    [digest],
                )?;
            }
            let start = Instant::now();
            let mut count = 0;
            for line in BufReader::new(File::open(corpus)?).lines() {
                let note: Remember = serde_json::from_str(&line?)?;
                if let Ok(old) = engine.store.get(&note.scope, &note.key) {
                    ensure!(
                        old.content == note.content
                            && old.title == note.title
                            && old.source == note.source,
                        "existing benchmark document differs"
                    );
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
            eprintln!(
                "{}",
                json!({"indexed":count,"seconds":start.elapsed().as_secs_f64(),"stats":engine.store.stats()?})
            );
        }
        Command::Query {
            queries,
            output,
            strategy,
        } => {
            ensure!(
                ["baseline", "hybrid", "lexical"].contains(&strategy.as_str()),
                "unknown strategy"
            );
            let query_hash = digest_hex(std::fs::read(&queries)?);
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output)
                .context("output must be new")?;
            for (index, line) in BufReader::new(File::open(queries)?).lines().enumerate() {
                let q: Query = serde_json::from_str(&line?)?;
                let start = Instant::now();
                let hits = if strategy == "baseline" {
                    engine.recall(&q.scope, &q.query, 16)?
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
                    engine
                        .store
                        .candidates(&q.scope, &q.query, vector.as_deref())?
                };
                let rows:Vec<_>=hits.iter().map(|h|json!({"key":h.memory.key,"score":h.score,"rerank_score":h.rerank_score,"bytes":h.excerpt.len()})).collect();
                writeln!(
                    file,
                    "{}",
                    json!({"id":q.id,"scope":q.scope,"strategy":strategy,"query_file_sha256":query_hash,"ms":start.elapsed().as_secs_f64()*1000.,"hits":rows})
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
