---
id: L-71
title: Merged model-facing Node schema on emit_plan (third emission arm)
pillar: Language
status: done
priority:
epic: flux-lang-emission-ab
design: docs/designs/flux-lang-emission-ab.md
note: "One Node object (kind enum + unioned optional props) instead of the 43-variant oneOf on emit_plan — same wire, same parse path; FLUX_EMISSION=merged."
---

# Merged model-facing Node schema on emit_plan (third emission arm)

## Goal
Shrink `emit_plan`'s model-facing schema from the 43-variant `oneOf` (~30 KB / ~7.5k tokens measured)
to a **single merged Node object** — `kind: enum[43]` plus the union of every variant's properties,
each optional, each declared once — added as a third emission arm (`EmissionArm::Merged`,
`FLUX_EMISSION=merged`) beside `json`/`text`. The wire format is already `{"kind": ..., ...props}`
(internal serde tagging), so the wire bytes, the parse path, and the internal `Node` AST are
**unchanged**; only the advertised schema shrinks. Per-kind semantics stay in the
`node_kind_catalog()` SSOT the prompt already carries.

## Acceptance
- [x] `flux_lang::schema::merge_node_schema(&mut Value)` + memoized `model_schema()`: locates the
      `Node` definition, replaces the `oneOf` with one object schema (`kind` enum, unioned optional
      props, `required: ["kind"]`), merges conflicting property shapes to an `anyOf` (so every
      `$ref` stays live — nothing to prune), and keeps a field description only when all kinds
      agree on it. Drift-proof tests: the `kind` enum matches `node_kind_rows()` exactly; every
      property of every variant survives the merge; no dangling `$ref`; the serialized merged
      schema is < 50% of `ast_schema()`; the merge is idempotent.
- [x] `EmissionArm::Merged` (`FLUX_EMISSION=merged`): `planner_tools` advertises the merged schema on
      the `ast` param (built by post-processing `tool_input_schema::<EmitPlanInput>()` so
      `complete`/`gather`/`brief` stay intact); prompt grammar, handler, and repair path are the
      json arm's, unchanged. Tests: arm parsing, merged tool-schema shape + prompt byte-equality,
      and a mock-provider `compile_turn_with_arm` run proving a schema-shaped payload compiles
      identically to json.
- [x] The live emission-ab harness (`crates/flux-eval/tests/emission_ab.rs`) runs `merged` as a third
      arm so the A/B table can be re-cut before any cutover decision.
- [x] `docs/designs/flux-lang-emission-ab.md` gains the third-arm section (rationale, merge rules,
      measured sizes, pre-registered decision rule); CHANGELOG entry under `[Unreleased]`; public
      docs on the website (Execution model "How a model emits a plan" + Tooling's
      `fluxlang schema --merged`).

## Progress
- 2026-07-09: story opened from a design discussion — measured the current surface: full `DraftAst`
  schema ~29,911 bytes (~7.5k tokens) on the tool + ~3.2k-token node-kind catalog in the prompt,
  overlapping. Decision: merge at the schema-projection level only (AST/wire untouched), measure via
  the kept L-20 A/B scaffold before any cutover.
- 2026-07-09: implemented end-to-end. `merge_node_schema`/`model_schema` in `flux-lang` (merged
  schema: 29,911 B → 10,248 B, −66%); `EmissionArm::Merged` + `merged_emit_plan_schema()` in
  `flux-flow` (prompt/decode shared with json — the compiler's exhaustive matches found every
  site); three-arm live harness; `fluxlang schema --merged`; design-doc section + website docs +
  CHANGELOG. Description-consistency rule added after the naive merge stamped `var.name` with
  throttle's "stable bucket name" doc. NOT done (deliberate): semantic node consolidation and
  per-node retry/timeout props — rationale in the design doc's "What was deliberately NOT done".
- **Next:** run the live three-arm A/B (`FLUX_EMISSION_AB=1 cargo test -p flux-eval --test
  emission_ab -- --ignored --nocapture`) and record the table in the design doc; cut over or delete
  the arm per the pre-registered rule.

## Notes
- Key files: `crates/flux-lang/src/schema.rs` (merge + tests), `crates/flux-flow/src/compile.rs`
  (`EmissionArm`, `planner_tools`, grammar selection), `crates/flux-eval/tests/emission_ab.rs`.
- Placement rules (`checkpoint` top-level only, pure-leaf `obj`/`list`) are context-sensitive — no
  JSON Schema expresses them; the analyzer + repair loop stay the enforcement authority in every arm.
- Conflicting property shapes across kinds (`value`: Node vs raw JSON; `branches`: Branch vs
  FallbackBranch; `cases`: MatchCase vs RouteCase; `name`: SymbolName vs String) loosen to a
  permissive schema — serde + analyze catch misuse, same as today.
