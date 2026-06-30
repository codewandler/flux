# Design: parameterized flow execution (input seeding)

**Status:** implemented (story [D-01](../stories/D-01-flow-input-seeding.md)) · **Layer:** L5
(`flux-sdk`), reusing L1–L2 (`flux-lang`, `flux-flow`) · **Owner:** Timo

> **As built.** Shipped as the two thin additions + a store primitive this design proposed, zero new
> crates. `FlowStore::seed(session_id, name, value)` (`crates/flux-flow/src/state.rs`) = `put_value`
> (via `Value::from_json`) + `bind` as `Visibility::Hidden` — so a seed resolves for `$name` (the
> interpreter's `resolve` is visibility-agnostic) but never shows in the model-facing `view`.
> `FlowClient::{parse, execute_with, run_flow}` (`crates/flux-sdk/src/flow.rs`) wrap `flux_lang::parse`
> and reuse `execute_flow` + the envelope verbatim (a shared `finish_outcome` helper keeps `execute`
> and `execute_with` from drifting). Resolved decisions: **isolation** = a **fresh per-run `FlowStore`**
> (the "use a fresh store view per run" option below); **precedence** = a flow-local `bind` shadows a
> seed (recommended default); **multi-turn** = **Option (A)**, one-shot — genuine top-level-`await` flows
> still belong on `FlowEngine`, not built here speculatively.

## Why

A downstream multi-tenant service needs to run a **stored, validated**
Flux-Lang flow as a reusable agent *behaviour*: parse it once, then execute it **per invocation** with
**effective settings** injected — author-time settings merged with validated invocation-time settings,
as a plain JSON object — while registering its own custom ops and reading structured output back. The
current `flux-sdk` surface can register ops and return structured results, but the two ends of "run this
specific flow with these inputs" are missing:

1. **No deterministic text → AST.** `FlowClient::compile(text, view)`
   ([`crates/flux-sdk/src/flow.rs:250`](../../crates/flux-sdk/src/flow.rs)) is a *model* round-trip
   (prompt-and-parse via the provider). For a stored, already-valid flow that is wrong: it costs a model
   call and is non-deterministic. The language already ships a deterministic parser —
   `flux_lang`'s `parse` ([`crates/flux-lang/src/parse.rs:26`](../../crates/flux-lang/src/parse.rs)) — but
   `FlowClient` doesn't expose it.
2. **No per-run input seeding.** `FlowClient::execute(ast)` (flow.rs:273) runs the AST against
   `self.store` (a `flux_flow::state::FlowStore` holding values/symbols). There is no way to put named
   values into that store *before* the run, so the only way to get input into a flow today is to bake it
   into the AST as a `lit()` node. A behaviour runner can't bake per-call settings into a shared AST.

This design adds those two seams — nothing else. The safety envelope, analyzer, store, and op-dispatch
path are reused verbatim.

## Current surface (what we build on)

In [`crates/flux-sdk/src/flow.rs`](../../crates/flux-sdk/src/flow.rs):
- `FlowClient::compile(text, view) -> DraftAst` — model NL→AST (kept; the authoring path).
- `FlowClient::analyze(ast) -> Result<(), Vec<Diagnostic>>` — catalog resolution.
- `FlowClient::execute(ast) -> ExecutionResult` — dispatch through `Executor` under the client's
  permission rules + approver; one-shot (a top-level `await` is surfaced as an error, flow.rs:284).
- `FlowClient::register_op(Arc<dyn Tool>)` / `register_pack(F)` — custom-op registration into the
  assembled `ToolRegistry`.
- `ExecutionResult { result, transcript, steps, tool_calls }` with `.parse::<T>()` / `.answer()` for
  typed artifact readback.

Underneath: `flux_flow::runtime::execute_flow(&store, &executor, &session_id, ast, &mut sink)` and the
`FlowStore` symbol table.

## Proposed API

Two thin additions on `FlowClient`, plus a seeding primitive on `FlowStore`.

```rust
impl FlowClient {
    /// Deterministic text → AST for a stored/validated flow (no provider round-trip).
    /// Thin wrapper over `flux_lang`'s `parse`; mirrors the model-path `compile`.
    pub fn parse(&self, text: &str) -> Result<DraftAst>;

    /// Execute `ast` with `inputs` seeded as flow variables before the run.
    /// Each (name, value) is visible to the flow as `$name`. Same envelope as `execute`.
    pub async fn execute_with(
        &self,
        ast: &DraftAst,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<ExecutionResult>;
}
```

Seeding primitive on the store (the actual new mechanism — everything else delegates):

```rust
impl FlowStore {
    /// Pre-bind a named value into the symbol table so a flow's `$name` resolves to it.
    pub fn seed(&self, name: &str, value: serde_json::Value) -> Result<()>;
}
```

`execute_with` = seed each input into a per-run store view, then call the existing `execute_flow`. A
`run_flow(text)` convenience (`parse → analyze → execute_with`) can follow, parallel to today's `run`.

## Semantics

- **Binding.** A seeded `name` populates the same symbol namespace flow-local `bind`s use, so `$name`
  references resolve to the injected value. Seeding happens **before** execution, so the flow sees inputs
  as already-bound variables.
- **Precedence.** A flow-local `bind` to the same name *shadows* a seed (the flow can override its
  inputs); document this explicitly and test it. (If we instead want seeds to be immutable inputs, that's
  a one-line policy choice — call it out at implementation time. Recommended default: flow-local binds
  win, matching ordinary lexical shadowing.)
- **Missing / extra.** A flow that references an unseeded `$name` fails the same way it does today for an
  unbound var (analyzer/runtime error). Extra seeds not referenced by the flow are harmless (ignored).
- **Envelope unchanged.** Ops still dispatch through `Executor::dispatch` under the client's permission
  rules + approver. Seeding injects *data*, never bypasses a capability check.
- **Isolation.** Seeds apply to a single run; concurrent/successive invocations of the same stored AST
  with different settings must not leak symbols between runs (use a fresh store view per run, or clear
  seeded names after). This is the one correctness-sensitive point — cover it with a test that runs the
  same AST twice with different inputs.

## Multi-turn decision

Downstream conversations (RTVBP voice, A2A) are multi-turn, but `FlowClient::execute*` is one-shot — a
top-level `await` is rejected (flow.rs:284). Two options:

- **(A) Keep `execute_with` one-shot.** Each turn = one `parse`-once + `execute_with(settings)` call;
  cross-turn state lives in the caller (the downstream service owns the conversation/session). Simplest; matches how
  the behaviour runner threads *effective settings* per call anyway.
- **(B) Route multi-turn through `FlowEngine`.** For flows that genuinely `await` across turns, the
  engine (not the one-shot SDK path) already supports suspend/resume; expose a seeded entry there.

**Recommendation:** ship **(A)** for D-01 — it covers the R-01 behaviour-runner need (parameterized,
per-invocation runs) with the least surface. Leave (B) as a follow-up only if a preset actually needs
top-level `await`; note it in D-01's Progress rather than building it speculatively.

## Worked example (`crates/flux-sdk/examples/parameterized_flow.rs`)

Hermetic (no API key): register a custom op, `parse` a tiny stored flow that references `$greeting` and
`$name`, `execute_with` a settings object, and read the structured result — proving inputs flow in
**without** any `lit()` in the AST. Mirrors the existing `faq_lookup.rs` / `intent_routing.rs` examples'
shape (custom op + analyze + execute + typed readback).

## Non-goals

- Not a config or secrets system — `inputs` is plain run data; secrets still go through the envelope.
- Not a new store or a new execution engine — `FlowStore` + `execute_flow` are reused unchanged.
- Not changing how ops are written — custom ops still implement `flux_runtime::Tool`.
- Not the multi-tenant event substrate (that's [D-02](../stories/D-02-tenant-event-substrate.md)).
