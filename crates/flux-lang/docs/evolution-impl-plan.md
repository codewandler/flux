# Flux-Lang evolution — implementation plan

Build plan for the design in
[`../../../docs/designs/flux-lang-evolution.md`](../../../docs/designs/flux-lang-evolution.md). Committed
and co-located with the design + the conformance matrix ([`STATUS.md`](STATUS.md)); keep all three in
sync on every change (the "keep design + plan in sync" rule). Each phase ships behind the full dev loop
(`cargo build/test`, `clippy -D warnings`, `fmt`, `flux-codegate`) with a test that fails before the
change.

## P0 — op-input JSON Schema (cross-cutting prerequisite) — ✅ DONE
- **Done in `opspec.rs`.** `OpSpec.inputs` changed from positional `Vec<TypeRef>` to **named**
  `Vec<Param>` (`Param { name, type, optional }`) — the crux, since `OpSpec` carried no param names but
  every downstream consumer (`schema_params`, `OpSignature::from_spec`, `param_signature`,
  `runtime::map_args_to_input`, the planner catalog) is name-keyed. `OpSpec::lower()` now projects a real
  object schema via `input_schema()` (killing the `{"type":"object"}` placeholder); a `type_ref_to_schema`
  helper maps each `TypeRef` variant (`Named → #/$defs/<name>` `$ref`, forward-compat with the P1 prelude).
- **Ordering contract:** required-param order is preserved (the `required` JSON array; load-bearing for
  positional binding); optional params have no order guarantee (`serde_json` has no `preserve_order` →
  object keys alphabetize), exactly as hand-written op schemas already behave.
- **No live-path risk:** `OpSpec` was test-only; real flux-tools ops register hand-written `ToolSpec`s, so
  their planner signatures are unchanged (`flux-flow` tests + `skill_docs_in_sync` stayed green).
- Tests: `opspec_lowers_typed_inputs_to_a_named_json_schema`, `required_param_order_is_preserved_through_lowering`,
  `type_ref_to_schema_projects_each_variant` (flux-lang `opspec.rs`); `map_args_binds_through_a_lowered_opspec_schema`
  (flux-lang `runtime.rs`) — proves OpSpec → lower → `from_spec` → positional bind end-to-end.

## P1 — v1-core prelude types + cognition op-pack
- **Builds on P0:** each cognition op declares its inputs as typed, named `OpSpec`/`Param`s, so it gets a
  faithful planner signature + `properties`/`required` for free — no hand-written JSON. The `Named` prelude
  types resolve the `#/$defs/<name>` `$ref`s P0's `type_ref_to_schema` already emits.
- `flux-lang`: new `prelude` module — register the **v1-core** subset as `Named` type schemas:
  `Span/Claim/Evidence/Need/Ctx/Query/Answer/Blocked` + the coding types `Patch/TestResult` (handles reuse
  the existing `Thing`/`ThingRef` — no `Ref` type; `Value` already has a `Ref` variant). **Grow on demand**
  (don't ship yet): `Source/Chunk`, `Hypothesis`, `Decision/Verdict`. These are **new**, distinct from
  `flux-evidence::Observation` (a generic audit bag); optionally a produced `Evidence` is recorded into the
  `EvidenceLog`. No `Value`/`TypeRef` change.
- `flux-lang::schema`: add `prelude_type_catalog()` + `prelude_schema()` SSOT generator; add a drift test
  mirroring `node_kind_catalog_covers_every_variant`.
- Cognition ops, split by provider need:
  - **pure** (`need`/`gaps`/`compare`/`dedupe`/`sort`/`top`/`merge`/`cite`) → **flux-tools (L2)** via
    `ToolRegistry`; no provider. (`need` and `gaps` are symmetric pure ops — `need` is **not** a node.)
  - **model-backed** (`ai.extract`/`ai.rank`/`ai.judge`/`ai.reason`/`synth`/`ai.rewrite`) → a **new
    `flux-cognition` crate (L3)**: `CognitionPack::new(provider).register(&mut registry)`, each tool
    owning a `Box<dyn Provider>` for single-shot structured calls; `ToolContext` untouched. Register
    `"flux-cognition" => 3` in `crates/flux-codegate/src/lib.rs` `layer()` (or the layering lint fails).
  - **datasource** (`query`/`Repo.search`/`Read.many`/`Test.run`/`Repo.patch`) → keep as the existing
    `flux-datasource` (L5) / `flux-tools` ops surfaced at L6; **not** in the L3 cognition crate.
- **Keep the `task` op.** The cognition pack is *additive* — `task` (full sub-agent delegation via the
  spawner) stays for delegated multi-step work; the cognition ops do single-shot structured model calls.
  Both coexist. (Future direction: some IO/LLM ops may later become language primitives — **not yet**;
  v1 keeps them as registered ops.)
- Add `flux-flow/docs/ops-reference.md` rows + engine-skill table for the new ops.
- Tests: prelude schema round-trip; a cognition op's `OpSpec` lowers to the expected named schema (reusing
  the P0 projector); `gaps`/`compare` purity.

