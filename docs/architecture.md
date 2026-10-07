# RAG service

[Enfour Memory](../README.md)

SQLite stores source records and revisions.

STE errors block writes. Harper findings are advisory. Missing private language data also blocks writes.
The agent reviews meaning, negation, quantities, conditions, and uncertainty. No model changes the prose.

Moka caches exact inputs. Cache inputs and versions must be the same. Recall keys also include scope, database generation, and expiry state.
Expired or changed records cannot use stale rankings. An empty cache only adds computation.

Graph exports return scoped JSON or DOT for a frontend. Tool adapters change return text only.

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
    classDef accent stroke:#d79860,stroke-width:2px,filter:none
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
    classDef accent stroke:#d79860,stroke-width:2px,filter:none
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
    classDef accent stroke:#d79860,stroke-width:2px,filter:none
    class cache,adapter accent
```
