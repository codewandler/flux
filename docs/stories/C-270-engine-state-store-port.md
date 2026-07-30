---
id: C-270
title: "Extract the engine's state store behind a port, off the direct `rusqlite` binding"
pillar: Core
status: ready
priority: 5
epic: portable-wasm-runtime
design: docs/designs/portable-wasm-runtime.md
note: "flux-flow binds rusqlite in src/state.rs only — 1 of 22 files — but that file is ~940 lines / 25 public fns / 12 rusqlite refs, and 5 match QueryReturnedNoRows structurally; follow flux-events' existing EventBackend trait, which already solved this shape"
---

# Extract the engine's state store behind a port, off the direct `rusqlite` binding

## Goal

`rusqlite` links a C library and cannot build for `wasm32-unknown-unknown`, so the engine cannot be
portable while it depends on it directly. The dependency is confined to one file, so this is an
extraction rather than a rewrite: put the engine's session/flow state behind a port whose native
implementation is the current SQLite one.

## Acceptance

- [ ] The engine's state operations are expressed as a port; the SQLite implementation moves behind it
      and stays the default natively.
- [ ] `crates/flux-flow` no longer names `rusqlite` in its own dependencies.
- [ ] A failing-first test drives the engine through a non-SQLite implementation of the port (an
      in-memory one is enough) and gets identical observable behaviour for the durability properties
      the engine actually relies on.
- [ ] The trait carries its **own** "no such row" outcome rather than surfacing a driver error type.
      Five sites match `rusqlite::Error::QueryReturnedNoRows` structurally, so a port that passes that
      variant through is portable in name only.
- [ ] The remaining transitive SQLite reach is stated: confirm no other dependency of the portable
      core links SQLite, or name the one that does. The epic cannot reach `wasm32` while any does, so
      an unexamined dependency just relocates the blocker.
- [ ] Full gate green; no behavioural change natively.

## Progress

- (not started)

## Notes

- `crates/flux-flow/src/state.rs` is the only file in that crate touching `rusqlite` (verified: 1 of
  **22** `.rs` files). `use rusqlite::Connection` at `:17`; `rusqlite::params!` at `:272`, `:316`,
  `:335`, `:451`, `:499`; and `rusqlite::Error::QueryReturnedNoRows` at `:287`, `:339`, `:517`, `:554`,
  `:606` — 12 references in all, in a ~940-line file with 25 public functions. Not a 6-line change.
- **Follow the existing precedent instead of inventing a shape:** `flux-events` already did this, with
  `trait EventBackend: Send + Sync` at `crates/flux-events/src/store/mod.rs:255` and `mod sqlite` /
  `mod postgres` behind it. flux-events therefore needs **no** work here — an earlier draft of this
  story claimed it did, which was wrong.
- `state.rs`'s own doc header (`:1-12`) states the invariant the port must preserve: values are
  content-addressed and append-only, while symbols are last-writer-wins.
- Cold-boot migration behaviour is load-bearing here and was itself a bug once (C-230, the SQLite
  cold-boot migration race). Whatever the port looks like, that property needs to survive.
