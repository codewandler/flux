---
id: L-62
title: Native syntax — Try / Race / Scope / Saga / Pipe (arm/body control-flow)
pillar: Language
status: done
priority:
epic: flux-lsp
design: docs/designs/flux-lang-cst.md
note: "@json coverage 3/4 — GATED on isolation. Native text for the arm/body control-flow nodes, reusing the match/case/branch parser machinery."
---

# Native syntax — Try / Race / Scope / Saga / Pipe (arm/body control-flow)

## Goal
Native text for the five arm/body control-flow `@json`-only nodes, per the reviewed CST proposals,
reusing the existing arm-parsing machinery (`match`/`case`, `parallel`/`branch`, `fallback`/`branch`).

## Acceptance
- [ ] `try` + body / `catch [$err]` + handler; `race <timeout_ms> [-> $bind]` + `branch $name` arms;
      `scope [$res = <acquire>]` + body / `finally` + cleanup; `saga` + `step`…`undo` arm pairs;
      `pipe [-> $bind]` + indented call steps.
- [ ] Each: CST production + native `format` arm + `cst_to_draft` lowering.
- [ ] Failing-first per node: round-trips natively, text contains **no** `@json`.

## Progress
- Done 2026-07-09 (parse.rs + format.rs): `try` + body / `catch [$err]` + handler (clause shape like
  `when`/`else`); `race <ms> [-> $b]` + `branch $name` arms (reuses `parse_arms`); `scope [$r =
  <acquire>]` + body / `finally` + cleanup; `saga` + `step`…`undo` arm pairs (custom `parse_saga_
  steps`); `pipe [-> $b]` + call steps. Guards keep round-trip total (branch names identifiers;
  scope renders only when bind/acquire are both present and acquire is spellable, else `@json`).
  `arm_body_control_flow_round_trips_natively` + the random property test pass; full suite green;
  clippy + fmt clean.

## Notes
- **Gated** on isolation. Depends on **L-59**. Native `|>` pipe operator stays deferred (this ships
  the block form). Confirm `SagaStep`/`Branch` field shapes at implementation.
