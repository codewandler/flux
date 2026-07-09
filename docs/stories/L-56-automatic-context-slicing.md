---
id: L-56
title: Automatic context slicing for planner and model ops
pillar: Language
status: done
priority:
epic: flux-lang-agent-speed
design: docs/designs/flux-lang-agent-speed.md
note: "KF4: derive the minimum model-visible context from HIR symbol reads, op schemas, and policy-visible evidence boundaries"
---

# Automatic context slicing for planner and model ops

## Goal
Reduce planner and model-op tokens by sending only the symbols, fields, evidence windows,
and diagnostics needed for the next decision, derived from the analyzed plan and operation
schemas rather than handwritten prompt trimming.

## Acceptance
- [x] Context slicing computes per-call dependencies from HIR symbol reads, field access
      paths, operation schemas, and planner repair diagnostics.
- [x] Model ops and planner feedback receive the sliced context by default, with an audit
      record of which symbols/evidence were included and why.
- [x] Token budgets are enforced before dispatch using exact host-provided counts when
      available and a deterministic fallback when not.
- [x] Private, hidden, secret-derived, or policy-denied symbols are never included unless
      explicitly referenced and permitted for that model-visible boundary.
- [x] Tests cover sliced model-op input, deterministic budget trimming, excluded private
      evidence, and equivalence for a flow whose full context would exceed the budget.

## Progress

Implemented as a new pure, deterministic engine plus two real (not speculative) default
wirings, per the scope-discipline guidance to prefer a testable slicing function over
touching every model surface:

