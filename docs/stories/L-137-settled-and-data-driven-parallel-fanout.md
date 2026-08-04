---
id: L-137
title: "Settled and data-driven parallel fan-out"
pillar: Language
status: backlog
epic: flux-lang-authoring-ergonomics
design: docs/designs/flux-lang-authoring-ergonomics.md
areas: [flux-lang, flux-runtime, flux-lsp]
note: "Map homogeneous work over data concurrently and collect every outcome with explicit limits, ordering, cancellation, and failure"
---

# Settled and data-driven parallel fan-out

## Goal

Allow a program to fan the same bounded body over a collection and collect results in a stable
shape. Add an explicit settled mode in which a branch failure becomes typed result data instead of
cancelling siblings; ordinary fail-fast parallel behavior remains unchanged.

## Acceptance

- [ ] A design note fixes source/result ordering, concurrency limits, empty input, branch identity,
      variable scope, cancellation, fail-fast versus settled behavior, nested fan-out, and trace
      representation.
- [ ] The analyzer proves or enforces a finite input and positive concurrency bound; runtime limits
      remain effective even when the source collection is externally supplied.
- [ ] Failing-first tests cover all-success, one failure under fail-fast and settled modes, multiple
      failures, cancellation, timeout, empty input, deterministic result order, and concurrency cap.
- [ ] Settled entries use an explicit typed success/error representation shared with L-140; errors
      are not lossy strings and are never silently treated as success.
- [ ] Formatter round-trip, generated AST artifacts, syntax docs, LSP, and editor mirrors cover the
      accepted fan-out syntax.
- [ ] A provider-neutral example runs the same typed check over generic targets with `limit: 4` and
      reports all outcomes in input order.

## Progress

- 2026-08-05: Proposed to replace copy-pasted parallel branches when only their input data differs.

## Notes

- Illustrative syntax:

  ```flux
  targets = ["alpha", "beta", "gamma"]

  parallel settled each target in targets, limit: 4 -> attempts
    inspect(target) as Inspection

  failures = attempts.filter(.is_err)
  ```

- `settled` changes failure collection, not authorization: each branch still dispatches through the
  ordinary executor and may independently require approval.
