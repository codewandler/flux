---
id: C-279
title: "The wasm32 parity proof runs nowhere but a developer's machine"
pillar: Core
status: in-progress
priority: 6
epic: portable-wasm-runtime
design: docs/designs/portable-wasm-runtime.md
note: "C-271's parity test SKIPS silently without the artifact, so `cargo test --workspace` is green on any machine lacking the target — the proof exists and nothing runs it"
---

# The wasm32 parity proof runs nowhere but a developer's machine

## Goal

C-271 landed a real proof that the portable core compiles to `wasm32` and evaluates identically to the
native engine. Nothing runs it. The parity test **skips loudly to stdout but passes** when the module is
absent, so `cargo test --workspace` is green on every machine without `wasm32-unknown-unknown` installed
— which is every CI runner. A regression in the wasm build is currently caught only by whoever happens
to run `scripts/build-portable-wasm.sh` by hand.

## Acceptance

- [ ] A CI lane installs `wasm32-unknown-unknown` and runs `scripts/build-portable-wasm.sh`, so the
      parity assertion actually executes.
- [ ] A failing-first demonstration that today's gate cannot catch the regression class: break the
      portable core (or the ABI) on a branch, show `cargo test --workspace` still green, then show the
      new lane red. That contrast is the point of the story.
- [ ] The skip path is made **loud where it matters**: `FLUX_PORTABLE_WASM_REQUIRED=1` already turns the
      skip into a failure (C-271 verified both paths) — the lane must set it, so a missing artifact fails
      the job rather than passing it quietly.
- [ ] `rustup target add wasm32-unknown-unknown` stops being a manual prerequisite for the lane.
- [ ] The lane's cost is stated: it is a second full target build, so say where it runs (every push, or
      only when the portable core changes) and why that choice is right.

## Progress

- (not started)

## Notes

- ⚠ This is the same defect class the repo has hit repeatedly this cycle: a guard that exists, looks
  green, and is not actually running. C-264's CodeQL lane pinned a build mode Rust rejects; C-259's
  release verifier had never met a real release; C-248's reference guard could not see an omission.
  Each was green until someone looked. This one is already *known* to not run — filing it so that is
  written down rather than remembered.
- C-271's implementor verified both skip paths deliberately: absent artifact plus
  `FLUX_PORTABLE_WASM_REQUIRED=1` fails 3 of 4 tests with the right message; absent without it passes
  4/4. So the mechanism the lane needs already exists and is tested — this story is wiring, not design.
- Related: [C-271](C-271-portable-core-wasm-parity.md) built the proof and named this gap in its own
  report rather than leaving it to be discovered.