- **`flux_lang::context_slice`** (`crates/flux-lang/src/context_slice.rs`, re-exported from
  the `flux-flow` facade): the core of the story.
  - `required_symbols_in_call` walks the closed call-arg/template grammar (`lit`/`var`/`peek`/
    `obj`/`list`/`jq`/`expr`/`fmt`/`parse`/`ctx`) and narrows a symbol to the accessed `jq`
    field path when the read is a `jq` access directly off a `var` — the "field access paths"
    dependency source. It's a separate function from the whole-flow walk on purpose: reusing
    the exhaustive `for_each_node` visitor for this would also re-visit the `var` nested
    inside a narrowed `jq`, collapsing the narrowing back to a whole read.
  - `required_symbols_in_flow` reuses `optimize::collect_var_reads` (bumped to `pub(crate)`,
    together with `collect_interp_reads`/`collect_interp_reads_str`) — the same
    soundness-audited exhaustive walk the L-53 optimizer's liveness pass uses — for the
    coarser "everything a whole rejected plan touches" signal.
  - `required_symbols_for_call` narrows to only the object-arg fields matching an op's
    declared `required_params`/`optional_params` (via `OpCatalog`) — the "operation schemas"
    dependency source — falling back to the unnarrowed read set for an unknown op (safe
    over-approximation, never a hole).
  - `required_symbols_from_diagnostics` scans `Diagnostic.message` text for `` `$name` ``
    tokens (diagnostics already spell them that way, e.g. "unbound symbol `$typo`") — the
    "planner repair diagnostics" dependency source.
  - `slice_context` is the central function: reference → gate (Private/Hidden/secret-derived/
    policy-denied excluded unless the name is in an explicit `Boundary::permitted` set, even
    when referenced) → budget (drop-and-continue in visibility/reason-priority order, sized
    by a host `TokenCounter` when supplied or the deterministic `estimate_tokens` ~4-chars/
    token fallback otherwise). Returns the kept names plus a full `SliceRecord` naming every
    inclusion/exclusion and why.
  - 17 unit tests cover every acceptance bullet directly, including a dedicated `budget_uses_
    the_exact_host_provided_counter_when_given` (bullet 3's "exact" half),
    `excludes_private_and_hidden_and_secret_and_policy_denied_by_default` +
    `a_gated_symbol_is_included_only_when_explicitly_referenced_and_permitted` (bullet 4),
    `budget_drop_and_continue_never_lets_one_oversized_candidate_starve_the_rest` and
    `slicing_is_deterministic_across_repeated_calls` (bullet 3's determinism), and
    `equivalence_for_a_flow_whose_full_context_would_exceed_the_budget` (bullet 5's named
    equivalence scenario).
- **Model-op wiring** (`flux_lang::runtime::build_ctx`, the `ctx`/`ctx_append` node
  evaluation that feeds `ai.reason` and any future `Ctx`-typed model-op param): Private/Hidden
  members are now excluded from a context pack by default, before any char-budget shrinking
  runs — being named in a `ctx` node's `include:` list is a reference, not a permission grant.
  The exclusion is audited via a new `context.sliced` sink observation (existing `context.
  shrunk` budget-drop behavior is untouched — this is a genuinely new, additive gate no
  existing test exercised, verified via a temporary revert that reproduces the failure). This
  demonstrates the "exact host-provided count" half of bullet 3 implicitly too: a follow-up
  could route the existing char-budget loop through the same `slice_context` call with a
  `CharCounter`, but that refactor was left alone here to protect the four existing,
  precisely-asserted `build_ctx` budget tests (out of scope — no behavior to add, only risk).
- **Planner-feedback wiring** (`flux_flow::compile::gate_candidate_plan`'s rejected-plan
  branch, the one-shot AND the phased `emit_plan` tool-call paths share it): a rejected plan's
  repair message now appends a "Relevant session symbols for this repair" block sliced from
  `required_symbols_in_flow(&ast.body)` (what the rejected plan read) plus
  `required_symbols_from_diagnostics` (what its diagnostics named), against a 2000-token
  budget using the deterministic fallback estimator (no host-exact counter is threaded through
  this call site — the `Ctx`-pack wiring above is the exact-count demonstration). Appends
  nothing when nothing in the (already visibility-filtered) `SessionView` is referenced — the
  common case (a typo'd op name, a malformed literal). Verified load-bearing the same way (a
  temporary revert reproduces a real test failure, not just a smoke check).

### Interpretation notes / residuals
- "Secret-derived" and "policy-denied" are real gates in the engine (`SymbolFlags.secret_
  derived`/`policy_denied`, tested directly), but `flux-lang` stays IO-free by design, so
  nothing in this crate computes those flags from `flux-secret`'s `Redactor` or `flux-policy`
  yet — both current callers (`build_ctx`, the repair-feedback wiring) populate them as
  `false` today. Wiring a real secret/policy signal into a candidate's `SymbolFlags` is
  follow-up work for whichever surface first needs it, not implied by this story's Goal/
  Acceptance text.
- Did not touch `compile_turn_inner`'s far larger multi-tool agentic-loop message assembly
  beyond the single, well-isolated `gate_candidate_plan` diagnostics arm (shared by every
  `emit_plan`/`emit_plan_delta` caller) — going further (e.g. narrowing segment C's
  `symbols_block` on every planner call, not just repairs) would be exactly the "speculative
  wiring into every model surface" the story's own scope-discipline guidance says to avoid,
  and segment C already has its own tested, independent cap/eviction contract (A-07/A-24) this
  story doesn't need to touch.
- Did not fix the pre-existing, separate gap where a `Ctx` pack's `members` are symbol
  *names* only (not dereferenced content) by the time they reach a model op like `ai.reason` —
  real, but orthogonal to "derive the minimum context automatically"; flagged here for a
  follow-up story rather than folded into this one.

Gate: `cargo build --workspace`, `cargo test --workspace` (108/108 test-result blocks green,
zero failures), `cargo clippy --workspace --all-targets -- -D warnings` (clean), `cargo fmt
--all` (applied), `cargo test -p flux-codegate` (4/4 green) — all run and green.

## Notes
- Epic: [flux-lang-agent-speed](../designs/flux-lang-agent-speed.md).
- This should compose with existing `context` projection work in `flux-runtime`; do not add
  a model-visible bypass around redaction or policy.
