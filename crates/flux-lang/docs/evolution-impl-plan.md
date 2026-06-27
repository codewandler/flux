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

## P1 — v1-core prelude types + cognition op-pack — ✅ DONE
- **Shipped:** prelude in `flux-lang/src/prelude.rs` (11 v1-core types incl. `Verdict`, `prelude_schema()`
  `$defs` + `prelude_type_catalog()` SSOT + reference/skill blocks). Pure ops (`need`/`gaps`/`compare`/
  `dedupe`/`sort`/`top`/`merge`/`cite`) in `flux-tools/src/cognition.rs` under a force-on `cognition`
  group. Model-backed pack (`ai.*`/`synth`) in the new **`flux-cognition`** L3 crate. NOTE: wiring the
  model pack into a live registry (provider+model) is **P3** — it is a dead crate until then.
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

## P2 — `ctx` / `ctx_append` nodes (+2) — ✅ DONE
- **Shipped:** `Node::Ctx`/`Node::CtxAppend` (29→31), `build_ctx`/`append_ctx` enforce the budget at
  node-eval (char heuristic, **priority-prefix** shrink so pinned is never dropped for a plainer member,
  `RunEvent::CtxShrunk` records drops, immutable append). `ValueStore::binding()` accessor (default over
  `view()`). SSOT/docs regenerated; tests cover budget shrink, no-budget keep-all, unbound tolerance, and
  append eviction.
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

## P3 — SDK lifecycle surface (`flux-sdk`) — ✅ DONE
- **Shipped:** `flux-sdk/src/flow.rs` — `assemble_registry(provider, model)` (builtins + `CognitionPack`),
  `FlowClient` (compile→analyze→execute), `register_op`/`register_pack`/`register_prelude`, artifact
  re-exports. **Cognition pack wired into the live CLI** (`flux-cli` `build_agent`) — the model ops are
  now advertised on the real path (resolves the Wave-1 dead-crate finding). `optimize` deferred (needs
  node-id plan lowering). Classic agent-loop client kept as the simple front door.

- Keep `ClientBuilder`/`Client` (agent-loop) as the simple front door.
- Add: `OpRegistry` + `register_op`/`register_pack`/`register_prelude`; re-expose `compile_turn`,
  `analyze`, `execute` (reuse flux-flow adapters — no new envelope); `optimize` stub.
- Artifact builders/readers for `Ctx`/`Need`/`Claim`/`Evidence`/`Patch`/`TestResult`; result readers for
  evidence-used / gaps-open / risks.
- `FlowClient` façade (provider + packs + compile→analyze→execute → structured artifacts).
- Runnable example + doctest; feeds the roadmap "SDK + crates.io" tier.

## P4 — richer analyze (typed HIR) — ✅ DONE
- **Shipped:** `analyze::lower(ast, ops) -> HirFlow` runs the whole-flow validation, gathers the
  semantic effect set (declared bind/memo effects ∪ host-op effects mapped to `FlowEffect`), and adds a
  **call-arity** check (`for_each_node` traversal covers all 36 kinds). Full type inference over
  expressions remains.

## P5 — parallel tracks (prioritize later)
- **P5a — ✅ DONE.** Text syntax (`src/parse.rs` + `src/format.rs`): canonical compact form, `=`/`do`/`+=` markers, indentation blocks, `ctx`/`ctx_append` native; `@json` fallback for the rest; `parse(format(ast)) == ast` for every DraftAst (round-trip + real-example tests). Remaining sub-items (optional): a second *token-efficient* display mode, native `need`/`query` block grammar (today `@json`), and wiring a `fluxlang compile` CLI subcommand onto `parse`.
- **P5b — ✅ DONE.** Optimizer (`src/optimize.rs`: independent read-only binds batch into Parallel stages, side-effects fenced) + `runtime::execute_plan` + `flux-sdk` `optimize`/`execute_optimized`.
- **P5c + FLUX-APP — ✅ DONE.** Multi-agent `Program` layer (`flux-lang/src/program.rs` decls + module
  loader) + the orchestration op-pack (`emit`/`send`/`spawn`; `ask` MVP) + the **`flux-app`** L6 runtime
  host (event bus / supervisor / channels; `flux-codegate` `layer() => 6`) + `flux run app.flux` wired in
  `flux-cli`. Journeys execute on the interpreter under the real `Executor` envelope; **safe default =
  destructive ops denied**, `--yes` opts into allow-all.
- **Candidate control-flow primitives** (design §5.1): evaluate + selectively adopt `match`/`route`/
  `fallback`/`timeout`/`budget` (tier 1) and `checkpoint`/`compensate`/`once`/`scope` (tier 2); each
  adopted node goes through the node-kind SSOT + docs-sync gates.

## P6 — capability batch: await + Tier-1 control-flow + polish — 🚧 IN PROGRESS
The next batch (directions 1 + 4 + 2). Each sub-phase ships behind the full dev loop + an adversarial
review, then commits; `STATUS.md` rows flip as each lands.

