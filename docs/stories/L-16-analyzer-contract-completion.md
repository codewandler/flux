---
id: L-16
title: Analyzer contract completion — expression positions, name validity, bounds, locators
pillar: Language
status: done
epic: flux-lang-v1-hardening
design: docs/designs/flux-lang-v1-hardening.md
note: the analyzer accepts what the runtime rejects (call-in-arg, bad cond kinds), SymbolName is never validated (silent round-trip corruption F8), repeat max:0 passes, diagnostics have no locators, and two tree walkers hide new node kinds behind wildcards
---

# Analyzer contract completion

## Goal
Close the remaining analyzer holes (beyond L-15's definedness + required params) so a plan that
passes analysis actually runs: what the runtime's `eval_arg`/`eval_cond` reject, the analyzer
rejects first — with locators a repairing model can act on. Companion to L-15; together they make
error.rs's "the analyzer rejects unknown symbol/type mismatch" claim true.

## Acceptance
- [x] **Failing-first:** a `call` node in argument position (runtime `eval_arg` accepts only
      `lit/var/obj/list`, runtime.rs:2543) is a diagnostic with a "bind it first" hint; same for
      a non-`call/lit/var/expr` condition (`eval_cond`, runtime.rs:2479).
- [x] Non-identifier `SymbolName`s (dots, whitespace — via `SymbolName::is_identifier()` on
      ast.rs) rejected everywhere a symbol is *declared*; kills F8 from the JSON side (test uses
      the confirmed `Var{"a.b"}` counterexample).
- [x] `repeat max: 0` rejected; absurd `max` (> 100_000) diagnostic; empty `repeat`/`each`/`race`
      branch bodies rejected (consistent with the empty-`parallel`-branch rule).
- [x] Cross-branch same-symbol binds in `parallel` (including inner binds) are a diagnostic (F15
      analyzer half).
- [x] Diagnostics carry a JSON-pointer-style node path (`body[3].then[1]`).
- [x] `nested_bodies` and `node_contains_return` are exhaustive matches (no `_ =>`), like
      `for_each_node`.
- [x] `lower` wired into the production path (flux-flow compile, registry.rs:197, flux-cli) in
      the epic's integration step — flux-lang side exposes what that needs.
- [x] Full gate green; CHANGELOG entry.

## Progress
- 2026-07-02 flux-lang side implemented (analyze.rs only; shares L-15's staged
  `analyze_flow_with_session`/`lower_with_session` entry points):
  - Expression positions (F7): call args are checked against the runtime `eval_arg` set
    (`lit`/`var`/`obj`/`list` only) and conditions (`when`/`unless` cond, `repeat`/`loop` until,
    `assert`) against the `eval_cond` set (`call`/`lit`/`var`/`expr` only), each with a
    bind-it-first hint and a source comment naming the runtime coupling.
  - Name validity (F8): `SymbolName::is_identifier()` enforced at every declaration site
    (bind/memo, each as/collect, repeat collect, every optional `bind`, try catch, await binding,
    parallel/race branch names, ctx name, flow params) AND on `Var` references (the confirmed
    `Var{"a.b"}` corruption case); flow names via `is_valid_decl_name`, op names via
    `is_valid_op_name`. No existing flux-lang test needed loosening.
  - Bounds (F10): `repeat max: 0` rejected; `max > 100_000` diagnostic (`MAX_REPEAT_BOUND`);
    empty `repeat`/`each` bodies and empty `race` branch bodies rejected (mirroring the
    empty-`parallel`-branch rule).
  - Parallel disjointness (F15 analyzer half): two branches binding the same symbol — branch
    names or ANY inner binder form — is a diagnostic.
  - Locators (F11): diagnostics render a JSON-pointer-style node path (`body[3].then[1]`) into
    the message via an internal path-tracking accumulator; `Diagnostic`'s public shape is
    unchanged (message-only), so flux-flow/flux-sdk/flux-cli compile untouched.
  - Walkers (F12): `nested_bodies` and `node_contains_return` are exhaustive matches (no `_ =>`).
    While enumerating, `nested_bodies` also gained the dispatch-capable single-node positions the
    wildcard had been hiding from the cap-scope pass (bind/memo/return values, conds/untils,
    pipe steps, route selector, verify cmd/expect, scope acquire), and `node_contains_return` now
    sees through `with_tools` bodies.
  - Tests: `call_in_argument_position_is_a_diagnostic`, `invalid_condition_kind_is_a_diagnostic`,
    `non_identifier_symbol_names_are_rejected`,
    `repeat_each_race_bounds_and_empty_bodies_are_validated`,
    `parallel_cross_branch_binds_are_rejected`, `diagnostics_carry_node_paths`. flux-lang gate
    green; workspace builds.
- Remaining for done: wiring `lower` into the production path (flux-flow compile, registry.rs,
  flux-cli — the epic integration step), full gate, CHANGELOG. Residual (noted, not in scope
  here): `each` source / `jq` input / `parse` value are also `eval_arg` positions the analyzer
  still accepts calls in; `type_check_body` diagnostics don't carry node paths yet.

## Notes
- Findings F7, F8, F9, F10, F11, F12, F15 in docs/designs/flux-lang-v1-hardening.md.
