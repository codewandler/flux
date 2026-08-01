---
id: L-111
title: "Hot-path ergonomics — match on expressions, else-when chains, multi-value cases"
pillar: Language
status: backlog
priority: 40
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang]
note: "P9 — removes the `$kind = $step.kind` pre-bind, three-deep when nesting, and duplicated byte-identical case arms visible in agent-loop.flux"
---

# Hot-path ergonomics — match on expressions, else-when chains, multi-value cases

## Goal

Three ceremonies visible in the flagship flow: `match` only accepts a bound `$var`/literal subject
(agent-loop pre-binds `$kind = $step.kind` twice), `when` has no chaining form (the batch arm nests
three deep), and byte-identical case bodies must be duplicated (agent-loop's `"chat"`/`"error"`
arms). Add: `match step.kind` (dotted/expression subject, auto-bound internally),
`else when <cond>` chaining at the opener's indent, and `case "a", "b"` multi-value arms.

## Acceptance

- [ ] Failing-first per feature: parse + lower + execute tests, and `format` emits the new
      spellings canonically.
- [ ] `match <expr>` lowers to the same AST as the explicit pre-bind (a fresh internal symbol) so
      the runtime and optimizer are untouched; the trace still shows the subject value.
- [ ] `else when` nests arbitrarily and round-trips; the dangling-else rule stays the existing
      same-indent rule (syntax.md § indentation).
- [ ] Multi-value `case` matches on JSON equality against any listed value; duplicate values
      across arms are an analyzer error.
- [ ] agent-loop.flux is simplified with the new forms in the same PR (after L-104), with the
      mock-provider loop tests green — the story's proof that the ceremony actually disappears.

## Progress
-

## Notes

- New syntax ⇒ full mirror obligation: Prism, tree-sitter, TextMate/IntelliJ greps, plus golden
  regeneration (crate AGENTS.md § mirrors) — budget for it, it is most of the cost.
