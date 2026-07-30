---
id: C-274
title: "Make flux-events' SQLite dependency optional — the actual `wasm32` prerequisite"
pillar: Core
status: in-progress
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

- [x] `flux-events`' `rusqlite` dependency and its `mod sqlite` become optional behind a feature that
      is **on by default**, so no existing consumer changes behaviour or gains a build step.
- [x] A failing-first demonstration that the gap is real: with the feature off,
      `cargo tree -p codewandler-flux-flow -i rusqlite` reports **no** path (today it reports two —
      flux-flow directly and via flux-events). Record the before/after output; that command is the
      acceptance evidence, not a test name.
- [x] `flux-flow` then drops its own direct `rusqlite` line too — C-270 left it deliberately because
      removing it alone gated `FlowStore::in_memory` behind a feature for no gain. With this story
      landed that objection is gone, so finish it here.
- [x] The default build is byte-for-byte behaviourally unchanged: the SQLite backend is still the
      default `EventStore`, and the existing durability, concurrency and cold-boot-migration tests all
      still run against it.
- [x] With the feature off, the crate still **compiles** and `EventBackend` still has at least one
      usable implementation, or the story states plainly what an embedder must supply instead. A
      feature-off build that cannot construct any store is not a portability win.
- [x] The remaining SQLite reach in the portable core is enumerated: `rusqlite` is also named by
      `flux-cli`, `flux-tools` and `flux-capabilities` (per C-270). Say which of those the portable
      core actually needs, so C-271 inherits a list rather than a surprise.
- [x] Full gate green in both workspaces, plus `scripts/check-crate-versions.sh` — `flux-events` is
      not on the protocol line, but a feature change to a published crate is a version decision.

## Progress

Landed on `impl/C-274`. The driver is a feature in both crates; `cargo build -p codewandler-flux-flow
--no-default-features` compiles **204 units with zero SQLite units** (`libsqlite3-sys` and `rusqlite`
both absent), and the whole 230-test flux-flow lib suite passes in that configuration.

**The trap worth knowing about.** `flux-flow = { workspace = true, default-features = false }` in a
member manifest is **silently ignored** by cargo 1.97 for a workspace-inherited dependency — measured:
`cargo metadata` still reported `uses_default_features: true`, and the feature-off build still compiled
`rusqlite`. Gating only flux-events' own manifest therefore looks right and changes nothing. The fix is
`default-features = false` on the **`[workspace.dependencies]`** entry, with each native member opting
back in (`features = ["sqlite"]`, 8 crates) and flux-flow re-enabling it through its own default-on
`sqlite` feature instead. `flux-plugin` was already declared that way in the root manifest, so the
shape has precedent.

**What feature-off gives you** (acceptance item 5): a real backend, not a stub. `EventBackend` is
crate-private by design, so an embedder cannot supply one from outside — a compiling-but-unusable store
would have been the honest reading of "portable". `store/ephemeral.rs` is a driver-free
`EventBackend` (pure `std` collections behind one `Mutex`), always compiled, and run against **all 44
shared conformance bodies** in the default build plus three of its own for the guarantees it has to
reproduce by hand (copy-session atomicity, no id reuse after a prune). `EventStore::in_memory` and
`FlowStore::in_memory` resolve to it feature-off, so no consumer needs a `cfg`. What it gives up is
durability, which is why `EventStore::open` / `FlowStore::open` stay behind the feature rather than
quietly returning a store that forgets everything.

**Remaining SQLite reach in the portable core: none** (acceptance item 6). The other three namers are
all outside it — `cargo tree -p codewandler-flux-flow --no-default-features -e normal` reaches none of
them:
- `flux-tools` (`src/extra.rs`, the `sql_query` op) — a *tool*, not storage, and only a
  **dev-dependency** of flux-flow, so it is in no build of the core. It is why the evidence command
  needs `-e normal`: as a dev-dep it unifies flux-events' features back on in `cargo tree`'s default view.
- `flux-capabilities` (`datasource/sqlite.rs`, `datasource/vector.rs`) — L5, reached only by
  flux-sdk / flux-cli / flux-lsp.
- `flux-cli` (`doctor.rs`, `usage.rs`) — the L6 surface; it depends on flux-flow, never the reverse.

So C-271 inherits no SQLite work. The blockers it still owns are the ones the design doc names:
`flux-system::System` is a concrete struct, `tokio`, and the `now_ms()` clock in both state facades.

**Version decision** (acceptance item 7): `scripts/check-crate-versions.sh` passes and requires no bump
— it guards only the independently-versioned protocol line, and both crates inherit
`[workspace.package].version`. The change is additive for a default-features consumer (new default-on
feature, new `EventStore::ephemeral()`); the only surface that moves is `EventStore::open` /
`FlowStore::open` / `SqliteState` becoming feature-conditional, which cannot affect anyone today because
the feature did not exist to switch off. Workspace-version crates move at the release cut.

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
