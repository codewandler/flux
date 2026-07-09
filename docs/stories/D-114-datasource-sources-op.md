---
id: D-114
title: "`sources` op — enumerate the knowledge datasources the agent can query"
pillar: Agent
status: done
priority: 20
design: docs/designs/datasource-discoverability.md
epic: datasource-discoverability
note: "the agent can `list` records inside a KNOWN source but nothing enumerates the sources themselves — no op, no CLI, no trait method (only the unexposed PostgresBackend::namespaces/scan); the agent cannot answer 'what knowledge do I have?'"
---

# `sources` op — enumerate the knowledge datasources the agent can query

## Goal
Give the agent (and any caller) one read-only op that enumerates the knowledge datasources in the
index — per source: its name/key, the entities it holds, and a record count — so `search`/`list`
stop requiring out-of-band knowledge of the `source` strings.

## Acceptance
- [x] Failing-first test: `DatasourceBackend::sources()` returns the distinct sources with entity
      sets + record counts on `MemoryBackend` (then green on `SqliteBackend`; `PostgresBackend`
      under the `postgres` feature must still compile + pass its gated test).
- [x] A sixth ungrouped read-only op beside the five in
      `crates/flux-capabilities/src/datasource/ops.rs`, registered by `register_datasource_ops` /
      `datasource_tools`; op-level test proves it reports the auto-indexed `local` source and a
      program-declared source after ingestion.
- [x] Naming decided in-story: bare `sources` (consistent with the existing bare five) vs
      `datasource.sources` (collision-safe) — record the call and why.
- [x] Website `website/docs/agent/datasources.md` retrieval-ops table goes five → six, with a line
      answering "how does the agent learn which sources exist".

## Progress
- 2026-07-09 filed from the datasource-discoverability grounding pass (see design doc).
- 2026-07-09 implemented: `DatasourceBackend::sources() -> Result<Vec<SourceSummary>>`
  (`crates/flux-capabilities/src/datasource/mod.rs`) with `SourceSummary { source, entities,
  count }`; implemented on `MemoryBackend`, `SqliteBackend`, and `PostgresBackend` (the last
  scoped correctly per-namespace — a different-namespace probe sees no sources, proven by
  `pg_sources_reports_distinct_sources_entities_and_counts`, gated on `TEST_POSTGRES_URL` under
  the `postgres` feature and green locally against a real Postgres). `SemanticBackend` delegates
  (it wraps another backend and only intercepts `search`). New ungrouped `SourcesOp` (bare
  `sources`, no args) registered by both `register_datasource_ops` and `datasource_tools` in
  `crates/flux-capabilities/src/datasource/ops.rs`; renders `source (N records; entities: e1, e2)`
  per line, or `"no sources"` on an empty index.
- Naming call: bare `sources`, matching the existing five (`search`/`get`/`list`/`relation`/
  `batch_get`) rather than a `datasource.` prefix. The design doc (datasource-discoverability.md)
  already settled this — consistency with the sibling ops outweighs the generic-name collision
  risk the ops audit flagged; verified no other registered tool anywhere in the workspace is named
  `"sources"` today (`grep -rl '"sources"' crates` hits only this op's own module).
- Updated the doc surfaces that enumerate the five retrieval ops to six: `website/docs/agent/datasources.md`
  (adds the "how does the agent learn which sources exist" paragraph the Acceptance names),
  `website/docs/language/ops.md`, `crates/flux-flow/docs/ops-reference.md`, and
  `.flux/skills/flux-flow/SKILL.md`, plus the two flux-cli comments that named the five ops.
- Gate: `cargo test --workspace` green (one transient a2a_conformance failure under full parallel
  contention was confirmed a flake — passes in isolation, and touches no file this story changed);
  `cargo clippy --workspace --all-targets -- -D warnings` and the pg-specific
  `cargo clippy -p codewandler-flux-events -p codewandler-flux-capabilities --features postgres
  --all-targets -- -D warnings` (matching CI's two invocations exactly) both clean; `cargo fmt
  --all -- --check` clean in both the root and `plugins/` workspaces.

## Notes
- Trait: `crates/flux-capabilities/src/datasource/mod.rs:62` (12 methods, no enumeration).
- The ops are ungrouped "core" ops — always surfaced; the new one should match.
- `PostgresBackend::namespaces()`/`scan()` (postgres.rs:110/139) are cross-*scope* associated fns —
  different axis (scopes vs sources within a scope); do not conflate.
