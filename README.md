# Enfour Memory

Local RAG for coding agents, with keyword search and semantic search in one Rust service.
Keep facts, exact evidence, and revision history across sessions.

[Connect](#connect) · [Architecture](#architecture) · [Self-host](#self-host) · [Measurements](#measurements) · [Reference](#reference)

## Connect

Use an existing server's URL and bearer token. Add this to `~/.codex/config.toml` on the agent's computer:

```toml
[mcp_servers.enfour-memory]
url = "http://memory.example.com:7463/mcp"
http_headers = { Authorization = "Bearer YOUR_TOKEN" }
startup_timeout_sec = 30
tool_timeout_sec = 180
```

Replace the hostname and token. Keep the configuration private, then restart the client.
Both `/mcp` and `/mcp/rag` use hybrid retrieval and reranking.
One token gives access to all scopes. Use a trusted LAN, TLS proxy, or SSH tunnel.

Install the skills on the agent's computer with Node.js and `npx`:

```sh
DISABLE_TELEMETRY=1 npx skills add http://memory.example.com:7463 --agent codex --skill enfour-recall enfour-maintain -g
```

The skills select a stable project scope, check saved claims, and help with memory writes. Try:

> Save this project decision: use SQLite for local storage. Use this session as the source.

In a new session:

> Recall the storage decision for this project and show its source.

The server runs the local models. No model files or memory checkout are necessary on clients.
Enfour uses no cloud inference or telemetry. Your agent can send retrieved text to its model provider.

## Architecture

SQLite stores source records and revisions. The graphs show the default RAG service, its write checks, and its recall order.

<table>
<thead><tr><th>RAG service</th></tr></thead>
<tbody><tr><td nowrap>MCP and HTTP · writing checks · local models · SQLite storage · exact caches · result adapters<br/><a href="assets/diagrams/rag.light.generated.svg"><picture><source media="(prefers-color-scheme: dark)" srcset="assets/diagrams/rag.dark.generated.svg"><img src="assets/diagrams/rag.light.generated.svg" width="880" alt="RAG service: authenticated MCP, writing checks, retrieval, SQLite, caches, output adapters, and graph exports."></picture></a></td></tr></tbody>
</table>

<table>
<thead><tr><th>Save a memory</th><th>Recall a memory</th></tr></thead>
<tbody><tr>
<td valign="top" nowrap>STE and Harper → sentence chunks → embeddings → SQLite<br/><a href="assets/diagrams/write.light.generated.svg"><picture><source media="(prefers-color-scheme: dark)" srcset="assets/diagrams/write.dark.generated.svg"><img src="assets/diagrams/write.light.generated.svg" width="440" alt="Write order: exact evidence, STE checks, Harper advice, error report, chunks, embeddings, and SQLite commit."></picture></a></td>
<td valign="top" nowrap>Query → keyword and vector search → fusion → reranking<br/><a href="assets/diagrams/recall.light.generated.svg"><picture><source media="(prefers-color-scheme: dark)" srcset="assets/diagrams/recall.dark.generated.svg"><img src="assets/diagrams/recall.light.generated.svg" width="440" alt="Recall order: snapshot, exact cache, query embedding, keyword and vector search, fusion, reranking, and output adapter."></picture></a></td>
</tr></tbody>
</table>

Open each graph for a larger view.

STE errors block writes. Harper findings are advisory. Missing private language data also blocks writes.
The agent reviews meaning, negation, quantities, conditions, and uncertainty. No model changes the prose.

Moka caches exact inputs. Cache inputs and versions must be the same. Recall keys also include scope, database generation, and expiry state.
Expired or changed records cannot use stale rankings. An empty cache only adds computation.

Graph exports return scoped JSON or DOT for a frontend. Tool adapters change return text only.

<details>
<summary>Native Mermaid diagrams and source</summary>

### RAG service

<!-- diagram: rag -->
```mermaid
flowchart TB
    agent("Coding agent") --> http("axum · HTTP + bearer token<br/>rmcp · /mcp and /mcp/rag")
    viewer("Browser / graph viewer") --> http
    subgraph service["One local Rust service"]
        http --> engine("Memory engine<br/>Scope · evidence · revisions")
        engine --> writes("Write path<br/>STE checks + Harper advice<br/>Sentence chunks → embeddings")
        engine --> reads("Recall path<br/>Keywords + vectors → fusion<br/>GTE ModernBERT reranking")
        writes --> db[("SQLite · source of truth<br/>Records · history · relations<br/>FTS5 · chunks · vectors")]
        db --> reads
        writes -.-> cache("Moka · exact computation<br/>Validation · chunks · embeddings<br/>Rerank batches · recall results")
        reads -.-> cache
        writes --> data("Typed data / JSON")
        reads --> data
        data --> adapter("Tool result adapter<br/>TOON · compact JSON · pretty JSON")
        db --> export("Graph API<br/>Versioned JSON / Graphviz DOT")
    end
    adapter --> client("Agent reads text<br/>or decodes TOON / JSON")
    export --> frontend("Frontend / graph viewer")
    classDef default filter:none
    classDef accent stroke:#8170c7,stroke-width:2px,filter:none
    class writes,adapter accent
```

### Save a memory

<!-- diagram: write -->
```mermaid
flowchart TB
    agent("Agent reviews facts and meaning<br/>Exact evidence + source") --> draft("validate_memory · optional preflight<br/>remember · mandatory write checks")
    draft --> parse("pulldown-cmark · source ranges<br/>Keep code, quotes, and identifiers exact")
    parse --> checks("Simplified Technical English · Issue 9 policy<br/>20 words / sentence · 6 sentences / paragraph<br/>Vocabulary · contractions · semicolons")
    dictionary("Private verified dictionary<br/>+ technical glossary") -.-> checks
    checks --> advice("Harper grammar advice<br/>+ contextual rule advice")
    advice --> gate{"Writing errors?"}
    gate -->|Yes| reject("Return rule, field, range, and repair guidance<br/>No inference or database change<br/>Agent repairs and submits again")
    gate -->|No| chunks("Sentence-aware chunks · 480-token budget<br/>Split long literals without truncation")
    chunks --> embed("fastembed · local BGE-small<br/>384-number vectors")
    embed --> tx("SQLite transaction · revision conflict check<br/>Save source, revision, chunks, vectors, and rule versions<br/>Update FTS5 and retrieval generation")
    tx --> result("Typed data / JSON → result adapter<br/>TOON / compact JSON / pretty JSON")
    classDef default filter:none
    classDef accent stroke:#8170c7,stroke-width:2px,filter:none
    class checks,advice,gate accent
```

### Recall a memory

<!-- diagram: recall -->
```mermaid
flowchart TB
    query("recall · scope + original query<br/>No prose rewrite or grammar gate") --> snapshot("SQLite read snapshot<br/>Read generation and expiry state")
    snapshot --> cache{"Exact result cache hit?"}
    cache -->|No| embed("fastembed · BGE-small query vector")
    embed --> retrieval("Scoped SQLite records · active and unexpired<br/>FTS5 / BM25 + exact vector search<br/>Up to 64 chunks from each search")
    retrieval --> fusion("Reciprocal rank fusion<br/>One passage per memory")
    fusion --> rerank("fastembed · GTE ModernBERT<br/>Rerank candidate passages")
    rerank --> save("Cache ranked results")
    save --> expiry("Recheck expiry before return<br/>Retry if expiry changed")
    cache -->|Yes| expiry
    expiry --> limit("Select requested result count<br/>Records + evidence + scores")
    limit --> data("Typed data / JSON")
    data --> adapter("Return adapter only · minify header<br/>toon · uglify-json · none")
    adapter --> client("Client reads text or decodes the result<br/>MCP envelope stays JSON")
    classDef default filter:none
    classDef accent stroke:#8170c7,stroke-width:2px,filter:none
    class cache,adapter accent
```

</details>

Use `main` for RAG. Agent memory development is on `experimental/agent-memory`. Use a different state directory for that branch.

## Self-host

Use a Linux x86-64 server with Docker Compose, Python 3.11 or newer, and about 3 GiB of RAM.
Get [ASD-STE100 Issue 9](https://www.asd-ste100.org/STE_downloads.html) and install Poppler.
The private dictionary is mandatory for writes. Keep the PDF and extracted dictionary out of Git.

Run these commands from the repository root:

```sh
scripts/enfour cargo image
scripts/enfour cargo fetch
scripts/enfour cargo build --release --locked --offline
scripts/enfour models
scripts/enfour language --pdf /private/ASD-STE100_ISSUE9.pdf
scripts/enfour up --build
```

Open <http://127.0.0.1:7463>. The token is in `state/access.token`.
For LAN access, copy `.env.example` to `.env`, set `ENFOUR_BIND`, and add the hostname to `ENFOUR_HOSTS`.
The default port is TCP 7463. The dashboard keeps its token only in memory.

Builds use the local Linux Docker socket, Cargo downloads, and sccache.
Use `scripts/enfour --help` for commands. Use `up --build` after a new release build.

<details>
<summary>NixOS and recovery</summary>

Import [nixos/module.nix](nixos/module.nix). Set `services.enfour-memory.enable = true` and `imageFile` to a pinned image archive.
Before activation, stop Compose. Copy state, models, and language data into the module's `dataDir`.
Use `state/`, `models/`, and `language/` there, with the configured `uid` and `gid`.
The boot service is `enfour-memory.service`.

Use systemd to control it. The Python commands control the checkout's Compose service.

Create a consistent database backup:

```sh
scripts/enfour cli --lexical-only backup /data/backup.sqlite
```

The destination must not exist. Also save the token, models, dictionary, and runtime image.

Stop writers before a database backup. Stop the service before restore or migration.
Keep the previous state, including WAL and SHM files, until verification is complete.

Existing records and revisions stay in SQLite after an update from the agent memory branch.
Old Git files and events stay in storage without new updates.

Rebuild indexes after model or chunk-format changes. Review existing memories before `migrate-language`. Keep the original revisions and migration backup.

</details>

## Measurements

Reference host: **ThinkPad T480s · i7-8650U · 16 GiB RAM**. Service limit: two CPUs and 3 GiB.
Quantized BGE-small-en-v1.5 and GTE ModernBERT use about 211 MiB of model files.

The selected profile measured **74.63% Recall@5** and **0.6779 nDCG@10** on 340 LoCoMo development queries.
These queries selected the profile. This is not an independent test result or leaderboard claim.

TOON measurements use v0.4 fixtures without validation metadata: 20 warmups and 2,000 measured runs for each case.
Times include result allocation, without inference, transport, or MCP envelopes. Bytes are not tokens.

| Payload | TOON bytes / µs | Compact JSON bytes / µs | Pretty JSON bytes / µs |
| --- | ---: | ---: | ---: |
| 5 recall hits | 2,127 / 15.519 | 2,936 / 3.304 | 3,652 / 4.042 |
| 16 recall hits | 6,485 / 37.037 | 9,405 / 9.708 | 11,694 / 12.978 |
| Graph | 1,554 / 5.878 | 1,737 / 1.700 | 2,079 / 2.407 |

Writing checks measured 1.626 ms at p50 without cache and 6.165 µs for exact cache hits.
This short-memory sample used 50 runs for each case. The first grammar report used 990 ms.

## Reference

<details>
<summary>Tools, formats, and frontend APIs</summary>

| Tools | Purpose |
| --- | --- |
| `recall`, `inspect` | Find evidence and read revision history. |
| `validate_memory`, `remember` | Check prose, then save a revision with a source. |
| `forget`, `relate`, `graph` | Hide records, link facts, and export relations. |

Writes must have a source and the current revision. Use revision `0` for a new key.

TOON is the default result text. Set the `minify` header to `uglify-json` for compact JSON or `none` for JSON with spaces and newlines.
Inputs, storage, and computation keep typed data or JSON. Only return text changes.

Authenticated frontend endpoints: `/api/recall`, `/api/graph`, `/api/graph.dot`, and `/api/status`.
Graph exports accept `?scope=…`.
The public `/mcp/skills` endpoint gives installation instructions without memory access.

</details>

<details>
<summary>Writing policy and rule coverage</summary>

Enfour applies a 20-word sentence limit and six-sentence paragraph limit.
Keep exact evidence in code or quotations, with prose about the evidence and a source.
Queries, identifiers, and model prefixes keep their original form. Harper grammar findings are advisory.
Rejected writes use no inference and change no records or indexes.
Accepted revisions record policy, validator, dictionary, and glossary versions.

| Coverage | Issue 9 rules |
| --- | --- |
| Enforced | 1.1, 4.1, 5.1, 6.3, 4.2, 6.6, 8.1, 8.4–8.7 |
| Advisory | 1.2, 1.4, 2.1, 3.1–3.6 |
| Contextual review | 1.3, 1.5–1.14, 2.2, 3.7, 4.3–4.5, 5.2–5.5, 6.1–6.2, 6.4–6.5, 7.1–7.3, 8.2–8.3, 9.1–9.4 |

The 20-word limit is below the 25-word limit for descriptions in Issue 9.
Passed checks do not show complete compliance. [Review meaning and context](https://www.asd-ste100.org/STEsoftware.html), including negation, quantities, conditions, and uncertainty.

</details>

<details>
<summary>Development and diagram source</summary>

Use targeted checks for regular changes. With a fix, include the failure and the checks that apply.
Public tests use a synthetic dictionary. Release builds must remove `test-support`.

```sh
scripts/enfour check helpers
scripts/enfour check product --language language-private/dictionary.json
scripts/enfour diagrams --setup
scripts/enfour diagrams
scripts/enfour diagrams --check
```

Diagram tools use Node.js, npm, and Chromium on the computer that creates these files only.
SVGs come from the Mermaid blocks above. Run retrieval evaluations at evaluation milestones.

</details>

[MIT license](LICENSE). TOON fixtures keep their license and provenance.
