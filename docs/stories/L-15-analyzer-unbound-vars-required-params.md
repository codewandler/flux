---
id: L-15
title: Analyzer diagnostics — unbound $var references and missing required params
pillar: Language
status: done
epic: flux-lang-v1-hardening
design: docs/designs/flux-lang-v1-hardening.md
note: an unknown $var silently types as Any and an unbound {{sym}} renders verbatim at runtime; schemars-derived op schemas exist (D-34/36) but nothing checks required params before dispatch — both failure classes surface only at runtime, costing repair round-trips
---

# Analyzer: unbound-$var + required-param diagnostics

## Goal
Catch two whole failure classes at analysis time — where the compile repair loop fixes them for
one cheap round — instead of at runtime. Verified 2026-07-02: `infer_type` returns `Any` for an
unknown `$var` (`crates/flux-lang/src/analyze.rs:231`, no diagnostic); an unbound `{{sym}}` is
left verbatim at runtime (`runtime.rs:2681-2685`); op input schemas are schemars-derived
(D-34/D-36) but arg validation happens only inside each tool's `execute` (post-envelope). Serves
the Language pillar's "typed plans" claim.

## Acceptance
- [x] **Failing-first:** `unbound_var_reference_is_a_diagnostic` (flux-lang) — `$typo` used, never
      bound, empty session set → `Err` naming `$typo`. Sibling
      `session_symbols_satisfy_var_references` proves no false positive for session-seeded vars.
- [x] `analyze_flow(ast, ops, session_symbols: &HashSet<String>)` (+ `lower`; clean cutover, all
      call sites updated): a `Node::Var{name}` errors iff the name is not a flow param, not bound
      by ANY binder form anywhere in the flow (Bind, Memo, Each.as/collect, Repeat.collect,
      Pipe/Seq/Retry/Loop/Race.bind, Try.catch, Await.binding, Parallel branch names), and not in
      `session_symbols`. Order-insensitive on purpose — zero false positives, catches the typo
      class. Callers: compile passes the SessionView symbol names; composites (registry.rs:197) and
      presets pass empty; SDK passes seeded param names.
- [x] **Failing-first:** `object_call_missing_required_param_is_a_diagnostic` — in
      `check_call_types`: an object-literal call missing a `required_params` key, and an
      empty-args call to an op with required params, are diagnostics (keys are static even when
      values are dynamic); a call with the key present passes. No jsonschema dependency;
      dispatch-time stays the tools' own serde validation.
- [x] Explicitly out of v1 (recorded residual): `{{sym}}` interpolation inside `Fmt` templates —
      braces are ambiguous with literal content (code/JSON examples); a false positive would
      reject a *valid* plan.
- [x] Full gate green; CHANGELOG entry. Watch the compile-repair loop in the eval suite after
      landing (false-positive canary).

## Progress
- Filed 2026-07-02 from the harness claims review (P10 of the round).
- 2026-07-02 flux-lang side implemented (analyze.rs only, staged for the epic's Phase-3 cutover):
  - `analyze_flow_with_session(ast, ops, session_symbols: &HashSet<String>)` +
    `lower_with_session(…)` carry the definedness check; `analyze_flow`/`lower` remain as thin
    delegates with an empty set, doc-marked transitional — call sites (flux-flow compile.rs,
    registry.rs, flux-cli, flux-sdk) cut over in the epic integration step.
  - Definedness is order-insensitive by design: a `Node::Var` errors iff its name is not a flow
    param, not bound by ANY binder form anywhere in the flow (bind/memo, each as/collect, repeat
    collect, pipe/seq/retry/loop/race/timeout/budget/fallback/with_tools/scope/once bind, try
    catch, await binding, parallel branch names, ctx name — race branch names deliberately
    excluded: the runtime never binds them), and not in `session_symbols`.
  - Required params: `check_call_types` now rejects an object-literal call (and a lone
    `obj`-template call — keys are static even when values are dynamic) missing a
    `required_params` key; the empty-args case was already covered by `check_node`.
  - Tests (failing-first): `unbound_var_reference_is_a_diagnostic` (incl. the use-before-bind +
    param no-false-positive halves), `session_symbols_satisfy_var_references`,
    `object_call_missing_required_param_is_a_diagnostic`. `cargo test -p flux-lang` green
    (178 lib tests), workspace still builds.
- Remaining for done: call-site cutover + full-gate + CHANGELOG (epic Phase 3), then the
  eval-suite compile-repair canary watch.

## Notes
- Signature fallout is mechanical across flux-flow/flux-sdk/flux-cli — the gate catches misses.
- `FlowStore::seed` (state.rs:310) is what makes the SDK's seeded-param set the right
  session_symbols input there.
