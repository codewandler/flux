---
id: D-142
title: SDK storage injection + the resumable Session handle
pillar: Agent
status: done
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 1 foundation — unlocks resume, suspensions, flow-driven voice, time machine"
---

# SDK storage injection + the resumable Session handle

## Goal
Give `flux_sdk::Client` persistent, injectable storage and a cheap resumable `Session` handle, so
embedded agents survive process restarts and every later front door (suspensions, flow-driven
voice, projections, time machine) has a seam to hang off.

## Acceptance
- [ ] `Storage::{in_memory, dir, custom}` exists; `ClientBuilder::storage(Storage)` defaults to
      in-memory with today's exact behavior (existing `lib.rs` tests stay green untouched).
- [ ] `Storage::dir(d)` opens `d/events.db` + `d/flow.db` (the CLI convention) — failing-first
      test: build with `Storage::dir`, run a turn, drop the client; rebuild over the same dir,
      `open_session(id)`, `history()` shows the prior turn.
- [ ] `Client::{create_session, open_session, latest_session}` return `Session`;
      `Session::{id, send}` work; `open_session` on an unknown id errors.
- [ ] `Client::run`/`session_id` stay source-compatible over a lazily created default session.
- [ ] `Client::{event_store, engine}` escape hatches exist and are documented as such.
- [ ] A turn-guard serializes concurrent `send`s on two `Session`s of one `Client` (test with two
      tasks + a slow mock provider; assert no interleaved turn).

## Progress
- 2026-07-11: implemented — `src/storage.rs` (Storage in_memory/dir/custom + resolve),
  `src/session.rs` (Session {engine, id, turn_guard} + send/history; Collector moved here),
  `Client` holds `Arc<FlowEngine>` + model + turn guard; create/open/latest_session +
  default_session + event_store/engine escape hatches; `tokio` promoted to a real dep (sync
  Mutex). Default session stays EAGER (created at build) so `session_id()` remains infallible —
  documented on `latest_session`. Tests: dir persistence + resume-by-id, unknown-id error,
  turn-guard serialization (interval non-overlap), all pre-existing tests green (26/26).

## Notes
- `crates/flux-sdk/src/lib.rs` (builder/build rework), new `src/storage.rs` + `src/session.rs`.
- In-memory path must use `FlowStore::in_memory_with_events(events)` to keep events wired.
- `FlowEngine.executor/events/flow` are pub (`crates/flux-flow/src/engine.rs:48-51`);
  `EventStore::latest_session` exists (`crates/flux-events/src/store/mod.rs:234`).
- ⚠ Concurrent-session WIP (D-141 docs pass) holds uncommitted doc-comment hunks in
  `lib.rs`/`flow.rs` — do not touch the module-doc region; stage commits surgically.
