---
id: L-55
title: Plan-delta emission for cheap safe repairs
pillar: Language
status: done
priority:
epic: flux-lang-agent-speed
design: docs/designs/flux-lang-agent-speed.md
note: "KF3: let repair turns patch the previous AST, then materialize and validate the full plan before execution"
---

# Plan-delta emission for cheap safe repairs

## Goal
Let planner repair rounds emit a small patch against the previous Flux-Lang AST instead of
re-emitting the entire plan, while keeping execution behind the same full-AST analyzer,
policy, and audit gates.

## Acceptance
- [x] A versioned plan-delta representation can replace, insert, delete, or edit nodes by
      stable path or node id without executing any partial plan.
- [x] The engine materializes the delta into a complete `DraftAst`, normalizes it through
      the same model-ingress rules as full emissions, and runs the existing analyzer before
      any operation dispatch.
- [x] Failed or malformed deltas produce repair feedback and do not mutate the accepted
      previous plan.
- [x] Audit/state stores enough source material to reconstruct both the delta and the
      materialized full plan.
- [x] Tests cover a successful one-node repair, malformed delta rejection, stale-base
      rejection, and an attempted hidden/denied op that remains gated after patching.

## Progress
- 2026-07-09 review fix: rejection feedback now carries the rejected plan's content hash (the
  `base` the tool schema promised - a model's first delta previously always failed stale-base);
  verdict bookkeeping shared via `apply_plan_gate`; hash derivation reuses `sha256_hex`; bounds
  errors folded into one helper. NOTE: hash MUST come from the struct serializer, never the
  `Value` copy (BTreeMap re-orders keys - caught live by the delta tests).
- 2026-07-09: Implemented end-to-end. New `flux_flow::delta` module (`crates/flux-flow/src/delta.rs`)
  materializes a versioned `{version, base, ops[]}` patch against a `DraftAst`'s JSON wire form: a
  single generic path walker (`resolve_container_mut`) resolves `body[i]`, `.then[i]`,
  `.otherwise[i]`, `.handler[i]`, `.finally[i]`, `.body[i]`, `.branches[j]`, `.cases[j]`,
  `.default[i]`, `.steps[j]`, `.undo[i]`, and any nesting of those — full path coverage, not just
  the "one level" floor the story allowed, since the walker treats every segment as a generic JSON
  field/array hop rather than matching `Node` variants one at a time. `base` is a SHA-256 of the
  previous AST's canonical JSON (`ast_content_hash`); a mismatched `base` is refused as stale
  without touching the previous rejected plan. A new `emit_plan_delta` tool (`compile.rs`) is
  advertised only once a previous plan this turn was decoded and then rejected
  (`last_rejected_ast`); its materialized result runs through the SAME `relax_field_access`
  normalization, `hidden_ops_in` surfacing, gather enforcement, and `validate_plan`
  (analyzer/lower) gate a full `emit_plan` does — factored into a shared `gate_candidate_plan`
  helper so provenance (whole emission vs. patch) can never change what gets accepted. The
  accepted `Compiled` carries the raw delta JSON (`delta_source`) alongside the existing
  materialized-plan `plan_source`, threaded through to a new `EventKind::PlanAttempted.delta_source`
  field (additive, `#[serde(default)]`, redacted like `plan_source`) so audit can reconstruct both
  the patch that was sent and the full plan it materialized into.
  Tests: `crates/flux-flow/src/delta.rs` (11 unit tests — hash stability, replace/insert/delete
  mechanics, nested and deeply-nested path resolution, stale-base non-mutation, out-of-range/
  non-list/undecodable rejection, `parse_delta` malformed-input rejection) plus 5 in
  `compile.rs`'s `mod tests`: `delta_repairs_a_single_invalid_node` (successful one-node repair +
  audit reconstruction), `delta_with_malformed_op_is_repair_feedback`,
  `delta_with_stale_base_is_rejected_then_a_correctly_based_delta_still_succeeds` (stale rejection
  + proof the previous plan survives untouched), `delta_that_introduces_a_hidden_op_is_still_gated`,
  and a defensive `delta_with_no_previous_plan_is_repair_feedback`. Gate: `cargo build --workspace`,
  `cargo test --workspace` (green, 0 failed), `cargo clippy --workspace --all-targets -- -D
  warnings` (clean — added a scoped, justified `#[allow(clippy::large_enum_variant)]` on
  `TurnOutput`: the new `delta_source` field tipped `Compiled` over clippy's size-diff threshold,
  and boxing it would ripple through ~35 call sites for a once-per-turn, not hot-path, allocation),
  `cargo fmt --all` (clean), `cargo test -p flux-codegate` (layering intact — delta stays a private
  module inside the existing L3 `flux-flow` crate, no new crate).

## Notes
- Epic: [flux-lang-agent-speed](../designs/flux-lang-agent-speed.md).
- This is an emission optimization, not a new execution semantics. The runtime still sees
  a complete analyzed plan.
