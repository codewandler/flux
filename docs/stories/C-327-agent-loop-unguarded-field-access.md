---
id: C-327
title: "The agent loop's unguarded `$intent.kind` reports a flux-lang error for a stage failure"
pillar: Agent
status: ready
priority: 9
areas: [flux-flow, flux-lang]
note: "found by C-319 and confirmed by its reviewer — agent-loop.flux does `$intent_kind = $intent.kind` and `$kind = $step.kind` with no `?`, so any path leaving a stage result non-JSON dies with ``field access `.kind` … of a string``, naming flux-lang instead of the stage that failed; this is the error shape that sent C-304's implementor chasing a phantom"
---

# The agent loop reports a stage failure as a flux-lang error

## Goal

Make a non-JSON stage result say which stage failed, instead of surfacing as a field-access error
that names the language.

`crates/flux-flow/assets/agent-loop.flux:9` does `$intent_kind = $intent.kind`, and `:26` does
`$kind = $step.kind`, both without the `?` optional-access form. Strict access on a value that is a
string rather than an object raises `jq_access_error`
(`crates/flux-lang/src/runtime.rs:4267-4272`, with `json_type_name` at `:4288`), producing
``field access `.kind` … of a string``.

The mechanics that make this reachable: a bound string is auto-parsed as JSON **only** if it yields
an object or array (`crates/flux-lang/src/runtime.rs:4140-4147`); otherwise it stays a string and
hits exactly that error.

**The cost is diagnostic, and it is measurable.** This is the error shape that sent C-304's
implementor chasing a regression in its own diff, and it is the observation that produced
[C-319](C-319-strict-review-test-depends-on-tree-dirtiness.md). C-319 established that the mechanism
it *blamed* — a size-driven truncation into invalid JSON — does not exist, which leaves this
genuinely open: something left a stage result non-JSON, the error named flux-lang, and the operator
had no way to know which stage was at fault.

## Acceptance

- [ ] **Failing-first**: a test driving the loop with a stage that returns a non-JSON result, showing
      today's error names the field access rather than the stage.
- [ ] The error names the stage and what it returned. A reader should learn *which* step produced an
      unusable result without reading flux-lang's source.
- [ ] Both sites are covered — `$intent.kind` at `:9` and `$step.kind` at `:26`. They share the
      shape; fixing one leaves the other.
- [ ] **Decide whether the loop should also be resilient, not only legible**, and say why. A stage
      returning a non-JSON result is arguably a provider or tool defect the loop should surface and
      halt on — but halting with a good message is a different design than degrading to a default
      intent. State the choice; do not let it be decided implicitly by whichever is easier.
- [ ] ⚠ Changing `agent-loop.flux` touches a **frozen asset**. Check
      `crates/flux-lang/tests/cst_agreement.rs` — it pins flow assets by `ast_sha256`, so an AST
      change must regenerate that pin in the same commit. Note the hash is over the **AST**, not the
      text, so a comment-only edit will not red it (established by C-319's reviewer).
- [ ] Full gate green in both workspaces.

## Notes

- Found by [C-319](C-319-strict-review-test-depends-on-tree-dirtiness.md), whose implementor
  explicitly declined to widen scope into it, and confirmed at file:line by its reviewer.
- ⚠ **The original symptom is still unexplained, and that is worth knowing before you start.**
  C-319 proved there is no truncation path and that `detect_intent` returns a JSON object even when
  its stage fails (`crates/flux-flow/src/loop_host.rs:597-612`). So either C-304's implementor
  misattributed the error, or some *other* path leaves a stage result non-JSON and is still live.
  The `strict_review` test can no longer reach it. **The raw failure transcript from C-304's run
  would settle it** — worth hunting for before assuming this story is purely cosmetic, because if a
  live path exists, the message is only half the defect.
