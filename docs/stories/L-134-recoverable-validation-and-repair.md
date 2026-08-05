---
id: L-134
title: "Recoverable validation failures and bounded repair"
pillar: Language
status: backlog
epic: flux-lang-authoring-ergonomics
design: docs/designs/flux-lang-authoring-ergonomics.md
areas: [flux-lang, flux-orchestrate]
note: "Keep `assert` fatal; make malformed external values explicit data with bounded repair and an owned fallback"
---

# Recoverable validation failures and bounded repair

## Goal

Separate violated program invariants from malformed external results. Preserve `assert` as a fatal
invariant check, while giving typed task results and other external values an explicit, bounded
repair path whose failure can be handled as data or returned as a stable terminal result.

## Acceptance

- [ ] A design decision specifies the recoverable validation value, scope/lifetime of diagnostics,
      repair input, maximum attempts, terminal fallback, cancellation behavior, and trace events.
- [ ] Failing-first tests distinguish a valid negative result, a validation failure repaired on the
      next attempt, exhausted repair, provider failure, cancellation, and a fatal `assert`.
- [ ] Repair is opt-in and statically bounded; no spelling permits implicit or unbounded retries.
- [ ] Each attempt preserves causal trace identity and validation evidence without producing an
      invalid tool-call history or bypassing provider/runtime budgets.
- [ ] Analyzer control-flow checks prove the value is bound only on successful validation and that
      every exhausted-repair path is handled or returned.
- [ ] A provider-neutral example repairs a malformed generic assessment once, then returns an
      explicit blocked result if it remains invalid.

## Progress

- 2026-08-05: Proposed to replace repeated parse/assert/re-prompt ladders without weakening fatal
  invariant semantics.

## Notes

- Illustrative syntax:

  ```flux
  assessment = task(role: "assessor", input: { artifact }) as Assessment
    repair 1 with validation_error
    else return Outcome {
      status: "blocked"
      kind: "invalid_assessment"
      details: validation_error
    }
  ```

- `repair` describes validation recovery, not general operation retry. Existing retry/backoff
  semantics remain the mechanism for transient effect failures.
