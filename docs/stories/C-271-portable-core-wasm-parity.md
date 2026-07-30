---
id: C-271
title: "Prove the portable core compiles to `wasm32` and evaluates a model-free flow"
pillar: Core
status: in-progress
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

## Notes

- Blocked, not backlog: the dependency is structural, not a priority call.
- `flux-lang` is L0 with no IO and should need no changes to be portable; if it does, that is a
  finding worth recording, since the layering map asserts otherwise.
  *(Outcome: it needed **no source change** — only a target-gated dependency. The map was right.)*
- Keep the first program genuinely trivial. The deliverable is the substrate, and a large example
  makes a failure ambiguous between the port and the program.
