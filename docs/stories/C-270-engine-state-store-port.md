---
id: C-270
title: "Extract the engine's state store behind a port, off the direct `rusqlite` binding"
pillar: Core
status: in-progress
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

- [x] The engine's state operations are expressed as a port; the SQLite implementation moves behind it
      and stays the default natively.
- [ ] `crates/flux-flow` no longer names `rusqlite` in its own dependencies.
- [x] A failing-first test drives the engine through a non-SQLite implementation of the port (an
      in-memory one is enough) and gets identical observable behaviour for the durability properties
      the engine actually relies on.
- [x] The trait carries its **own** "no such row" outcome rather than surfacing a driver error type.
      Five sites match `rusqlite::Error::QueryReturnedNoRows` structurally, so a port that passes that
      variant through is portable in name only.
- [x] The remaining transitive SQLite reach is stated: confirm no other dependency of the portable
      core links SQLite, or name the one that does. The epic cannot reach `wasm32` while any does, so
      an unexamined dependency just relocates the blocker.
- [x] Full gate green; no behavioural change natively.

## Progress

The port landed; one acceptance item did not, and the measurement behind it changes what that item is
worth. Details below so a resuming agent does not re-derive them.

**Done.** `src/state.rs` became `src/state/`:

- `state/port.rs` — `trait FlowStateBackend: Send + Sync` (14 methods, object-safe), plus
  `Lookup<T>` (`Found` / `NoSuchRow`), `SymbolBinding<'_>`, `StoredSymbol`, `Suspension`.
- `state/sqlite.rs` — `SqliteState`, the pre-existing implementation moved verbatim, including the
  `CREATE TABLE IF NOT EXISTS` + error-ignoring `ALTER TABLE` cold-boot migration and both
  `flux-allow-direct-io` allowances. The **only** file in the crate whose *code* names `rusqlite`.
- `state/memory.rs` — `MemoryState`, a driver-free backend over `BTreeMap`s behind one lock.
- `state/mod.rs` — `FlowStore` as a facade over `Arc<dyn FlowStateBackend>`. All 25 public method
  signatures and all three existing constructors are unchanged, so the 123 external call sites needed
  no edits; `FlowStore::with_backend` is the one new entry point.

Absence is the port's own outcome: `Lookup::NoSuchRow`. The `QueryReturnedNoRows` matches dropped from
five to four only because `load_suspension` and `take_suspension` now share one
`SqliteState::read_suspension` projection instead of duplicating the query — no site was lost.

**Not done: `crates/flux-flow` still names `rusqlite` in `[dependencies]`.** Two reasons, and the
second matters more than the first:

1. `crates/flux-flow/Cargo.toml`'s dependency list is coordinator-fenced, as is `Cargo.lock`.
2. **It would buy nothing on its own.** `cargo tree -p codewandler-flux-flow -i rusqlite` shows *two*
   paths in, not one:

   ```
   rusqlite v0.40.1
   ├── codewandler-flux-events v0.38.0
   │   └── codewandler-flux-flow v0.38.0
   └── codewandler-flux-flow v0.38.0
   ```

   `flux-events` names `rusqlite` **non-optionally** (`crates/flux-events/Cargo.toml`), and flux-flow
   cannot drop flux-events — `Arc<EventStore>` is in `FlowStore`'s public signature. So flux-flow will
   not build for `wasm32` whether or not its own line goes. The design doc's "flux-events does NOT
   need this work" is right about the *seam* (`EventBackend` exists) and wrong about the *dependency*:
   the seam is there, but the driver is still linked unconditionally.

   Dropping flux-flow's own line also is not a one-liner: `SqliteState` and the three SQLite
   constructors would have to go behind a `sqlite` feature, which makes `FlowStore::in_memory` and
   friends conditionally absent from a published crate's API — a reviewed decision, not an
   implementor's, especially for zero capability gain.

**Recommended next step:** a sibling story that gives `flux-events` an optional `rusqlite` (it already
has the trait), then drop both direct dependencies together. That is the change that actually moves
C-271 (`wasm32` compile proof) forward; doing flux-flow's half alone is cosmetic.

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
