---
id: D-146
title: Re-export sweep — one import for every public-signature type
pillar: Agent
status: done
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 1 closer — ends the 4-5-crate dependency scavenger hunt"
---

# Re-export sweep — one import for every public-signature type

## Goal
Enforce the design rule "if a type appears in any public SDK signature, the SDK re-exports it",
grouped so the crate root stays focused: `flux_sdk::{tools, approval, subagents, voice, observe}`
modules plus root-level `Provider`, `AgentSink`, `AgentSpec`, `Usage`, `CancellationToken`, ….

## Acceptance
- [x] `examples/custom_tool.rs` builds a function-tool and an `Approver` using ONLY `flux_sdk::`
      paths (`flux_sdk::tools::{tool_fn, ToolSpec}`, `flux_sdk::approval::{Approver, ApprovalChoice,
      IntentSet}`) — no direct `flux-runtime`/`flux-spec` import. Compiled by the gate
      (`--examples`), so a missing re-export fails CI. (`SubAgents` joins the proof in wave 2 when
      `Client::with_sub_agents` lands.)
- [x] Every type named in a wave-1 `pub fn` signature resolves under `flux_sdk::`: grouped modules
      `tools`/`approval`/`observe` + root `Provider`/`AgentSpec`/`Permissions`/`Usage`/`AgentSink`/
      `CancellationToken`; `EventStore`+`FlowStore` (for `Storage::custom`) under `observe`.
- [x] `#![warn(missing_docs)]` satisfied — each group carries a module doc naming its door.

## Progress
- 2026-07-11: implemented — grouped re-export modules `flux_sdk::{tools, approval, observe}` +
  root re-exports (`Provider`, `AgentSpec`, `Permissions`, `Usage`; `AgentSink`/`CancellationToken`
  already added in D-144). Removed the now-redundant internal `use` for AgentSpec/Usage/Provider
  (the `pub use` binds them locally too — E0252 on the first build, fixed). `subagents`/`voice`
  modules deferred to their waves (D-148/D-155). Proven by `examples/custom_tool.rs`.

## Notes
- `crates/flux-sdk/src/lib.rs` re-export blocks + `examples/custom_tool.rs`. Ran LAST in wave 1.
  Left the crate-level module-doc prose (`//! There are three front doors…`) untouched — the
  concurrent D-141 session owns that rewording.
