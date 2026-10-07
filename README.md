# Enfour Memory

Local RAG for coding agents, with keyword search and semantic search in one Rust service.
Keep facts, exact evidence, and revision history across sessions.

[Connect](docs/connect.md) · [Self-host](docs/self-host.md) · [Reference](docs/reference.md) · [Measurements](docs/benchmarks.md) · [Development](docs/development.md) · [Mermaid source](docs/architecture.md)

## Architecture

<a href="assets/diagrams/rag.light.generated.svg"><picture><source media="(prefers-color-scheme: dark)" srcset="assets/diagrams/rag.dark.generated.svg"><img src="assets/diagrams/rag.light.generated.svg" width="880" alt="RAG service: MCP, writing checks, local models, SQLite, caches, adapters, and graph exports."></picture></a>

## Save

<a href="assets/diagrams/write.light.generated.svg"><picture><source media="(prefers-color-scheme: dark)" srcset="assets/diagrams/write.dark.generated.svg"><img src="assets/diagrams/write.light.generated.svg" width="880" alt="Write order: exact evidence, STE checks, Harper advice, chunks, embeddings, and SQLite commit."></picture></a>

## Recall

<a href="assets/diagrams/recall.light.generated.svg"><picture><source media="(prefers-color-scheme: dark)" srcset="assets/diagrams/recall.dark.generated.svg"><img src="assets/diagrams/recall.light.generated.svg" width="880" alt="Recall order: cache, keyword and vector search, fusion, reranking, expiry checks, and result adapter."></picture></a>
