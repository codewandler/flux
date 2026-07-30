---
id: C-274
title: "Make flux-events' SQLite dependency optional — the actual `wasm32` prerequisite"
pillar: Core
status: ready
priority: 5
epic: portable-wasm-runtime
design: docs/designs/portable-wasm-runtime.md
note: "found by C-270: rusqlite reaches flux-flow by TWO paths, and flux-events names it non-optionally — so dropping flux-flow's own line buys nothing. This one drops both at once and is what unblocks C-271"
---

# Make flux-events' SQLite dependency optional — the actual `wasm32` prerequisite

## Goal

C-270 put the engine's state store behind a port and then discovered its own last acceptance item was
not worth doing alone: `rusqlite` reaches `flux-flow` by **two** paths, and the second one is
`flux-events`, which names the driver non-optionally. Since `Arc<EventStore>` sits in `FlowStore`'s
public signature, flux-flow cannot drop flux-events — so the engine links SQLite whether or not
flux-flow names it directly. Make flux-events' SQLite backend optional so both direct dependencies can
go at once, which is the real prerequisite for a `wasm32` build.

## Acceptance

- [ ] `flux-events`' `rusqlite` dependency and its `mod sqlite` become optional behind a feature that
      is **on by default**, so no existing consumer changes behaviour or gains a build step.
- [ ] A failing-first demonstration that the gap is real: with the feature off,
      `cargo tree -p codewandler-flux-flow -i rusqlite` reports **no** path (today it reports two —
      flux-flow directly and via flux-events). Record the before/after output; that command is the
      acceptance evidence, not a test name.
- [ ] `flux-flow` then drops its own direct `rusqlite` line too — C-270 left it deliberately because
      removing it alone gated `FlowStore::in_memory` behind a feature for no gain. With this story
      landed that objection is gone, so finish it here.
- [ ] The default build is byte-for-byte behaviourally unchanged: the SQLite backend is still the
      default `EventStore`, and the existing durability, concurrency and cold-boot-migration tests all
      still run against it.
- [ ] With the feature off, the crate still **compiles** and `EventBackend` still has at least one
      usable implementation, or the story states plainly what an embedder must supply instead. A
      feature-off build that cannot construct any store is not a portability win.
- [ ] The remaining SQLite reach in the portable core is enumerated: `rusqlite` is also named by
      `flux-cli`, `flux-tools` and `flux-capabilities` (per C-270). Say which of those the portable
      core actually needs, so C-271 inherits a list rather than a surprise.
- [ ] Full gate green in both workspaces, plus `scripts/check-crate-versions.sh` — `flux-events` is
      not on the protocol line, but a feature change to a published crate is a version decision.

## Progress

- (not started)

## Notes

- Evidence, measured by C-270's implementor and re-verified by the coordinator:
  ```
  $ cargo tree -p codewandler-flux-flow -i rusqlite
  rusqlite v0.40.1
  ├── codewandler-flux-events → codewandler-flux-flow
  ├── codewandler-flux-flow
  └── codewandler-flux-tools [dev] → codewandler-flux-flow
  ```
  and `crates/flux-events/Cargo.toml:31` — `rusqlite.workspace = true`, not optional.
- The seam already exists and does **not** need inventing: `trait EventBackend: Send + Sync` at
  `crates/flux-events/src/store/mod.rs:255`, with `mod sqlite` and `mod postgres` behind it. This story
  is about the *dependency*, not the abstraction — a distinction the epic's design doc originally got
  wrong, which is why it is called out here.
- ⚠ `flux-pg` is pulled in only when the `postgres` feature is on (`Cargo.toml:12`), so there is
  precedent in this very manifest for an optional backend. Follow that shape.
- Blocks [C-271](C-271-portable-core-wasm-parity.md). Nothing in the epic reaches `wasm32` until this
  lands, so it outranks C-271 regardless of priority numbers.
