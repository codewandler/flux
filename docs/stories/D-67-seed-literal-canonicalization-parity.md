---
id: D-67
title: FlowStore::seed literal-canonicalization parity — a seeded object must marshal like a lit-bound one
pillar: Agent
status: done
design:
epic:
note: "found adopting D-56 in ai-agents (C-14): execute_with-seeded objects reached preset ops as the bare object while the old Bind{Lit}-prepend workaround delivered the string-wrapped form — seed() stored Value::from_json where the interpreter stores lit_text"
---

# FlowStore::seed literal-canonicalization parity

## Goal
A `$name` seeded via `FlowStore::seed` / `FlowClient::execute_with` must be indistinguishable from
a literal-bound one everywhere downstream — same stored shape, same text, same arg marshaling — so
swapping the old Bind-node-prepend workaround for the D-56 seam is behavior-preserving.

## Root cause
`FlowStore::seed` stored seeded values structurally via `Value::from_json` (an object arrived as
`Value::Struct`), while the interpreter's own `Node::Lit` bind path canonicalizes every literal to
the JSON-as-string `Value::String(lit_text(&jv))` — the same shape op results take. `seed()`'s doc
comment even claimed parity with the interpreter's literal path but didn't deliver it. The
asymmetry is observable at arg marshaling: `map_args_to_input` passes a lone *object* argument
straight through as the op's whole input but wraps a lone *string* under the op's sole required
param — so `some_op($input)` received a different input depending on whether `$input` was seeded
or literal-bound (ai-agents' preset ops broke exactly this way; the structural path also lost
integer fidelity through the value model's f64 numbers: `3` read back as `3.0`).

## What changed
- `flux_lang::runtime::lit_value` (new, public, next to the private `lit_text` it wraps): the
  canonical stored `Value` for a host-supplied JSON literal — a JSON string is itself, `null` the
  empty string, anything else its compact JSON text, wrapped as the JSON-as-string
  `Value::String`. The one canonicalization a host applies when injecting a value from outside the
  flow.
- `FlowStore::seed` (crates/flux-flow/src/state.rs) stores through `lit_value` instead of
  `Value::from_json`; its doc comment now states the delivered contract.
- `state.rs::seed_then_resolve_round_trips` updated: a structured seed reads back as the compact
  JSON text (`Value::String("{\"n\":3}")`), integer preserved — not the f64-lossy structural form.
- Field access on seeded objects keeps working: `jq_parse_input` already re-parses a string value
  that parses as a JSON object/array before path traversal (op results are stored the same way),
  so `$seeded.field` behaves as before.
- Rider: `flux_flow` crate root now re-exports `TranscriptAccumulator` and `UsageRecording`
  alongside the existing voice re-exports (both were already pub in `flux_flow::voice`; downstream
  had to deep-import them).

## Acceptance
- [x] Failing-first parity test written and observed red before the fix:
      `flow::tests::a_seeded_object_marshals_exactly_like_a_literal_bound_one`
      (crates/flux-sdk/src/flow.rs, colocated with the D-56 tests) — seeding an object via
      `execute_with` delivers the identical op input as the equivalent literal `Bind` (the lone
      `$var` argument string-wraps under the op's sole required param), then green after.
- [x] No flux test relies on a Struct-shaped seed at an op; the one store-level round-trip
      assertion encoding the old behavior updated in place.
- [x] Scoped gate green: `cargo build/test/clippy --all-targets -- -D warnings` for
      flux-lang, flux-flow, flux-sdk; consumers flux-cli and flux-app build + test green.

## Notes
- Found adopting D-56 in ai-agents (its C-14 story); verified empirically there before filing.
- Same review context carries the flux-flow crate-root voice re-export rider
  (`TranscriptAccumulator`, `UsageRecording`).
