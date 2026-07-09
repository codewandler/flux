---
id: D-114
title: "`sources` op — enumerate the knowledge datasources the agent can query"
pillar: Agent
status: ready
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
- [ ] Failing-first test: `DatasourceBackend::sources()` returns the distinct sources with entity
      sets + record counts on `MemoryBackend` (then green on `SqliteBackend`; `PostgresBackend`
      under the `postgres` feature must still compile + pass its gated test).
- [ ] A sixth ungrouped read-only op beside the five in
      `crates/flux-capabilities/src/datasource/ops.rs`, registered by `register_datasource_ops` /
      `datasource_tools`; op-level test proves it reports the auto-indexed `local` source and a
      program-declared source after ingestion.
- [ ] Naming decided in-story: bare `sources` (consistent with the existing bare five) vs
      `datasource.sources` (collision-safe) — record the call and why.
- [ ] Website `website/docs/agent/datasources.md` retrieval-ops table goes five → six, with a line
      answering "how does the agent learn which sources exist".

## Progress
- 2026-07-09 filed from the datasource-discoverability grounding pass (see design doc).

## Notes
- Trait: `crates/flux-capabilities/src/datasource/mod.rs:62` (12 methods, no enumeration).
- The ops are ungrouped "core" ops — always surfaced; the new one should match.
- `PostgresBackend::namespaces()`/`scan()` (postgres.rs:110/139) are cross-*scope* associated fns —
  different axis (scopes vs sources within a scope); do not conflate.
