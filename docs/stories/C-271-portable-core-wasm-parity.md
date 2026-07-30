---
id: C-271
title: "Prove the portable core compiles to `wasm32` and evaluates a model-free flow"
pillar: Core
status: ready
priority: 5
epic: portable-wasm-runtime
design: docs/designs/portable-wasm-runtime.md
note: "the epic's first end-to-end proof; blocked on C-269 + C-274 (C-270 landed and found C-274 is the real wasm32 prerequisite). Parity against the NATIVE engine on the same .flux, not against a golden file"
---

# Prove the portable core compiles to `wasm32` and evaluates a model-free flow

## Goal

Turn the epic from a design into a fact: build the portable evaluation core for a `wasm32` target and
have it execute a deterministic, model-free `.flux` program, producing the same result the native
engine produces for the same source.

## Acceptance

- [ ] A `wasm32` build of the portable core exists and is produced by a command recorded in the repo,
      not by hand.
- [ ] A parity test runs the **same** `.flux` source through the native engine and the Wasm module and
      asserts the results match. Parity is against the native engine, deliberately — a golden file
      would let both sides drift together.
- [ ] The chosen Wasm flavour (`wasm32-unknown-unknown` vs `wasm32-wasip2`/Component Model) is
      recorded with its reasoning, closing that design open question.
- [ ] The `tokio` question is settled in writing: what the portable core uses instead, or which
      feature set is safe on `wasm32`.
- [ ] The scope limit is stated in the story: model-free only, and what that excludes.

## Progress

- **2026-07-30 — UNBLOCKED.** All three prerequisites are on `main`: C-269 (the `System` port),
  C-270 (the engine state port) and C-274 (SQLite made an opt-out feature, verified by
  `cargo tree -p codewandler-flux-flow --no-default-features -e normal -i rusqlite` reporting nothing).
  C-274's implementor also enumerated the remaining SQLite reach for you: **none** in the portable core —
  `flux-tools` is a dev-dependency only, `flux-capabilities` is L5, `flux-cli` is L6. So this story
  inherits no SQLite work. What it still owns, per the design and C-270's findings: `flux-system`'s
  concrete `System` is now a port but the file family is unported, `tokio`, and `now_ms()` calling
  `SystemTime::now` in both state facades — unavailable on `wasm32-unknown-unknown`, so the clock is a
  host import.

- (superseded) (blocked on C-269, C-270 and **C-274** — C-270 has landed, and in landing it found that C-274 is the
  real `wasm32` prerequisite: `rusqlite` reaches the engine via flux-events non-optionally, so nothing
  here can build until that dependency is made optional. Also inherited from C-270: `now_ms()` calls
  `SystemTime::now`, which does not exist on `wasm32-unknown-unknown` — the clock is a host import.)

## Notes

- Blocked, not backlog: the dependency is structural, not a priority call.
- `flux-lang` is L0 with no IO and should need no changes to be portable; if it does, that is a
  finding worth recording, since the layering map asserts otherwise.
- Keep the first program genuinely trivial. The deliverable is the substrate, and a large example
  makes a failure ambiguous between the port and the program.
