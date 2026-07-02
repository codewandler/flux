---
id: L-15
title: Analyzer diagnostics — unbound $var references and missing required params
pillar: Language
status: ready
priority: 10
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
- [ ] **Failing-first:** `unbound_var_reference_is_a_diagnostic` (flux-lang) — `$typo` used, never
      bound, empty session set → `Err` naming `$typo`. Sibling
      `session_symbols_satisfy_var_references` proves no false positive for session-seeded vars.
- [ ] `analyze_flow(ast, ops, session_symbols: &HashSet<String>)` (+ `lower`; clean cutover, all
      call sites updated): a `Node::Var{name}` errors iff the name is not a flow param, not bound
      by ANY binder form anywhere in the flow (Bind, Memo, Each.as/collect, Repeat.collect,
      Pipe/Seq/Retry/Loop/Race.bind, Try.catch, Await.binding, Parallel branch names), and not in
      `session_symbols`. Order-insensitive on purpose — zero false positives, catches the typo
      class. Callers: compile passes the SessionView symbol names; composites (registry.rs:197) and
      presets pass empty; SDK passes seeded param names.
- [ ] **Failing-first:** `object_call_missing_required_param_is_a_diagnostic` — in
      `check_call_types`: an object-literal call missing a `required_params` key, and an
      empty-args call to an op with required params, are diagnostics (keys are static even when
      values are dynamic); a call with the key present passes. No jsonschema dependency;
      dispatch-time stays the tools' own serde validation.
- [ ] Explicitly out of v1 (recorded residual): `{{sym}}` interpolation inside `Fmt` templates —
      braces are ambiguous with literal content (code/JSON examples); a false positive would
      reject a *valid* plan.
- [ ] Full gate green; CHANGELOG entry. Watch the compile-repair loop in the eval suite after
      landing (false-positive canary).

## Progress
- Filed 2026-07-02 from the harness claims review (P10 of the round).

## Notes
- Signature fallout is mechanical across flux-flow/flux-sdk/flux-cli — the gate catches misses.
- `FlowStore::seed` (state.rs:310) is what makes the SDK's seeded-param set the right
  session_symbols input there.
