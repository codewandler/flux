# Flux-Lang — PRD conformance status (RTM)

**Purpose.** A living **requirements-traceability matrix** for the Flux-Lang
[PRD](PRD.md). The PRD is the immutable source-of-record (preserved verbatim); **this** file tracks how
much of it is actually built, and where. Keep it honest: every **Done** row cites a real file or test;
update it in the same commit as the behaviour it describes.

**Legend.** ✅ Done · 🟡 Partial · ⬜ Planned · ➕ New (beyond the original PRD; see
[`docs/designs/flux-lang-evolution.md`](../../../docs/designs/flux-lang-evolution.md)).

> Note: the implementation has intentionally grown **beyond** the PRD's "deliberately small" v1 node set
> (PRD §4/§8 list ~7 constructs; `ast.rs` ships **31**). That is a superset, not a regression.

## Language & AST (PRD §8, §10.1)

| PRD § | Requirement | Status | Evidence / note |
|---|---|---|---|
| 8, 10.1 | Draft AST + core node kinds (`flow`/bind/call/thing/branch/repeat/await/return/effect) | ✅ | `src/ast.rs` — 31 `Node` kinds |
| 8 | Constructs beyond v1 (`each`/`parallel`/`race`/`try`/`retry`/`confirm`/`loop`/`throttle`/…) | ✅ | `src/ast.rs`, `src/runtime.rs` |
| 8 | `await` pause/resume | 🟡 | node exists; interpreter rejects it (cross-turn suspend unbuilt) |
| 1, 8 | Compact **text parser** (text → AST, auto-detect forms) | ⬜ | `parse.rs`/`format.rs` are a "Toolchain plan" in `syntax.md` |
| 8, 16 | Pretty-printer / renderer (AST → readable) | ✅ | `src/render.rs` (one-way) — round-trip blocked on the parser |
| 20.1 | AST serializable + versioned (JSON wire) | ✅ | serde on `ast.rs`; `examples/*.flux` are JSON |

## Analyzer & lowering (PRD §10.2, §15)

| PRD § | Requirement | Status | Evidence / note |
|---|---|---|---|
| 10.2, 20.1 | Name resolution + unknown-op rejection | ✅ | `src/analyze.rs` |
| 8 | Bounded-loop checking | ✅ | `src/analyze.rs` |
| 10.2 | **Type checking** + `DraftAst → HirFlow` lowering | 🟡 | `HirFlow` is a stub (carries body + gathered effects only) |
| 12 | Effect gathering | ✅ | `src/effects.rs`; effects collected on `HirFlow` |
| 10.3, 15 | **Optimizer** (parallelize/cache/CSE/…) + `PhysicalPlan` execution | ⬜ | `Stage` types exist (`ast.rs`); nothing executes them |

## Runtime, store & events (PRD §9, §14.3, §19, §20.2–20.3)

| PRD § | Requirement | Status | Evidence / note |
|---|---|---|---|
| 19, 20.3 | Interpreter (bind/call/when/repeat/return + more) | ✅ | `src/runtime.rs` |
| 9.2, 20.2 | Immutable value store; outputs stored as values by id | ✅ | `flux-flow/src/state.rs` (`FlowStore`, SQLite) |
| 9.3 | Symbol table + visibility tiers (visible/hidden/pinned/expired/private) | ✅ | `state.rs`; `Visibility` |
| 9.3 | Focus aliases ("the draft", "those results") | 🟡 | symbol resolution present; explicit focus set thin |
| 9.1 | Thing references + deterministic resolver | 🟡 | `ThingRef`/`Thing` node in AST; resolver interface thin |
| 14.3, 20.3 | Immutable replayable run trace (`RunEvent`) | ✅ | `RunEvent` in `ast.rs`; appended by `FlowStore` |
| 20.2 | Old value versions remain addressable (audit/undo) | ✅ | value-id revisions in `state.rs` |

## Operations, effects & policy (PRD §11, §12, §14)

