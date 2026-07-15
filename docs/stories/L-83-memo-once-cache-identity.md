---
id: L-83
title: Key memo/once cache hits on op+input provenance, not symbol name/label
pillar: Language
status: done
priority: 14
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "Correctness (Medium) — memo/once return a stale/wrong value or skip a real side effect on name/label collision"
---

# Key memo/once cache hits on op+input provenance, not symbol name/label

## Goal
Fix the cache-identity bug in the reflexive memoization ops. `memo $x = op(args)` decides a hit whenever
`store.resolve(session,"x")` returns *any* binding, without checking it came from this op+args — so
`memo $config = fetch_config()` returns an unrelated earlier `$config` and never runs `fetch_config`
(and mislabels the transcript `(cached)` over the old text); editing `memo $s = v1()` → `v2()` and
re-running also returns v1. `once` keys only on `(session, label)`, so two blocks sharing a label
(LLM-picked default, copy-paste, cross-turn) silently skip the second's genuinely different side effect.

## Acceptance
- [x] Failing-first test `memo_keys_on_op_and_input_provenance`: a `memo` re-executes when the op or
      its inputs differ rather than returning a stale binding of the same name.
- [x] `once` keys fold body identity (`once_key(label, body)`), so same-label-different-body blocks
      don't collide.
- [x] Hits keyed on op + canonical-input provenance (`memo_cache_key(op, args)`), not "name is bound".

## Progress
- **2026-07-15 — DONE (full workspace gate green).** `memo` now keys the cache on `memo_cache_key(op,
  args)` (op + canonical inputs) instead of "name is bound"; `once` keys on `once_key(label, body)`.
  Verified by `memo_keys_on_op_and_input_provenance` + the full suite.

## Notes
- `crates/flux-lang/src/runtime.rs:2061` (`memo`), `:3061` (`once`); `crates/flux-flow/src/state.rs:84`.
- Design: [harness-hardening](../designs/harness-hardening.md).
