---
id: D-143
title: Client envelope parity — custom tools, injected approver, tool subset
pillar: Agent
status: ready
priority: 2
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 1 — the FlowClient knobs the classic door is missing"
---

# Client envelope parity — custom tools, injected approver, tool subset

## Goal
Bring `ClientBuilder` to parity with `FlowClientBuilder` on envelope/registry concerns so the
classic conversational door supports the SDK table stakes: custom function tools, approval
callbacks, and tool subsetting — all still gated by the one envelope.

## Acceptance
- [ ] `ClientBuilder::{register_op, register_pack, tools, approver, with_cognition, from_spec}`
      exist; `approver` overrides `auto_approve`.
- [ ] Failing-first: a `tool_fn` registered on the builder is callable by a planned turn
      (mock planner emits a plan calling it) AND an injected deny-listing `Approver` blocks it
      (mirror `an_injected_approver_policy_gates_per_op`, `crates/flux-sdk/src/flow.rs:1165`).
- [ ] `tools(["read"])` hides `write` from the advertised catalog (catalog-capturing mock).
- [ ] `flux-spec` promoted from dev-dep to real dep; `FnTool`/`tool_fn`/`ToolSpec`/`Risk`
      nameable via `flux_sdk::tools` (full sweep is D-146).
- [ ] Shared builder state factored into a private `envelope.rs` used by both builders (no
      behavior change on `FlowClient` — its tests stay green).

## Progress
- (pending)

## Notes
- `crates/flux-sdk/src/lib.rs`, new `src/envelope.rs`, `crates/flux-sdk/Cargo.toml`.
- Registry mutations run before `AgentSpec::assemble` (which subsets + registers agent ops).
- Cargo.lock hunk must carry only this story's dep promotion (concurrent WIP in the lock).
