---
id: L-123
title: "Three production paths execute a user flow with no analyzer gate at all, and `fork --edit` is the sharp one"
pillar: Language
status: in-progress
priority: 11
epic: flux-lang-hardening
design: docs/designs/flux-lang-hardening.md
areas: [flux-lang, flux-flow, flux-app, flux-sdk]
note: "found by L-116's threat-model census, which asked which production paths reach `execute_flow` without `lower()`. The answer was several — none model- or remote-controlled, which caps severity at defence-in-depth, but `flux session fork --edit <file>` parses a fresh arbitrary user flow and runs it with zero static checks"
---

# The doors that skip the analyzer

## Goal

Make the static-analysis gate consistent across the paths that execute a flow, or state per path why
it is absent.

[L-116](L-116-repeat-and-loop-budgets.md) gave `repeat` an iteration budget, a transcript cap and a
yield point, and moved the loop budget to per-execution scope. In answering *"which production paths
reach `execute_flow` without `lower()`?"* it produced a census worth acting on.

**The model- and remote-facing surfaces are clean** — this is the reassuring half, and it is why this
is defence-in-depth rather than a remotely reachable DoS:

- the agent loop's AST is `analyze_flow`-gated (`crates/flux-flow/src/engine.rs:357`);
- the model's `flow_run` JSON AST **is** lowered (`crates/flux-flow/src/loop_host.rs:679`);
- no HTTP / A2A / MCP endpoint accepts an AST at all.

**The gaps are all local-operator inputs**, and they are inconsistent with each other:

1. ⚠ **`fork::diverge_edit` (`crates/flux-flow/src/fork.rs:304`)** — reached by
   `flux session fork --edit <file>`. It parses a **fresh arbitrary user flow** and runs it with no
   analyzer gate whatsoever. This is the sharp one: every other static check — not just the loop
   bound — is absent there.
2. **`flux app` journeys** (`crates/flux-app/src/app.rs:1236`, `:1239`) — no analyzer pass at all.
3. **The SDK execute doors** (`crates/flux-sdk/src/flow.rs:518`, `:547`, `:578`, `:612`) — `analyze()`
   is opt-in and never called.

Replay / fork / resurrect / what-if additionally re-`parse` a persisted `plan_source` without
re-lowering.

L-116's interpreter budget now backstops the *loop* case on all of these. Nothing backstops the rest.

## Acceptance

- [x] **Failing-first**: a test driving a flow through `fork --edit` that a `analyze_flow` pass would
      reject, showing it executes at the merge base.
- [x] Each of the three paths either runs the analyzer, or carries a comment at the call site saying
      why it is exempt and what backstops it instead. **No path is left silently inconsistent with
      its siblings** — that inconsistency is the story.
- [x] The SDK's opt-in `analyze()` is either made the default, or its being opt-in is documented as a
      deliberate embedder choice at the public surface, so an embedder knows they own the check.
- [x] A note in the design doc records the invariant decided here: which entry points guarantee
      static analysis and which do not, so the next entry point added knows which side it is on.
- [x] Full gate green.

## Notes

- L-116 already closed the loop-multiplication half at runtime: the budget is per flow execution and
  shared across `repeat`/`each`/`loop` at every depth. ⚠ Its one remaining boundary is a **composite
  op call**, which re-enters `execute_flow` with a fresh budget — so `loop { call composite_that_loops() }`
  still multiplies, bounded only by `DEFAULT_MAX_COMPOSITE_DEPTH` (8). Threading the budget handle
  through `run_call`/`eval_cond`/`execute_composite_call` is a materially larger change and is
  in scope for this story only if the implementer judges it proportionate — say either way.
- Severity framing, so nobody over- or under-reacts: these are local-operator inputs. Someone who can
  run `flux session fork --edit` can already run arbitrary commands. The value here is consistency
  and defence-in-depth, not closing a remote hole.

## Progress

- Filed 2026-08-01 from L-116's threat-model census, which enumerated these with file:line evidence
  rather than asserting the gate was universal.
- Implemented 2026-08-01 on `impl/L-123`. The invariant settled — *a flow body this engine did not
  itself produce is `analyze_flow`-gated; engine output (replayed / resumed / sliced) is exempt and
  says so at its call site* — is written up in the design doc with a per-entry-point table.
  - **Path 1, `fork --edit`** — gated. `fork::analyze_edited` runs ahead of `record_fork_plan`, so a
    refused plan leaves no accepted-attempt record (the C-211 "a failed fork leaves no trace" rule).
    `session_symbols` comes from the fork session's store, not an empty set, so an edit that drops
    leading statements and reads what the replayed prefix bound still analyzes clean — pinned by its
    own test.
  - **Path 2, `flux app` journeys** — gated by `app::analyze_journey`, against the executor's own
    narrowed registry plus the program's composites, on the post-`rewrite_asks` AST. **Symbol
    definedness is excluded**, deliberately and documented: a journey's environment is payload-shaped
    (`seed_payload` binds one symbol per event field), so definedness is a fact about a delivery, not
    the program. Found the hard way — the strict version broke two `flux-channels` tests whose
    journey reads a payload-only `$delivery`. Everything statically decidable stays enforced.
  - **Path 3, the SDK** — documented, not defaulted. Forcing `analyze()` would break `execute_with`'s
    seeding (seeded `$name`s read as unbound to plain `analyze`; only `analyze_seeded` sees them).
    `FlowClient` carries a *"Static analysis is yours to run"* section and each of the four
    `execute*` doors points at it.
  - Sibling exemptions commented: `diverge_inject` + fork prefix replay (`fork.rs` module header and
    call site) and the journey ask-resume (`app.rs`), each naming its backstop.
- ⚠ **Behavioural consequence to call out at integration.** Gating journeys makes the *deprecated*
  2+-positional call form (`send("cli", $reply)`) a startup error there, as it already is under
  `flux flow run`. `map_args_to_input` still accepts it at run time, by design and only so a legacy
  *stored* plan does not fail mid-flight after side effects. Repo-wide blast radius was one test
  fixture (`flux-app/tests/integration.rs`'s `ECHO`); both shipped journey examples already used the
  named-object form.
- **Composite-call budget boundary: judged out of proportion, not done.** L-116's remaining
  fresh-counter boundary needs a budget handle threaded through
  `run_call`/`eval_cond`/`execute_composite_call` — the interpreter's hot call path, a different
  subsystem from this story's call-site gates, with its own failing-first burden. Recorded as still
  open in the design doc; wants its own story.
