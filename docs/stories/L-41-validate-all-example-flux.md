---
id: L-41
title: CI-validate every checked-in examples/*.flux (JSON + native text)
pillar: Language
status: done
design:
epic:
note: only 4/12 root examples are pinned by flows_validate.rs; the drift class (value-template drift) broke checked-in flows twice before; loop-poc.flux is the one example with zero references anywhere
---

# CI-validate every checked-in examples/*.flux (JSON + native text)

## Goal
`crates/flux-eval/tests/flows_validate.rs` pins 4 of the 12 root `examples/*.flux` files to the
live op registry (JSON form only); strict_review/multi-perspective/cognition-research are covered
elsewhere, but ~6 examples — including `god-review.flux` and `channels-app.flux` — have no CI
guard at all. Value-template drift has broken checked-in flows twice (I-05 era eval-infra
defects). Sweep them all: every file under `examples/` must parse (serde for JSON, `flux_lang`
parse for native text) and pass the same `lower` gate `flux flow run` applies; program-form files
must parse as a Program.

## Acceptance
- [x] A test (extend `flows_validate.rs` or a sibling) that *enumerates the directory* — a newly
      added example is guarded by default, not by remembering to add it to a list.
- [x] Native-text examples go through `flux_lang` parse → `lower`; JSON examples through the
      existing serde → `lower` path; `.flux` Program files (e.g. channels-app) through the program
      parser. Ops resolved against the same registry build flows_validate uses today.
- [x] Failing-first: the test fails on the current tree if any of the 6 unguarded examples has
      drifted (or, if all currently pass, failing-first is demonstrated with a planted drift in a
      scratch commit noted in Progress).
- [x] `examples/loop-poc.flux` fate decided (delete, or reference it from a doc) — it is the only
      example with zero references in README/docs/AGENTS/crates.

## Progress
- 2026-07-07 — implemented. New sibling test
  `crates/flux-eval/tests/examples_validate.rs`: `every_example_validates_against_its_form_appropriate_gate`
  enumerates `examples/*.flux` via `std::fs::read_dir` (no hardcoded file list) and sniffs each
  file's form, routing it through the strictest gate available:
  - **JSON `DraftAst`** (6 files: cognition-research, eval-smoke, eval-synthetic, improve-multi,
    improve-synthetic, improve-tbench) — serde → the same `lower` gate `flux flow run` applies.
  - **Native-text bare flow** (4 files: god-review, multi-perspective, strict_review,
    advanced-code-review) — `flux_flow::program::Module::parse_str` → `lower`, against
    `register_builtins` + `register_eval_ops` + `task` + (new) `flux-cognition`'s `CognitionPack`
    (`ai.extract`/`ai.rank`/`ai.judge`/`ai.reason`/`synth`/`ai.rewrite`, wired to a key-free
    `NullProvider` — no network/model call, `lower` only reads declared signatures). Added
    `flux-cognition` as an L3→L3 dev-dependency of `flux-eval` for this.
  - **Native-text Program** (1 file: channels-app) — `Module::parse_str` → `Module::Program` + a
    registry-free structural check (every `trigger.run` resolves to a declared journey/flow). The
    journeys' orchestration ops (`send`/`ask`/`emit`/`spawn`) are constructed by `flux-app` (L6)
    against a live `Bus`/`JourneyHost` — genuinely unreachable from flux-eval (L3) without a
    layering violation, so parse + structural is the honest gate here (`flux-app`'s own tests cover
    the runtime path).
  - One native-text flow, `advanced-code-review.flux`, calls `slack.message.send` (an
    out-of-process `flux-plugin-slack` op, same layering class as the Program orchestration ops) —
    pinned parse-only via the documented `FLOW_PARSE_ONLY` exception list.
  - Gate: `cargo test -p flux-eval` (44 unit + 1 emission_ab + 1 examples_validate + 1
    flows_validate, all green), `cargo clippy -p flux-eval --all-targets -- -D warnings` clean,
    `cargo fmt -p flux-eval -- --check` clean, `cargo test -p flux-codegate` green (layering intact).

  **Failing-first, genuinely (no synthetic drift needed — the sweep found real drift on first
  run):**
  1. `advanced-code-review.flux` didn't even parse: `unknown top-level declaration: "-- advanced-
     code-review.flux --"` — its original text was written in a brace-delimited / `call("op",
     {...})` / `@param` / adjacent-string-literal-concatenation dialect that was **never the real
     grammar** (its own commit message `00d4b41` says "examples are not part of the workspace gate
     and this one was not executed here"; `docs/stories/L-37-multi-perspective-example.md` names
     this exact file's style as "aspirational, not parsed"). Fixed the example: rewrote it in the
     real indentation grammar (`docs/syntax.md`), using the documented `@json` escape for the four
     Tier-2 nodes with no native-text spelling it needs (`confirm`, `saga`+`once`+`budget`,
     `verify`) — every interpolated string is bound to a `$symbol` via `fmt(...)` first, since a
     call/`@json` argument position only accepts `lit`/`var`/`obj`/`list` (verified against
     `crates/flux-lang/src/analyze.rs`'s `check_node`). One deliberate simplification vs. the
     original aspirational sketch: `once`'s idempotency `label` is a compile-time-constant `String`
     field (not a `Node`), so the original's `"...{pr_branch}"`-templated label was never expressible
     either — the rewrite uses a fixed label and drops the per-branch `$pr_branch` from the nested
     `observe` call inside `confirm` (kept literal-only to avoid a deep hand-written `Obj` nest).
  2. `improve-multi.flux` failed `lower`: `op improve_log is missing required parameter record` at
     all 3 call sites — the exact "value-template drift" class the `flows_validate.rs` module doc
     already names (`improve_log` grew a required `record` wrapper; the already-CI-pinned
     `improve-synthetic.flux` has it, `improve-multi.flux` never got the same fix since nothing
     guarded it). Fixed the example: wrapped each call's fields under `"record": {...}`, matching
     `improve-synthetic.flux`'s shape exactly.
  3. `loop-poc.flux` failed `lower`: `unknown operation: plan` / `run_plan` — traced these ops and
     found they are **not** ordinary registry `Tool`s at all (no `impl Tool for` exists anywhere for
     either name); they're wired directly into `flux-flow`'s `LoopHost`/interpreter dispatch for a
     live agent-loop session (`crates/flux-flow/assets/agent-loop.flux` is the real, load-bearing
     flow that uses them) — categorically unreachable from any static registry. Combined with zero
     references anywhere in README/docs/AGENTS/crates (confirmed by grep) and the file not
     demonstrating anything `agent-loop.flux` doesn't already do for real, this is a superseded
     scratch proof-of-concept, not a maintained example — **deleted** `examples/loop-poc.flux` (git
     history preserves it; nothing else references the path).
  4. Verified the `FLOW_PARSE_ONLY` tiering mechanism itself is load-bearing, not dead code: with
     the `advanced-code-review.flux` entry temporarily disabled, the sweep correctly failed again
     with `unknown operation: slack.message.send`; restored, green.
- Board regeneration and `CHANGELOG.md` intentionally **not** touched by this pass — a concurrent
  session owns edits to `docs/stories/README.md`/`CHANGELOG.md` in this tree right now (per the
  task's explicit constraint); the next board regen will pick up this story's `done` status.

## Notes
- Coverage map (verified 2026-07-07): flows_validate pins improve-tbench / improve-synthetic /
  eval-smoke / cognition-research; flux-sdk tests pin strict_review + multi-perspective; unguarded:
  advanced-code-review, channels-app, god-review, loop-poc, improve-multi, eval-synthetic.
- Crate-local example dirs (flux-app/examples, flux-lang/examples) are already test-covered.