| PRD § | Requirement | Status | Evidence / note |
|---|---|---|---|
| 11 | Op registry + `OpSpec` + handler trait (effects/retry/idempotency/approval/cache) | ✅ | `src/opspec.rs`; `OpHost`/`OpCatalog` seams |
| 11 | **Op-input JSON Schema** from `OpSpec` | ✅ (P0) | `OpSpec`'s typed, **named** `inputs: Vec<Param>` project to a real object schema (`properties`/`required`) via `OpSpec::lower()`/`input_schema()` (`opspec.rs`); `schema_params`/`OpSignature` read it back. Tests: `opspec_lowers_typed_inputs_to_a_named_json_schema`, `required_param_order_is_preserved_through_lowering`, `map_args_binds_through_a_lowered_opspec_schema` |
| 12, 14.2 | Effects first-class; policy allow/deny/approval before side effects | ✅ | `src/effects.rs` → `flux-policy`; `Executor::dispatch` |
| 14.1 | Prompt-injection resistance (analyzer-validated AST; external content is data) | ✅ | one envelope; no-bypass tests in `flux-runtime` |
| 14.2 | Dangerous effects (Delete/Money) denied by default | ✅ | default-deny policy |

## Context management (PRD §13)

| PRD § | Requirement | Status | Evidence / note |
|---|---|---|---|
| 13 | Symbolic session view (no full outputs/secrets/log to model) | ✅ | `SessionView` projection in `state.rs` |
| 13 | Per-step dependency slice | 🟡 | implicit/global today |
| 13 | **Explicit, budgeted context packs** (`ctx`/`ctx_append`) | ✅ (➕) | built: `src/ast.rs` (`Ctx`/`CtxAppend`), `src/runtime.rs` (`build_ctx`/`append_ctx`, budget@node-eval, `CtxShrunk` trace) |

## Public API & SDK (PRD §17)

| PRD § | Requirement | Status | Evidence / note |
|---|---|---|---|
| 17.1 | `compile_turn(text, view, registry, llm) -> DraftAst` | ✅ | surfaced via `flux-sdk` `FlowClient::compile` (delegates to `flux-flow/src/compile.rs`) |
| 17.2 | `analyze(ast, session, registry, policy) -> HirFlow` | 🟡 → ✅ | `analyze::lower` produces a typed `HirFlow` (effects + arity; full type inference later); `FlowClient::analyze` |
| 17.3 | `optimize(hir) -> PhysicalPlan` | ⬜ | needs node-id'd plan lowering (deferred) |
| 17.4 | `execute(plan, session, runtime) -> ExecutionResult` | ✅ | `FlowClient::execute` over `execute_flow` |
| 17.5 | `register_op` / `register_pack` / `register_prelude` | ✅ | `flux-sdk` `flow` module |
| 17 | **`flux-sdk` exposes the lifecycle** (not the agent loop) | ✅ (P3) | `FlowClient` (`flux-sdk/src/flow.rs`): assemble registry (builtins + **cognition pack**) + compile→analyze→execute + artifact helpers; classic loop kept as the simple front door |

## UI editor model (PRD §16)

| PRD § | Requirement | Status | Evidence / note |
|---|---|---|---|
| 16 | Graph projection from AST/HIR; node inspector; trace-to-node mapping | ⬜ | only ASCII `render.rs` today |

## Example operation packs (PRD §4/§7, §19)

| PRD § | Requirement | Status | Evidence / note |
|---|---|---|---|
| 7.1 | Slot-filling pack | ✅ (➕) | `need`/`gaps` pure ops in `flux-tools/src/cognition.rs` |
| 7.2 | KB / FAQ (evidence + grounding) pack | ✅ (➕) | artifact ontology (`prelude.rs`) + `synth`/`ai.*` in `flux-cognition` |
| 11 | **Cognition op-pack** (`ai.extract`/`rank`/`judge`/`synth`/`gaps`) | ✅ (➕) | pure ops in `flux-tools/src/cognition.rs`; model-backed in `flux-cognition` (L3) |

## Near-term roadmap (PRD §0)

