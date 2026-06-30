---
id: D-01
title: Parameterized flow execution — the behaviour-runner seam
pillar: Agent
status: done
theme: downstream-managed-services
design: docs/designs/flow-input-seeding.md
note: "deterministic `FlowClient::parse` (no model round-trip) + a per-run input-seeding seam (`FlowStore::seed` + `FlowClient::execute_with`/`run_flow`) so a stored flow runs per invocation with injected `$var` settings — fresh-store isolation, flow-local binds shadow seeds, envelope unchanged; modules, zero new crates; serves downstream behaviour-runner/preset consumers (see [CHANGELOG](../../CHANGELOG.md))"
---

# Parameterized flow execution — the behaviour-runner seam

## Goal
Let a consumer run a **stored, validated** Flux-Lang flow **per invocation** with input *values* injected
at call time — so a downstream service can drive one flow as a reusable agent behaviour instead of
re-compiling from natural language or baking inputs into the AST. This is the deepest near-term
integration surface and the highest-leverage downstream item.

## Why (downstream managed services)
A downstream **behaviour runner** plus **preset framework** must take a
stored flow (custom or preset) and execute it with **effective settings** — author-time ⊕ validated
invocation-time JSON — threaded in, custom ops registered, control actions out. Today that is awkward:
`FlowClient::compile` is a model round-trip (wrong for a stored, already-valid flow), and the only way to
get inputs into a flow is to bake them in as `lit()` AST nodes — there is no per-run value-injection seam.

## flux gap
In `crates/flux-sdk/src/flow.rs`, `FlowClient` can register custom ops (`register_op` / `register_pack`)
and return structured output (`ExecutionResult`), but:
- there is **no deterministic `parse(text) → DraftAst`** — only `compile()` (a provider round-trip);
- there is **no way to seed input values** into the `FlowStore` symbol table before `execute`;
- `execute` is **one-shot** (a top-level `await` errors out — flow.rs:284).

## Acceptance
- [x] `FlowClient::parse(text) -> Result<DraftAst>` exists, wrapping the deterministic `flux_lang` parser
      (`crates/flux-lang/src/parse.rs`); no provider call. Test `parse_is_deterministic_no_provider_call`:
      parse a stored flow → analyze, asserting the mock provider consumed no reply.
- [x] An input-seeding seam — `FlowClient::execute_with(ast, inputs)` + a `FlowStore::seed` primitive —
      makes a flow's `$var` references resolve to injected values. Test `execute_with_seeds_a_var_no_literal`:
      a flow returning `$greeting` yields the seeded value, with **no** `lit` node in the AST.
- [x] Custom ops still dispatch through the safety envelope (`Executor::dispatch`) unchanged — test
      `custom_op_still_dispatches_through_the_envelope` (a seeded value reaches a registered op; a
      destructive op under the default `DenyApprover` is gated).
- [x] A hermetic example `crates/flux-sdk/examples/parameterized_flow.rs` injects a settings JSON object
      and reads structured output — no API key, no baked-in inputs (one stored flow, three invocations).
- [x] Full gate green (`cargo build/test/clippy/fmt`, `cargo test -p flux-codegate`) — 680 tests pass.

## Progress
- **Done.** Shipped as **modules, zero new crates**: `FlowStore::seed` (`crates/flux-flow/src/state.rs`)
  = `put_value(Value::from_json)` + `bind` as `Hidden` (resolves for `$name`, stays out of the model-facing
  `view`); `FlowClient::{parse, execute_with, run_flow}` (`crates/flux-sdk/src/flow.rs`) wrapping
  `flux_lang::parse::parse` and reusing `execute_flow` + the envelope verbatim.
- **Decisions made:** (1) **isolation** — `execute_with` runs against a **fresh per-run `FlowStore`**, so
  successive runs of one stored AST with different inputs can't leak symbols (test
  `execute_with_isolates_runs`). (2) **precedence** — a flow-local `bind` shadows a seed (last-writer-wins;
  test `a_flow_bind_shadows_a_seed`). (3) **multi-turn** — shipped **Option A** (one-shot `execute_with`;
  cross-turn state lives in the caller). Genuine top-level-`await` flows still belong on `FlowEngine`; not
  built here speculatively (left as a follow-up only on concrete demand).
- Built in an isolated worktree (`feat/d01-flow-input-seeding`), off the contested `main`.

## Notes
- Reuse, don't reimplement: `flux_lang`'s `parse`/`format`, `flux_flow::state::FlowStore` (already holds
  values/symbols), `flux_flow::runtime::execute_flow`. The work is two thin additions + an example.
- Serves downstream behaviour-runner and preset-as-static-flow use cases.
- Non-goal: a config/secrets system or a new store — just per-run variable injection.
