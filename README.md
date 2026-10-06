# Enfour Memory

A small Rust memory service for coding agents: source-backed records, local
embeddings and reranking, revision history, graph exports, a browser search
page, and an official `rmcp` MCP server. Runs on the ThinkPad; no cloud
inference or telemetry in the service.

See the [architecture diagrams](docs/architecture.md) for the system overview,
recall processing order, and atomic write path.

The deployed v0.2 profile was selected on a fixed retrieval development split.
Read the [benchmark report](docs/benchmark-results.md), [research audit](docs/research.md)
and [validation record](docs/validation.md) for evidence and limitations.
Development evidence Recall@5 improved from 66.53% to 74.63% across 340
questions. The larger held-out study was deferred; this is not a claim of
independent test accuracy or a state-of-the-art leaderboard result.

## Start on the homelab

Requires the existing local Docker daemon, `enfour-compiler-cache` network,
Valkey compiler cache and `enfour-cargo-downloads` volume.

```sh
scripts/cargo-local image
scripts/cargo-local fetch
scripts/download-models
scripts/cargo-local test --locked --offline -- --include-ignored
scripts/cargo-local build --release --locked --offline
scripts/enfour up
```

Open **http://127.0.0.1:7463**. The first start creates `state/access.token` with
private permissions. Paste its contents into the page; the page does not save
the token in browser storage. `scripts/enfour status` shows service health.
The container restarts with the local Docker daemon. `scripts/enfour down`
stops it while preserving state and models.

The default bind is loopback. To publish on the server LAN interface:

```sh
printf 'ENFOUR_BIND=127.0.0.1\n' > .env
ENFOUR_URL=http://memory.example.com:7463/mcp scripts/enfour up
```

Open **http://memory.example.com:7463**. The ignored `.env` preserves the bind address
for later Compose commands. The connector remembers the endpoint in
`state/server.url`; `ENFOUR_URL` can override it. Do not publish the raw HTTP
service on the Internet.

## Connect an agent once

The connector uses rmcp on both sides and reads the private token itself.
It forwards tools to the shared service, keeping one resident set of models.
On the ThinkPad, registration is already installed for Codex and Grok. Start
a new client session to load it. To register a different checkout:

```sh
scripts/install-clients --codex-source /home/user/nix-config-copy/home/user/configuration/codex/config.toml
```

The installer preserves other settings, saves private backups and refuses to
overwrite a conflicting Enfour entry. It upgrades the previous managed
60-second timeout to 180 seconds for deeper local reranking, preserving other
settings. Omit `--codex-source` on systems without
Home Manager. Alternatively add the entry manually, using the actual absolute path:

```toml
[mcp_servers.enfour-memory]
command = "/absolute/path/to/enfour-memory/scripts/enfour"
args = ["connect"]
startup_timeout_sec = 30
tool_timeout_sec = 180
```

On this NixOS setup, put persistent Codex configuration in the Home Manager
source. Its three-way defaults merge preserves runtime edits; the installer
updates both the source and live configuration for an explicit persistent default.
Grok can use the same stdio command in its MCP configuration. Clients that
support remote MCP directly can use `/mcp` with bearer authentication.
The scripts always target the ThinkPad's explicit local Docker socket.

No per-project memory daemon, database or index setup is required. Use the
repository origin as the scope, for example `repo:example.com/memory-demo`.
The native `enfour-memory scope /path/to/repo` helper prints the normalized
origin identity. Repositories with no origin need an explicit stable scope.
The service does not infer identity from ambiguous folder names.

Agents should recall relevant decisions before work, inspect before replacing
a key, and remember only useful verified findings with evidence. MCP alone
cannot force automatic capture; client behavior remains part of the system.
Existing Basic Memory data and integration are not migrated by this project.

## Six tools

| Tool | Purpose |
| --- | --- |
| `recall` | Scoped lexical+dense retrieval, reciprocal rank fusion and bounded local reranking; return source evidence to the agent reader. |
| `remember` | Create or revise a fact, preference, decision, lesson or checkpoint. Requires evidence and an expected revision. |
| `inspect` | Read a memory's revision history, including deleted versions. |
| `forget` | Hide a memory from recall and the active graph; retain audit history. |
| `relate` | Add an evidence-backed typed link between current revisions in one scope. |
| `graph` | Export the active graph as versioned JSON with stable IDs and provenance. |

