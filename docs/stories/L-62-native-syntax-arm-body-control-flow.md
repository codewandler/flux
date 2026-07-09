---
id: L-62
title: Native syntax — Try / Race / Scope / Saga / Pipe (arm/body control-flow)
pillar: Language
status: backlog
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
- (not started — gated on isolation; depends on L-59)

## Notes
- **Gated** on isolation. Depends on **L-59**. Native `|>` pipe operator stays deferred (this ships
  the block form). Confirm `SagaStep`/`Branch` field shapes at implementation.
