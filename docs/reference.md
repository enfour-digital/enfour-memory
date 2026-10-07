# Reference

[Enfour Memory](../README.md)

## Tools, formats, and frontend APIs

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

## Writing policy and rule coverage

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

## Branches

Use `main` for RAG. Agent memory development is on `experimental/agent-memory`. Use a different state directory for that branch.

[MIT license](../LICENSE). TOON fixtures keep their license and provenance.
