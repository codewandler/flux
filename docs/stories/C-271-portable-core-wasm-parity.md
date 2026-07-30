---
id: C-271
title: "Prove the portable core compiles to `wasm32` and evaluates a model-free flow"
pillar: Core
status: done
priority: 5
epic: portable-wasm-runtime
design: docs/designs/portable-wasm-runtime.md
note: "the epic's first end-to-end proof. Landed for the LANGUAGE core (flux-lang: parser + reference interpreter); flux-flow's FlowEngine still cannot cross — it depends on flux-system, whose file family is not ported"
---

# Prove the portable core compiles to `wasm32` and evaluates a model-free flow

## Goal

Turn the epic from a design into a fact: build the portable evaluation core for a `wasm32` target and
have it execute a deterministic, model-free `.flux` program, producing the same result the native
engine produces for the same source.

## Acceptance

- [x] A `wasm32` build of the portable core exists and is produced by a command recorded in the repo,
      not by hand.
- [x] A parity test runs the **same** `.flux` source through the native engine and the Wasm module and
      asserts the results match. Parity is against the native engine, deliberately — a golden file
      would let both sides drift together.
- [x] The chosen Wasm flavour (`wasm32-unknown-unknown` vs `wasm32-wasip2`/Component Model) is
      recorded with its reasoning, closing that design open question.
- [x] The `tokio` question is settled in writing: what the portable core uses instead, or which
      feature set is safe on `wasm32`.
- [x] The scope limit is stated in the story: model-free only, and what that excludes.

## Scope limit — what "model-free" excludes

The portable module is instantiated with an **empty import table**, so the fragment of Flux-Lang it
can execute is exactly the fragment that needs no authority: literals, operator formulas (`expr`),
`fmt`, field access, the `obj`/`list` constructors, and the pure control-flow nodes (`when`,
`unless`, `match`, `repeat`, `each`, `seq`, `assert`, `return`).

Everything below is **out of scope for this story** and fails loudly rather than silently taking a
different path on one substrate:

- **Any `call`** — the module's host has an empty op catalog and a `dispatch` that always denies, so
  every registered operation (`read`, `bash`, `map`, `ai_*`, …) is unavailable. Asserted by
  `an_op_call_is_refused_identically_on_both_substrates`.
- **Model calls.** No provider, no credential, no inference import. (The design's "Model calls" open
  question stays open; this story simply takes the model-free branch.)
- **Anything that needs a clock** — `loop`/`timeout`/`throttle`/`debounce` and `await`. A clock is a
  host import by design; the portable core's poll loop reports *"the portable core has no reactor"*
  rather than inventing one.
- **Durability.** `MemStore`, no `DurableStore`, so `once`/`checkpoint` degrade to re-running.
- **Resource limits.** Fuel, memory ceiling and wall-clock deadline are C-273, not here. The poll
  budget in the portable core is a hang-breaker, not a resource bound.

## Progress

- **2026-07-30 — UNBLOCKED.** All three prerequisites landed on `main`: C-269 (the `System` port),
  C-270 (the engine state port) and C-274 (SQLite made an opt-out feature).
- **Landed.** `flux-lang` — the language *and its reference interpreter* — now builds for
  `wasm32-unknown-unknown`, and a parity test proves the wasm build and the native build produce
  byte-identical results (whole transcript, not just the return value) for the same `.flux` source.
  - `scripts/build-portable-wasm.sh` — the recorded build command; also runs the parity test with
    `FLUX_PORTABLE_WASM_REQUIRED=1` so it cannot pass vacuously.
  - `crates/flux-lang/examples/portable/core.rs` — the portable core, `#[path]`-shared **verbatim**
    by the wasm module and the native half of the test, so the two halves cannot drift.
  - `crates/flux-lang/examples/portable/wasm_abi.rs` — the hand-written three-function ABI.
  - `crates/flux-lang/examples/portable/parity.flux` — the trivial model-free program.
  - `crates/flux-lang/tests/wasm_parity.rs` — parity, plus the zero-imports assertion.
  - `crates/flux-lang/Cargo.toml` — the one substantive code-level change: `tokio` split by target
    family. That single edit was the *entire* blocker.
