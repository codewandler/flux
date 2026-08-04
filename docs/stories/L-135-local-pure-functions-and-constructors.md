---
id: L-135
title: "Local pure functions and typed value constructors"
pillar: Language
status: backlog
epic: flux-lang-authoring-ergonomics
design: docs/designs/flux-lang-authoring-ergonomics.md
areas: [flux-lang, flux-lsp]
note: "Name repeated computation and terminal-result construction without hiding operations, approvals, or control flow"
---

# Local pure functions and typed value constructors

## Goal

Let authors name repeated deterministic computation and value construction inside a Flux program.
Keep the first version deliberately small and pure so functions cannot disguise operations,
approvals, retries, concurrency, or ambient context.

## Acceptance

- [ ] A design note fixes declaration/call syntax, lexical capture, type inference, return rules,
      recursion policy, evaluation limits, diagnostics, AST/wire shape, and lowering strategy.
- [ ] Failing-first analyzer tests reject operation calls, task calls, approvals, mutable capture,
      recursion (unless separately bounded and accepted), and effects reached through another local
      function.
- [ ] Runtime tests cover parameter binding, optional/default policy if accepted, typed constructors,
      early return semantics, deterministic evaluation, and budget exhaustion without panics.
- [ ] Existing pure built-ins and expression evaluation have one documented relationship to local
      functions; this story does not create a competing expression engine.
- [ ] Formatter round-trip, generated artifacts, syntax docs, LSP navigation/diagnostics, and editor
      mirrors cover declarations and calls.
- [ ] A domain-neutral example factors repeated terminal result construction into one typed helper.

## Progress

- 2026-08-05: Proposed after repeated near-identical result objects obscured terminal policy in a
  substantial Flux program.

## Notes

- Illustrative syntax:

  ```flux
  record Outcome {
    status: "ok" | "blocked"
    kind?: String
    messages: List<String>
  }

  fn blocked(kind: String, messages: List<String>) -> Outcome
    return Outcome { status: "blocked", kind, messages }

  when checks.empty
    return blocked("no_checks", ["at least one check is required"])
  ```

- Constructors may turn out to be ordinary calls to record types; the story should prefer that
  smaller model if it remains unambiguous.
