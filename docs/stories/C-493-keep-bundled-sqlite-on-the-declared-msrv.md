---
id: C-493
title: Keep bundled SQLite on the declared MSRV
pillar: Core
status: in-progress
priority: 1
note: "rusqlite 0.40 pulled libsqlite3-sys build syntax newer than Rust 1.87; repair the published 0.52 line without raising the promise"
---

# Keep bundled SQLite on the declared MSRV

## Goal
Every default Flux crate, including the bundled SQLite paths, builds on the Rust 1.87 version the
published manifests promise.

## Acceptance
- [x] A failing-first `cargo +1.87 check -p codewandler-flux-tools --locked` reproduces the
      `libsqlite3-sys` `cfg_select!` compiler failure from flux-exchange's MSRV job.
- [x] The registry-only dependency graph uses compatible database, archive, document, terminal, and
      channel dependency lines that compile on 1.87, without raising `rust-version` or adding a
      path/git patch.
- [ ] The workspace gate and codegate pass, and the 0.52 patch release is published from CI.

## Progress
- 2026-08-02: flux-exchange v0.14.0's ordinary CI failed only its MSRV job while its release gate
  passed on current stable. Reproduction on Flux v0.52.1 identified bundled libsqlite3-sys 0.38.1;
  its build script uses `cfg_select!`, unavailable to the promised compiler.
- 2026-08-02: The exact full-workspace 1.87 build then exposed previously masked drift in SQLx,
  zip, scraper/pdf-extract, ratatui's stability macro, and Slack's serialization helper. Compatible
  registry lines now pass `cargo +1.87 build --workspace --locked`; SQLx 0.8's dynamic-query API is
  used only after the same schema identifier validation as before.

## Notes
- This repairs the existing compatibility promise. Raising the MSRV to make CI green would turn a
  transitive patch regression into a consumer-facing compatibility break.
