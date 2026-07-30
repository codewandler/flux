---
id: C-279
title: "The wasm32 parity proof runs nowhere but a developer's machine"
pillar: Core
status: done
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

- [x] A CI lane installs `wasm32-unknown-unknown` and runs `scripts/build-portable-wasm.sh`, so the
      parity assertion actually executes.
      → `.github/workflows/ci.yml`, the `portable-wasm` job. **Authored, not observed** — see
      Verification below: no GitHub runner has executed it.
- [x] A failing-first demonstration that today's gate cannot catch the regression class: break the
      portable core (or the ABI) on a branch, show `cargo test --workspace` still green, then show the
      new lane red. That contrast is the point of the story.
      → Done at the merge base `80bb31fa`; both outputs quoted below.
- [x] The skip path is made **loud where it matters**: `FLUX_PORTABLE_WASM_REQUIRED=1` already turns the
      skip into a failure (C-271 verified both paths) — the lane must set it, so a missing artifact fails
      the job rather than passing it quietly.
      → job-level `env:` on `portable-wasm`, and verified: with the artifact absent under that flag,
      3 of 4 parity tests fail.
- [x] `rustup target add wasm32-unknown-unknown` stops being a manual prerequisite for the lane.
      → `targets: wasm32-unknown-unknown` on the pinned `dtolnay/rust-toolchain` step.
- [x] The lane's cost is stated: it is a second full target build, so say where it runs (every push, or
      only when the portable core changes) and why that choice is right.
      → argued in the job's comment block and summarised below. Answer: **every push and every PR,
      no `paths:` filter.**

## Progress

**The diff.** `.github/workflows/ci.yml` only — one new `portable-wasm` job, modelled on
`sandbox-backend` rather than on a new shape. No Rust, no script change: `scripts/build-portable-wasm.sh`
already did everything the lane needs, which is what made this story wiring rather than design.

**The failing-first demonstration (acceptance item 2).** At `80bb31fa`, with a deliberate off-by-one
in `flux_eval`'s packed return value (`(out_ptr << 32) | (out_len - 1)`, truncating the JSON by one
byte) — a defect invisible to any host build, because `wasm_abi.rs` is `#![cfg(target_family = "wasm")]`
and is not even type-checked natively:

- `cargo test --workspace` → **exit 0. Green.** That is the defect the story names.
- `./scripts/build-portable-wasm.sh` → **FAILED. 2 passed; 2 failed**, on truncated JSON:
  `left: "{\"error\":\"execute: unknown op \`read\`\",\"ok\":false"` against
  `right: "…,\"ok\":false}"`.

The fixture was committed by the coordinator during a session-limit recovery (`1ae10588`) and reverted
in `db03eccf`; `git diff 80bb31fa -- crates/flux-lang/examples/portable/wasm_abi.rs` is empty on this
branch.

**Where the lane runs, and why: every push and every PR, no `paths:` filter.** The full argument is in
the job comment. The short form is that the regression class is *feature unification*, not file
locality — C-271's real blocker was tokio's `net` feature pulling `mio` — so a filter on
`crates/flux-lang/**` would skip exactly the PR that broke it, and a filter wide enough to be correct
would be a hand-maintained second copy of the dependency graph with nothing checking it. That is the
same "guard that looks green because it isn't running" defect this story exists to close, and it would
be the fourth instance this cycle after C-248, C-259 and C-264. The cost does not justify the risk: the
job is parallel and far off the critical path that `check` owns.

**Measured cost**, on an 8-core dev box with a warm cargo registry, from a clean wasm target dir:
~15s wall / ~71s CPU for the wasm build, artifact 1,880,833 bytes, `target/wasm32-unknown-unknown`
~142 MB. A 2-core runner with a cold cache will be several times that, dominated by building
flux-lang's dependency closure twice (wasm32 release + host debug test binary). Still a fraction of
`check`.

**Verification — what was and was not proved.**

- **No GitHub runner has executed this lane.** The implementor cannot trigger a CI run. Everything
  below was produced locally, in this worktree, against its own `./target` (no shared
  `CARGO_TARGET_DIR`). The lane's *command sequence* was reproduced; the lane itself was not observed.
- The lane's command, run exactly as the job runs it (`FLUX_PORTABLE_WASM_REQUIRED=1
  ./scripts/build-portable-wasm.sh`) from a clean wasm target: **4 passed; 0 failed**.
- The vacuity check: `FLUX_PORTABLE_WASM_REQUIRED=1 FLUX_PORTABLE_WASM=/nonexistent/… cargo test -p
  codewandler-flux-lang --test wasm_parity` → **1 passed; 3 failed**, each with
  "the portable wasm module is not built at …". So the lane cannot report success without having
  actually executed the module.
- Not verified: that `dtolnay/rust-toolchain`'s `targets:` input installs the target on a real runner,
  and that the runner's disk and cache behave. If `targets:` silently did nothing, the build script
  exits 2 with "the wasm32 target is not installed" and the lane fails loudly rather than skipping —
  that path is by construction, not observation.

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
