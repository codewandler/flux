---
id: D-01
title: Parameterized flow execution — the behaviour-runner seam
pillar: Agent
status: backlog
priority:
theme: downstream-managed-agents
design: docs/designs/flow-input-seeding.md
---

# Parameterized flow execution — the behaviour-runner seam

## Goal
Let a consumer run a **stored, validated** Flux-Lang flow **per invocation** with input *values* injected
at call time — so a downstream service can drive one flow as a reusable agent behaviour instead of
re-compiling from natural language or baking inputs into the AST. This is the deepest near-term
integration surface and the highest-leverage downstream item.

## Why (managed-agents)
The managed-agents **R-01 behaviour runner** + **A-03 preset framework** (their next milestone) must take a
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
- [ ] `FlowClient::parse(text) -> Result<DraftAst>` exists, wrapping the deterministic `flux_lang` parser
      (`crates/flux-lang/src/parse.rs`); no provider call. Failing-first test: parse a stored flow string
      → analyze → execute, asserting no `stream()` was hit.
- [ ] An input-seeding seam (e.g. `FlowClient::execute_with(ast, inputs)` + a `FlowStore` `seed`/
      `with_inputs` primitive) makes a flow's `$var` references resolve to injected values. Failing-first
      test: a flow returning `$greeting` yields the seeded value, with **no** literal in the AST.
- [ ] Custom ops still dispatch through the safety envelope (`Executor::dispatch`) unchanged.
- [ ] A hermetic example `crates/flux-sdk/examples/parameterized_flow.rs` injects a settings JSON object
      and reads structured output — no API key, no baked-in inputs.
- [ ] Full gate green (`cargo build/test/clippy/fmt`, `cargo test -p flux-codegate`).

## Progress
- Backlog. Design doc written: [`docs/designs/flow-input-seeding.md`](../designs/flow-input-seeding.md).
- Open design call captured there: whether multi-turn `await`/resume rides this path (via `FlowEngine`)
  or `execute_with` stays one-shot.

## Notes
- Reuse, don't reimplement: `flux_lang`'s `parse`/`format`, `flux_flow::state::FlowStore` (already holds
  values/symbols), `flux_flow::runtime::execute_flow`. The work is two thin additions + an example.
- Serves managed-agents stories **R-01** (behaviour runner) and **A-03** (presets as static flows).
- Non-goal: a config/secrets system or a new store — just per-run variable injection.