## P2 — `ctx` / `ctx_append` nodes (+2)
- `ast.rs`: **2** new `Node` variants (`ctx`, `ctx_append`) with doc-comments (so the node-kind SSOT
  regenerates). Frozen 29 stay. (`need`/`gaps` are pure ops from P1, not nodes.)
- `runtime.rs`: interpret `ctx` (resolve include/exclude → members, apply the declared `budget`) and
  `ctx_append` (immutable rebind + re-apply budget). Budget is enforced **at node evaluation, eagerly** —
  not at op dispatch (the interpreter is op-agnostic; op sigs carry no types, `opspec.rs:85-98`): shrink
  members by visibility then recency with a **heuristic char-based counter in v1**, record drops in the
  run trace, store a bounded `Ctx`. Consuming ops inline the already-bounded members at arg-resolution.
  No provider in L0.
- `store.rs`: the shrink reads per-symbol visibility/recency, so `ValueStore` likely needs a small
  binding-metadata accessor (the "couples to the symbol table" point); add it on the trait + `MemStore` +
  `FlowStore`.
- `analyze.rs`: validate `ctx`/`ctx_append` member references.
- Docs sync: `UPDATE=1 cargo test -p flux-lang --test skill_in_sync` + `UPDATE=1 cargo test -p flux-flow
  --test skill_docs_in_sync`; hand-write `reference.md` sections for `ctx`/`ctx_append`.
- Tests: a **build→append→budget-shrink** trace with drops recorded (so `ctx` isn't a glorified struct
  literal); a `need`→`gaps`→`repeat until $open.empty` loop (ops + existing nodes, no new machinery).

## P3 — SDK lifecycle surface (`flux-sdk`)
- Keep `ClientBuilder`/`Client` (agent-loop) as the simple front door.
- Add: `OpRegistry` + `register_op`/`register_pack`/`register_prelude`; re-expose `compile_turn`,
  `analyze`, `execute` (reuse flux-flow adapters — no new envelope); `optimize` stub.
- Artifact builders/readers for `Ctx`/`Need`/`Claim`/`Evidence`/`Patch`/`TestResult`; result readers for
  evidence-used / gaps-open / risks.
- `FlowClient` façade (provider + packs + compile→analyze→execute → structured artifacts).
- Runnable example + doctest; feeds the roadmap "SDK + crates.io" tier.

## P4 — richer analyze (typed HIR)
- `DraftAst → HirFlow` with type + effect checking (PRD item 3 / §10.2). Turns several STATUS rows 🟡→✅.

## P5 — parallel tracks (prioritize later)
- Text display modes + `parse.rs`/`format.rs` (PRD items 1–2): `=`/`do`/`+=` markers, `ctx`/`need`/
  `query` blocks, optional `goal` header; round-trip `parse(format(ast)) == ast`.
- Optimizer + `PhysicalPlan` execution (PRD §15).
- Multi-agent `Program` layer (`program.rs` decls + orchestration op-pack) → then the deferred
  **`flux-app`** L6 runtime host (event bus / scheduler / channels / supervisor); register in
  `flux-codegate` `layer()`; `flux run app.flux`.
- **Candidate control-flow primitives** (design §5.1): evaluate + selectively adopt `match`/`route`/
  `fallback`/`timeout`/`budget` (tier 1) and `checkpoint`/`compensate`/`once`/`scope` (tier 2); each
  adopted node goes through the node-kind SSOT + docs-sync gates.

## Cross-cutting gates
- Node-kind SSOT (+3) and the new prelude-type catalog must stay green (drift tests).
- Keep `flux-lang` L0-pure (no L1+ deps); cognition model-ops live in a registrable pack, not in the
  language core.
- Update `STATUS.md` rows as phases land.