The HTTP API also exposes `GET /api/status`, `/api/recall?scope=…&query=…`,
`/api/graph?scope=…`, and `/api/graph.dot?scope=…`. Every API request needs
`Authorization: Bearer …`. `/` and `/healthz` contain no memory data.
The status API includes the active embedding/reranker repositories, pinned
revisions, vector dimension and embedding identity.
JSON graph nodes contain complete current records; edges name their endpoint
revisions and evidence. Updating or deleting an endpoint hides stale links
until they are revalidated. A frontend can build directly on this data.
Graphviz can render DOT with `dot -Tsvg graph.dot -o graph.svg`.

MCP tool-result text defaults to TOON in v0.3.0. Send `minify: uglify-json` for
compact JSON or `minify: none` for pretty JSON; `minify: toon` selects the default.
The MCP envelope, inputs, data APIs and underlying memory/inference stay JSON
or typed data. See [output adapters](docs/output-formats.md) for Codex settings,
stdio options, the local single-file encoder and conformance checks.

Scopes prevent accidental cross-project recall; they are not separate users
or access-control domains. The owner token grants access to all scopes.
Retrieved text is evidence, never executable instructions. Ranking measures
relevance, not factual truth; a high rank is not a verification claim.

## Storage and recovery

`state/memory.sqlite` holds records, immutable revisions, relationships, FTS5
and 384-dimensional vectors. SQLite WAL, FULL synchronization, foreign keys
and revision checks protect committed updates. Exact vector scanning avoids
an ANN service and index tuning; measure before scaling to large corpora.

```sh
scripts/enfour cli --lexical-only doctor
scripts/enfour cli --lexical-only backup /data/backup-2026-10-06.sqlite
scripts/enfour cli --lexical-only export repo:example.com/memory-demo
scripts/enfour cli --lexical-only export repo:example.com/memory-demo --dot
```

Backup uses SQLite's online backup API and refuses to overwrite a destination.
It contains history and indexes, not the HTTP token or model files. Keep a
separate protected copy of the token and the model manifest. To restore, stop
the service, preserve the entire old state directory including WAL/SHM files,
restore the backup into a fresh state directory as `memory.sqlite`, restore
the token with mode 0600, run `doctor`, then start the service. Never copy a
live database's main file as a backup or mix an old WAL with a restored file.

Soft deletion preserves history and is not secure erasure. No automatic
forgetting, transcript ingestion or background LLM consolidation runs.
Embedding identity includes weights/tokenizer hashes and preprocessing;
opening with a different identity is rejected to prevent mixed-vector search.
Use the offline `reindex` command when changing embeddings; see
[model maintenance](docs/model-maintenance.md) for backup and rollback steps.

## Footprint and dependencies

The v0.2 model choice is quantized BGE-small-en-v1.5 plus GTE ModernBERT
reranking, selected on 340 LoCoMo development questions from nine configurations.
English is the initial retrieval target. Model files total about 211 MiB; inference
loads lazily, uses two ONNX intra-op threads, and admits at most eight requests
with serialized inference. Candidate reranking is bounded to 64 records; the default response contains
five excerpts. Lexical-only mode keeps its smaller candidate budget.
`--lexical-only` is an explicit mode when local inference is not wanted.

The build pins Rust 1.99.0, official rmcp 3.5.1, axum 0.8.9 and fastembed 7.1.0,
with a committed lockfile. Stable fastembed requires a prerelease `ort` binding;
see [build details](docs/build.md). No Python interpreter or compiler is needed
in the running service. Python is used only for provisioning and wire tests.

v0.2.1 adds automatic exact caching for completed rankings, embeddings,
four-passage reranker batches and token-aware chunking. Moka's estimated entry
budget totals 64 MiB. Database revisions, read snapshots and expiry windows
protect freshness; cached and uncached selected-model outputs are checked for
equivalence. Cache counters appear in authenticated `/api/status`; `--no-cache`
bypasses reuse for diagnostics. See [cache design and validation](docs/caching.md).

The v0.2 executable measures 36.5 MiB, its runtime image 122.8 MiB, and its
two-record deployment smoke test about 541 MiB process RSS. Compose allows
3 GiB and retains a lightweight local health probe. Warm searches over those
two records took 307–311 ms; the 64-candidate development run had a 12-second
median under concurrent CPU load. These are different workloads, not a latency
guarantee. Longer documents and larger candidate sets cost more inference time.

Routine changes receive targeted checks. Full dataset evaluations are reserved
for substantial retrieval changes or an explicit evaluation milestone, never
automatically for every commit. Completed predictions and model selection are
kept so documentation, client or UI edits do not repeat expensive inference.

Soft Serve repository: `ssh://git@git.example.com/enfour-memory.git`.
The [example graph](docs/example-graph.json) and [Graphviz export](docs/example-graph.dot)
contain two source-backed architecture records created through the deployed MCP
connector. See the [validation record](docs/validation.md) for the checks performed.