### P6a — `await` cross-turn suspend/resume (keystone) — ✅ DONE
- **Design: persist-continuation, top-level await only (v1)** — avoids re-running side effects; defers
  Tier-2 `checkpoint`/`once`. Cleaner than the original sketch: **no `Step::Suspend` variant** — await is
  intercepted by a top-level statement driver (`run_top_level`), not inside `exec_body`'s generic loop, so
  a suspend can only originate at a known top-level index. `exec_body`'s await arm stays an error (defends
  the nested / optimized-path cases the analyzer also rejects).
- **`flux-lang`:** `FlowOutcome.suspension: Option<Suspension { node, source }>`; `run_top_level` (shared by
  `execute_flow` and `resume_flow`) walks top-level statements — a top-level `await` records
  `RunEvent::Awaiting` and returns the suspension; `resume_flow(body, at, input, …)` binds `input` at `at`
  (lenient `as_type` coerce) and continues from `at+1`. `RunId` derived deterministically per session+node.
- **`flux-flow`:** `suspensions` table (`save_suspension`/`take_suspension`, one-shot, replace-on-resave,
  corrupt-body → graceful discard); engine `run_turn` resumes a pending suspension with the incoming
  message (bypassing the planner) and persists+ends the turn on suspend; `resume_flow` wrapper.
- **`analyze`:** `check_await_position` rejects `await` nested anywhere but top level (reuses `for_each_node`).
- **Adversarial review (keystone):** core confirmed sound (side-effect-once, symbol durability across the
  turn, multi-await, analyzer exhaustiveness, engine routes via `execute_flow` not `execute_plan`). Fixed:
  SDK `execute` now surfaces a suspension as an error (was silently dropping the remainder); corrupt
  persisted body recovers instead of wedging. **Accepted v1 limitations** (documented): a post-await op
  failure isn't retryable (no per-statement checkpoint); a pending await captures the next message with no
  escape/TTL (a REPL `/cancel` is the natural home); the optimized `execute_plan` path doesn't suspend.
- Tests: flux-lang suspend+resume (no prefix re-run, `Awaiting` emitted, `as_type` coercion), analyzer
  nested-await rejection, flux-flow `save`/`take` round-trip, and the **end-to-end engine two-turn resume**
  (`await_suspends_a_turn_then_the_next_message_resumes_it`).

### P6b — Tier-1 control-flow primitives (5 nodes, design §5.1) — ✅ DONE
- **Shipped:** `match` (exhaustive multi-way; subject = literal/symbol, JSON-equality), `route` (`!model`
  selector op → label → fixed analyzer-validated case; L0-pure), `fallback` (first-non-empty-success),
  `timeout` (wall-clock bound; sink buffered, real steps/transcript threaded so pre-deadline work stays
  counted+audited), `budget` (dispatch cap, checked at statement boundaries). **Tier-2 deferred**.
- Threaded the full add-a-node checklist (`ast` +5 variants +3 case structs / `runtime` exec+node_kind /
  `analyze` for_each_node+check_node+node_contains_return+type_check / `render` head+expr+children /
  flux-flow `walk_node`) + SSOT regen (36 node kinds). `optimize.rs` needed **no change** (the 5 are
  sequential barriers, never `readonly_bind`s). format/parse round-trip via the `@json` escape.
- Implemented in one coherent pass (the 5 share every exhaustive-match site, so parallel worktrees would
  conflict). Adversarial review fixed: match-subject must be a value (was analyze-pass/runtime-fail);
  timeout step/transcript accounting on a completed-then-timed-out body; fallback `≥1 branch` guard.
- Tests: per-node interpreter (`runtime.rs`), analyzer guard-rails (`analyze.rs`), text round-trip
  (`tests/text_roundtrip.rs`); node-kind catalog drift green.
- **Note for native grammar:** `parse.rs` already uses a `budget` keyword for `ctx`'s char-budget; resolve
  the collision before adding native (non-`@json`) syntax for `Node::Budget`.

### P6c — polish to truly-done — ✅ DONE (focus aliases deferred)
- **`fluxlang compile`** (P6c.1): text → JSON AST, mirrors `Render` (`bin/fluxlang.rs`). ✅
- **Token-efficient display** (P6c.2): `format::format_compact` — single-space block indentation,
  **display-only** (the parser is 2-space-sensitive, so it does not round-trip; `format` stays the
  writable form). ✅
- **Thing resolver** (P6c.3): `OpHost::resolve_thing` + `default_resolve_thing` (`host.rs`) resolve the
  self-identifying selectors (`Id`/`Key`, `File`-by-`Path`, `Url`) deterministically; `Node::Thing` binds
  the resolved value with a `ThingResolved` trace. `Name`/`Query` error pending a host resolver override
  (the injected `ThingResolver` for ambiguous lookups is the follow-up). ✅
- **Focus aliases** (P6c.4): **deferred** (the plan flagged it droppable). Rationale: the planner emits
  explicit `$symbol` names, not NL aliases — there is **no runtime consumer** for "the draft"/"those
  results", so a most-recent-by-type resolver would be dead code. Revisit when a surface needs NL focus
  resolution. Recorded in `STATUS.md` (§9.3).

## Cross-cutting gates
- Node-kind SSOT (+3) and the new prelude-type catalog must stay green (drift tests).
- Keep `flux-lang` L0-pure (no L1+ deps); cognition model-ops live in a registrable pack, not in the
  language core.
- Update `STATUS.md` rows as phases land.
