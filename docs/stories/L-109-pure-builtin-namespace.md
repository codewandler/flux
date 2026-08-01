---
id: L-109
title: "A closed pure-builtin namespace — expr/jq/peek call-style lower to pure nodes"
pillar: Language
status: backlog
priority: 36
epic: flux-syntax-simplification
design: docs/designs/flux-syntax-simplification.md
areas: [flux-lang]
note: "P7 — deletes the spec's own 'Beware: parses as an op call named expr' trap; shrinks @json to genuinely pathological shapes"
---

# A closed pure-builtin namespace — expr/jq/peek call-style lower to pure nodes

## Goal

Today `fmt(…)`/`parse(…)` are special-cased to pure nodes while `expr(…)`, `jq(…)`, `peek(…)`
call-style silently parse as ordinary *op calls* named `expr`/`jq`/`peek` — a trap the spec itself
flags ("Beware", syntax.md § Implementation status). Reserve the pure names as a closed set in
expression position: call-style spelling of any of them lowers to the pure node, never an op call.
This gives non-invertible `expr` formulas and non-symbol-input `jq` native spellings, shrinking the
`@json` escape to non-identifier names only.

## Acceptance

- [ ] Failing-first: `total = expr("price * qty", price: price, qty: qty)`,
      `first = jq(".results[0]", response)`, and `prev = peek(last)` lower to the pure
      `Expr`/`Jq`/`Peek` nodes; the existing keyword/sugar spellings keep working and formatting.
- [ ] A collision audit over the registered op catalog proves no production op is named
      `fmt`/`parse`/`expr`/`jq`/`peek` (test, not a one-time grep).
- [ ] `format` prefers the native call spelling for previously-`@json`-only shapes; round-trip
      property pools extended with non-invertible formulas and non-symbol jq inputs.
- [ ] The spec's "Beware" paragraph is deleted because it is no longer true.

## Progress
-

## Notes

- The expr *function library* names (`round`, `len`, …) stay inside formula strings — this story
  reserves only the five node names in expression position, keeping the closed set small.
- Breaking risk is narrow but real: any user op actually named `expr`/`jq`/`peek` changes meaning —
  hence the audit test and a `WHATS-NEW` note; ships inside the L-106→L-107 window.
