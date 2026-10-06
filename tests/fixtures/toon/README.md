# TOON encoder conformance fixtures

Copied unmodified from https://github.com/toon-format/spec, release v4.2.1,
commit `43d8c2933f64f07101d7e9369d3c9c332b944efe`, `tests/fixtures/encode/`.
Upstream MIT license is included. These exercise the encoder class (spec 4.2),
including comma/tab/pipe delimiters and configurable indentation. No decoder is
implemented or claimed by Enfour's return-only adapter.

Run `scripts/cargo-local test --locked --offline --test output`.