| Item | Status |
|---|---|
| 1. Two writable display modes (human + token-efficient) | ⬜ (grammar designed; parser deferred) |
| 2. `fluxlang compile` (text → AST, auto-detect) | ⬜ |
| 3. Richer `analyze` (type + effect checking → typed HIR) | 🟡 |
| 4. Op-input JSON Schema from `OpSpec` | ✅ (P0; `opspec.rs`) |

## Beyond the PRD — this design's additions (➕)

| Addition | Status | Where / evidence |
|---|---|---|
| Artifact-type ontology (v1-core: `Span`/`Claim`/`Evidence`/`Need`/`Ctx`/`Query`/`Answer`/`Blocked`/`Patch`/`TestResult`/`Verdict`; reuses `Thing`; **new**, distinct from `flux-evidence::Observation`) | ✅ | `src/prelude.rs` (+ `$defs` via `prelude_schema()`; SSOT `prelude_type_catalog()`) |
| First-class context packs (`ctx`/`ctx_append`; budget enforced at **node evaluation**, eager, char heuristic, priority-prefix shrink) | ✅ | `src/ast.rs`, `src/runtime.rs` (`build_ctx`/`append_ctx`) |
| Needs & gaps — **two pure ops** (`need`/`gaps`, not nodes) | ✅ | `flux-tools/src/cognition.rs` |
| Cognition op-pack + domain-wrapper convention | ✅ | pure: `flux-tools/src/cognition.rs`; model-backed: `flux-cognition` (L3) |
| `=`/`do`/`+=` marker syntax; optional `goal` header | ⬜ | evolution §5 (P5 text parser) |
| Multi-agent `Program` layer (agents/channels/triggers/journeys) + `flux-app` host | ⬜ | evolution §6 + appendix (P5c + flux-app) |
| Real `flux-sdk` lifecycle surface (`OpRegistry`/packs/prelude + `FlowClient` + artifact APIs) | ✅ | `flux-sdk/src/flow.rs`; cognition pack wired into the CLI registry too (`flux-cli` `build_agent`) |

## Key design decisions (resolved this round)

- **Model-op seam.** Model-backed cognition ops (`ai.*`, `synth`) live in a new provider-injected pack
  **`flux-cognition` (L3)**; **pure** ops (`gaps`/`compare`/`sort`/…) live in **flux-tools (L2)**;
  `ToolContext` is untouched (it has a `spawner`, no provider). Datasource verbs stay the existing
  L5 ops surfaced at L6. The cognition pack is **additive — the `task` op stays** (delegated sub-agent
  work alongside single-shot cognition calls); promoting any IO/LLM op to a language primitive is a
  **later** direction, not v1. (`flux-lang-evolution.md` §3.4)
- **Context budget.** Enforced **at `ctx`/`ctx_append` node evaluation** (eager): the node resolves
  members, shrinks by visibility→recency to the declared budget, records drops; consuming ops then get the
  already-bounded pack. Heuristic char-based counter in v1. No type-carrying op signatures needed. (§3.2)
- **`need`/`gaps` are pure ops, not nodes** (review #2): `need` only builds a `Need` value and the loop is
  ordinary control flow, so it stays symmetric with `gaps`. (§3.3)
- **Op-input JSON Schema (P0) — done.** The cross-cutting `OpSpec::lower()` rework shipped first, ahead of
  the prelude/pack (review #3). `OpSpec.inputs` became `Vec<Param>` (`Param { name, type, optional }`) so
  the typed inputs project to a faithful JSON Schema object: **required-param order is preserved** (the
  `required` array; load-bearing for positional binding), optional params carry no order guarantee (object
  keys are unordered — `serde_json` has no `preserve_order`), matching hand-written op schemas. `Named`
  types render as `#/$defs/<name>` `$ref`s, forward-compatible with the P1 prelude. Existing flux-tools ops
  are unaffected (they register hand-written `ToolSpec`s, not via `OpSpec`). (§3.4, §8; `opspec.rs`)
- **Additive AST.** **+2** node kinds (`ctx`/`ctx_append`); no `Value`/`TypeRef`/effect change;
  backward-compatible (existing JSON flows unaffected).
