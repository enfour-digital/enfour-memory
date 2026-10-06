use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use clap::{Parser, Subcommand};
use enfour_memory::{
    engine::Engine,
    output::OutputFormat,
    server::{MemoryServer, Recall, Scope},
};
use rmcp::{
    ServiceExt,
    transport::{
        stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use subtle::ConstantTimeEq;

#[derive(Parser)]
#[command(
    version,
    about = "Source-backed local memory. No cloud inference or telemetry."
)]
struct Cli {
    #[arg(long, global = true, default_value = "state/memory.sqlite")]
    db: PathBuf,
    #[arg(long, global = true, default_value = "models")]
    models: PathBuf,
    #[arg(long, global = true)]
    lexical_only: bool,
    /// Bypass ephemeral computation caches for diagnostics and cold comparisons.
    #[arg(long, global = true)]
    no_cache: bool,
    /// Tool-result text format; HTTP minify headers override this per request.
    #[arg(long, global = true, value_enum, default_value = "toon")]
    minify: OutputFormat,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Connect an agent over stdio to the shared server; keep models in one process.
    Connect {
        #[arg(long, default_value = "http://127.0.0.1:7463/mcp")]
        url: String,
        #[arg(long, default_value = "state/access.token")]
        token_file: PathBuf,
    },
    /// Lightweight local HTTP liveness probe.
    Health {
        #[arg(long, default_value = "127.0.0.1:7463")]
        address: SocketAddr,
    },
    /// Serve the local dashboard, graph API and official Streamable HTTP MCP.
    Serve {
        #[arg(long, default_value = "127.0.0.1:7463")]
        bind: SocketAddr,
        #[arg(long, default_value = "state/access.token")]
        token_file: PathBuf,
        #[arg(
            long,
            value_delimiter = ',',
            default_value = "localhost,127.0.0.1,[::1]"
        )]
        hosts: Vec<String>,
    },
    /// Official MCP stdio transport. stdout contains protocol messages only.
    Stdio,
    /// Create a private HTTP token; never overwrite an existing token.
    Init {
        #[arg(long, default_value = "state/access.token")]
        token_file: PathBuf,
    },
    /// Verify SQLite and report counts and the configured inference mode.
    Doctor,
    /// Offline, atomic rebuild of derived indexes for a new embedding model.
    Reindex,
    /// Consistent SQLite backup, including revision history and indexes.
    Backup { destination: PathBuf },
    /// Export active graph JSON or Graphviz DOT to stdout.
    Export {
        scope: String,
        #[arg(long)]
        dot: bool,
    },
    /// Print a stable project scope from the origin URL (without credentials).
    Scope {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
}
#[derive(Clone)]
struct Guard {
    token_hash: [u8; 32],
    hosts: Vec<String>,
}
async fn guard(State(g): State<Guard>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let name = if host.starts_with('[') {
        host.split(']')
            .next()
            .map(|v| format!("{v}]"))
            .unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or("").to_string()
    };
    if !g.hosts.iter().any(|h| h == &name || h == host) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(origin) = req.headers().get("origin") {
        let ok = origin
            .to_str()
            .is_ok_and(|o| o == format!("http://{host}") || o == format!("https://{host}"));
        if !ok {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    if req.uri().path() == "/" || req.uri().path() == "/healthz" {
        return next.run(req).await;
    }
    let token = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    if !bool::from(hash.ct_eq(&g.token_hash)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if req.uri().path().starts_with("/mcp")
        && let Err(error) = OutputFormat::from_headers(req.headers(), OutputFormat::default())
    {
        return (StatusCode::BAD_REQUEST, error).into_response();
    }
    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
}
async fn recall(State(s): State<MemoryServer>, Query(r): Query<Recall>) -> Response {
    match s.run(move |e| e.recall(&r.scope, &r.query, r.limit)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
async fn graph(State(s): State<MemoryServer>, Query(r): Query<Scope>) -> Response {
    match s.run(move |e| e.store.graph(&r.scope)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
async fn dot(State(s): State<MemoryServer>, Query(r): Query<Scope>) -> Response {
    match s.run(move |e| e.store.graph_dot(&r.scope)).await {
        Ok(v) => ([("content-type", "text/vnd.graphviz; charset=utf-8")], v).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}
async fn status(State(s): State<MemoryServer>) -> Response {
    match s
        .run(|e| {
            let mut v = e.store.stats()?;
            v["mode"] = if e.models.is_some() {
                "hybrid"
            } else {
                "lexical_only"
            }
            .into();
            v["models"] = e.models.as_ref().map(|m| m.info()).into();
            v["cache"] = e.cache_info();
            v["rerank_candidates"] = e
                .models
                .as_ref()
                .map(|_| enfour_memory::engine::DEFAULT_RERANK_CANDIDATES)
                .into();
            Ok(v)
        })
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
    }
}
#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Reindex => {
            ensure!(!cli.lexical_only, "reindex requires local models");
            let count = Engine::reindex(&cli.db, &cli.models)?;
            println!("Reindexed {count} current records; source history preserved.");
            return Ok(());
        }
        Command::Connect { url, token_file } => {
            return enfour_memory::bridge::connect(url.clone(), token_file, cli.minify).await;
        }
        Command::Health { address } => {
            use std::io::{Read, Write};
            let mut stream =
                std::net::TcpStream::connect_timeout(address, std::time::Duration::from_secs(2))?;
            stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
            stream.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
            write!(
                stream,
                "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
            )?;
            let mut response = [0; 12];
            stream.read_exact(&mut response)?;
            ensure!(&response == b"HTTP/1.1 200", "HTTP probe failed");
            return Ok(());
        }
        Command::Init { token_file } => {
            if let Some(p) = token_file.parent() {
                std::fs::create_dir_all(p)?;
            }
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(token_file)
                .context("token already exists or cannot be created")?;
            writeln!(
                f,
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            )?;
            f.sync_all()?;
            println!(
                "Token created at {}. Start with: enfour-memory serve",
                token_file.display()
            );
            return Ok(());
        }
        Command::Scope { path } => {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(["remote", "get-url", "origin"])
                .output()?;
            ensure!(
                output.status.success(),
                "project needs an origin remote or an explicit scope"
            );
            let raw = String::from_utf8(output.stdout)?;
            let raw = raw.trim().trim_end_matches('/').trim_end_matches(".git");
            let raw = raw
                .split_once("://")
                .map_or(raw, |(_, s)| s)
                .rsplit('@')
                .next()
                .unwrap_or(raw);
            ensure!(
                !raw.contains('?') && !raw.contains('#'),
                "remote URL has query or fragment; use an explicit scope"
            );
            println!("repo:{}", raw.replacen(':', "/", 1));
            return Ok(());
        }
        _ => {}
    }
    let mut engine = Engine::open(
        &cli.db,
        if cli.lexical_only {
            None
        } else {
            Some(&cli.models)
        },
    )?;
    engine.set_cache_enabled(!cli.no_cache);
    match cli.command {
        Command::Stdio => {
            MemoryServer::new(engine)
                .with_output_format(cli.minify)
                .serve(stdio())
                .await?
                .waiting()
                .await?;
        }
        Command::Doctor => {
            engine.store.check()?;
            println!("{}", engine.store.stats()?);
        }
        Command::Backup { destination } => {
            engine.store.backup(&destination)?;
            println!("{}", destination.display());
        }
        Command::Export { scope, dot } => {
            if dot {
                print!("{}", engine.store.graph_dot(&scope)?)
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&engine.store.graph(&scope)?)?
                )
            }
        }
        Command::Serve {
            bind,
            token_file,
            hosts,
        } => {
            let token =
                std::fs::read_to_string(token_file).context("run enfour-memory init first")?;
            ensure!(token.trim().len() >= 32, "HTTP token is too short");
            let g = Guard {
                token_hash: Sha256::digest(token.trim().as_bytes()).into(),
                hosts: hosts.clone(),
            };
            let s = MemoryServer::new(engine).with_output_format(cli.minify);
            let mcp = s.clone();
            let ct = tokio_util::sync::CancellationToken::new();
            let service = StreamableHttpService::new(
                move || Ok(mcp.clone()),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig::default()
                    .with_allowed_hosts(hosts)
                    .with_cancellation_token(ct.child_token()),
            );
            let app = Router::new()
                .route(
                    "/",
                    get(|| async { Html(include_str!("../assets/index.html")) }),
                )
                .route("/healthz", get(|| async { "ok" }))
                .route("/api/status", get(status))
                .route("/api/recall", get(recall))
                .route("/api/graph", get(graph))
                .route("/api/graph.dot", get(dot))
                .nest_service("/mcp", service)
                .with_state(s)
                .layer(tower_http::limit::RequestBodyLimitLayer::new(65536))
                .layer(middleware::from_fn_with_state(g, guard));
            let listener = tokio::net::TcpListener::bind(bind).await?;
            eprintln!(
                "Enfour Memory listening on http://{}",
                listener.local_addr()?
            );
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let mut term =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                            .expect("signal handler");
                    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
                    ct.cancel();
                })
                .await?;
        }
        Command::Init { .. }
        | Command::Scope { .. }
        | Command::Connect { .. }
        | Command::Reindex
        | Command::Health { .. } => unreachable!(),
    }
    Ok(())
}
