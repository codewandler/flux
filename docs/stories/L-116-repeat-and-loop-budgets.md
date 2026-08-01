---
id: L-116
title: "`repeat` gets the loop budget discipline; budget scope is decided"
pillar: Language
status: in-progress
priority: 8
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang, flux-flow]
note: "Review F3+F4, MEDIUM — repeat has no iteration budget/transcript cap/yield (timeout can never fire over a pure body); loop budgets are per-activation, doc says per-execution"
---

# `repeat` gets the loop budget discipline; budget scope is decided

## Goal

The `Repeat` arm (`runtime.rs:1956-2021`) has none of the three protections the `loop` arm carries
(iteration budget `runtime.rs:2496-2508`, `cap_transcript`, `yield_now`): a wire-supplied AST that
skipped `lower()` can carry `max: u32::MAX` and spin ~4.3e9 iterations, and a pure body has no
yield point so an enclosing `timeout` (tokio::time::timeout, `runtime.rs:3020-3034`) can never
fire. Separately, the documented "per-execution" budget (`runtime.rs:42`) is actually
per-node-activation (function-local counter), so nested loops multiply. Fix `repeat`; decide and
align the budget-scope semantics.

## Acceptance

- [x] Failing-first: a `repeat` with `max` above `DEFAULT_MAX_LOOP_ITERATIONS` through
      `execute_flow` (no analysis) fails with the budget error instead of spinning; a pure-body
      `repeat` inside `timeout` actually times out (the yield point exists).
- [x] `cap_transcript` applies on the repeat path (transcript ring bounded).
- [x] Budget scope decided and enforced-as-documented: either a per-execution counter threaded
      through `exec_body` (nested `each`/`repeat`/`loop` share one budget) or the doc-comment
      rewritten to per-activation with the multiplication risk stated; a nested-loops test pins
      whichever is chosen.
- [x] Answered in the story: does any production path reach `execute_flow` without `lower()`?
      (flux-flow read — determines the real-world severity and belongs in the closing note.)

## Progress

- **Decision — budget scope is per flow execution.** One `LoopBudget` (an `Arc<AtomicU64>` handle)
  is created per `execute_flow` / `execute_plan` call and threaded through `exec_body`; `repeat`,
  `each` and `loop` all charge that one counter at every nesting depth. The reasoning lives where
  the constant lives (`DEFAULT_MAX_LOOP_ITERATIONS`, `crates/flux-lang/src/runtime.rs`), not only
  in the commit message. A *handle* rather than a `&mut u64` specifically so `parallel` and `race`
  branches — which run concurrently and cannot share a mutable borrow — charge the same budget as
  their siblings instead of each being handed a private one; `steps`/`transcript` still fork and
  merge, the budget does not.
- **The one boundary that starts a fresh counter is a composite op call** — it is its own flow
  execution (own frame store, transcript, step count), bounded independently by
  `DEFAULT_MAX_COMPOSITE_DEPTH`. Stated in the constant's doc-comment rather than left implicit.
- **No second ceiling.** The analyzer's `MAX_REPEAT_BOUND` is now *derived* from
  `runtime::DEFAULT_MAX_LOOP_ITERATIONS` instead of restating `100_000`, so the static per-node
  ceiling can never drift above the runtime's per-execution one.
- **`repeat` now carries all three `loop` protections:** `budget.charge("repeat")` per round,
  `cap_transcript`, and `tokio::task::yield_now()`.
- Tests (`crates/flux-lang/src/runtime.rs`, all building the AST **directly** — going through
  `parse`/`lower` would not reproduce the defect, since `lower()` is what rejects the bound):
  `repeat_over_budget_terminates_under_default_budget`,
  `a_timeout_interrupts_a_pure_bodied_repeat`,
  `a_long_repeat_keeps_the_transcript_a_bounded_ring`,
  `nested_loops_share_one_per_execution_budget`.

### Closing note — does any production path reach `execute_flow` without `lower()`?

**Yes, several — but none of them are model- or remote-controlled, which caps the real-world
severity at "missing defense-in-depth / inconsistent gating" rather than a remotely reachable DoS.**

The two model-facing surfaces are *clean*: the agent loop's own AST is `analyze_flow`-gated
(`crates/flux-flow/src/engine.rs:357`), and the model's `flow_run` JSON AST **is** lowered before
execution (`crates/flux-flow/src/loop_host.rs:679`). A model cannot hand-write
`repeat 4294967295` and get it run. No HTTP/A2A/MCP endpoint accepts a flow AST at all — those
surfaces take text only.

The ungated paths are all *local operator* inputs:

- **`flux app` journeys** — `crates/flux-app/src/app.rs:1236/1239` executes `journey.flow` after
  only op-name/capability validation (`app.rs:800-830`); `analyze_flow`/`lower` never runs on a
  journey body.
- **The SDK execute doors** — `crates/flux-sdk/src/flow.rs:518/547/578/612` → `:807` take a
  caller-supplied `&DraftAst`; `analyze()`/`analyze_seeded()` are separate opt-in methods those
  doors never call. Same for `FlowEngine::start_flow_turn` (`engine.rs:1570`) and the voice driver
  (`crates/flux-flow/src/voice/driver.rs:374`).
- **replay / fork / resurrect / what-if** — `replay.rs:194`, `resurrect.rs:344`,
  `fork.rs:138/176/274`, `whatif.rs:479/580` re-`parse` a persisted `plan_source` and execute it as
  stored, never re-lowering. Mostly benign (that text was lowered when first accepted), with one
  exception: **`fork::diverge_edit` (`fork.rs:304`)**, where `flux session fork --edit <file>`
  (`crates/flux-cli/src/session.rs:363-372`) parses a *fresh, arbitrary* user-authored flow and
  runs it live with no analyzer gate at all.

`parse` accepts any `u32` for `repeat max` (`crates/flux-lang/src/cst_decode.rs:491`), and before
this story the interpreter looped `0..*max` unguarded — so every path above could execute an
effectively unbounded `repeat`. That is precisely why the budget belongs at the interpreter
boundary and not only in the analyzer.

## Notes

- Suggested fix shape: mirror the `loop` arm's three lines into `Repeat`; for scope, prefer one
  per-execution `u64` budget owned by the execution context — smallest diff that makes
  `runtime.rs:42`'s comment true.
- Review: docs/reviews/single/2026-08-01-flux-lang-subsystem-review.md F3, F4.
- Deviation from the suggested shape: a shared `Arc<AtomicU64>` handle rather than a `&mut u64`.
  A `&mut` counter cannot cross the `parallel`/`race` concurrency seam, so it would have had to be
  forked and merged there — which reintroduces exactly the multiplication F4 names, one level up.
