---
id: L-28
title: "Ledger fast-forward must not silently continue on a missing rehydration value"
pillar: Language
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "in resumable mode the fast-forward rebinds a skipped statement's value only `if let Some(value) = store.get_value(vid)?` — a None skips the rebind but still counts the statement as fast-forwarded, silently losing the bound symbol; top_level_bind also has no parallel arm. Latent until L-25's cross-store resume"
---

# Ledger fast-forward must not silently continue on a missing rehydration value

## Goal
Make L-22 prefix fast-forward fail loudly instead of silently losing a binding. In resumable mode the
fast-forward rehydrates each skipped statement's value only `if let Some(value) = store.get_value(vid)?`
(`crates/flux-lang/src/runtime.rs:1143`); on `None` it skips the rebind but **still** counts the statement as
fast-forwarded (inside `take(ledger_end)`) and re-ledgers it as `StatementCompleted{skipped:true}` — so the
symbol is silently dropped while execution resumes past it. `top_level_bind` (`runtime.rs:713`) also has no
arm for `parallel`, so a skipped `parallel`'s branch binds are never rehydrated. Latent today (the in-session
value store is INSERT-only, never evicted) — reachable exactly when [L-25](L-25-flow-run-resumable-mode.md)'s
cross-store / fresh-store resume lands.

## Acceptance
- [ ] Failing-first test: a resumed run whose ledgered `vid` is absent from the value store surfaces a hard
      resume error (or halt) naming the lost statement — not a silent skip that later dies on "unbound symbol".
- [ ] `top_level_bind` gains a `parallel` arm so a skipped parallel's branch binds are rehydrated.
- [ ] Behaviour on the in-session (all-values-present) path is unchanged.

## Progress
- 2026-07-03 DONE — resume fast-forward returns a hard `FlowError::Runtime` (names the lost statement + symbols) when a ledgered value can't be rehydrated; `top_level_bind` gained a `parallel` arm. Tests: `resume_with_missing_ledger_value_is_a_hard_error`, `resume_with_missing_parallel_value_is_a_hard_error`, `top_level_bind_covers_parallel_branches`; in-session path unchanged. Residual: full per-branch parallel cross-store rehydration needs a ledger-schema change (L-25 scope). Full gate green.

## Notes
- Evidence: `crates/flux-lang/src/runtime.rs:1143` (silent skip), `:713` (no parallel arm).
- Residual of [L-22](L-22-reified-halts-statement-ledger.md); worth a note in the
  [L-25](L-25-flow-run-resumable-mode.md) design. Design: [library-hardening](../designs/library-hardening.md).