- **The single blocker was `tokio`'s `net` feature**, via `mio`: *"This wasm target is unsupported by
  mio."* Nothing in `flux-lang`'s own source needed a change — the layering map's claim that it is an
  L0 leaf held up exactly as written.
- **`SystemTime::now`/`Instant::now` were not a compile blocker** for the language core: `flux-lang`
  touches them only on the throttle and time-bounded-loop paths, and `std` compiles them for
  `wasm32-unknown-unknown` (they panic at runtime). The model-free fragment never reaches them.
  The `now_ms()` seams C-270 left in the *state facades* are still owed a host clock — that is
  C-272's, once the engine itself can cross.

## Not yet done — scoped for follow-up

- **`flux-flow`'s `FlowEngine` cannot cross.** The portable core proved here is the **language
  core** (`flux-lang`: parse + the reference interpreter), which is what the design's Shape box
  names. The *engine* depends on `flux-system` (guarded IO) and `flux-runtime`; a
  `cargo build -p codewandler-flux-system --lib --target wasm32-unknown-unknown` fails on `mio`
  before it even reaches the unported file family. C-269 landed the `System` seam, but ~120 call
  sites still take the concrete type. Porting it is a story of its own, not a rider on this one.
- **CI does not build the wasm target.** `rustup target add wasm32-unknown-unknown` is a
  prerequisite, so the parity test *skips* (loudly) when the artifact is absent and the build script
  is the thing that proves it. Wiring a CI job is a follow-up.
- **No resource bound.** C-273.

## Finding: this story needed none of its three prerequisites

C-271 was blocked on C-269, C-270 and C-274. **As scoped, it required none of them.** The work was
done on a branch based at `a0e431f9`, which predates all three merges (C-269 at `14d6673c`, C-274 at
`10f1804f`), and the wasm32 build and the parity proof were already green there. `main` was merged in
afterwards and changed nothing about the result.

That is not an argument that the three were unnecessary — it is a correction to the epic's dependency
graph. **They are prerequisites for the *engine* crossing, not for the *language core* crossing**, and
this story turned out to be the second of those:

- `flux-lang` reaches no `rusqlite` at all, so C-274's SQLite work never applied to it.
- `flux-lang` reaches no `flux-system`, so C-269's `System` port never applied to it either.
- `flux-lang` has its own value store (`MemStore`), not the engine's, so C-270's `FlowStateBackend`
  never applied to it.

The one real blocker was orthogonal to all three: `tokio`'s `net` feature pulling `mio`. The
prerequisites become load-bearing at the *next* step — a `flux-flow` `FlowEngine` that crosses —
which is the follow-up scoped above.

## Traps for a later editor

- **`flux_alloc` must return an allocation whose capacity equals its length.** It uses a boxed slice
  (`vec![0u8; len].into_boxed_slice()`) precisely so `flux_dealloc` can rebuild it from `(ptr, len)`
  alone. A `Vec::with_capacity` there may over-allocate, and freeing it as though capacity == len is
  **undefined behaviour**. This is the only `unsafe` in the story's diff and the only place it is
  subtle. The `(ptr << 32) | len` result packing likewise assumes a 32-bit address space — correct
  for wasm32, silently wrong on wasm64.
- **The parity test skips when the artifact is absent**, so `cargo test --workspace` is green on a
  machine without the `wasm32-unknown-unknown` target — it prints `SKIP: …` and passes. That is
  deliberate (the target is a manual `rustup target add`), but it means
  **`scripts/build-portable-wasm.sh` is the load-bearing run** until a CI job wires it: the script
  sets `FLUX_PORTABLE_WASM_REQUIRED=1`, which turns the skip into a failure. A regression in the
  wasm build will not red the ordinary gate.

## Notes

- Blocked, not backlog: the dependency is structural, not a priority call.
- `flux-lang` is L0 with no IO and should need no changes to be portable; if it does, that is a
  finding worth recording, since the layering map asserts otherwise.
  *(Outcome: it needed **no source change** — only a target-gated dependency. The map was right.)*
- Keep the first program genuinely trivial. The deliverable is the substrate, and a large example
  makes a failure ambiguous between the port and the program.
