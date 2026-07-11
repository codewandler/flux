---
id: D-146
title: Re-export sweep — one import for every public-signature type
pillar: Agent
status: ready
priority: 5
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
- [ ] Failing-first: a doc-tested example implementing `Tool` + `Approver` and constructing
      `SubAgents` compiles with only `flux_sdk::` (plus a provider) imports — no direct
      `flux-runtime`/`flux-spec`/`flux-orchestrate` deps.
- [ ] Every type named in a `pub fn` signature of `flux-sdk` resolves under `flux_sdk::` (audit
      list in the design doc §Re-export rule; include `EventStore` for `Storage::custom`).
- [ ] `#![warn(missing_docs)]` satisfied — each re-export group has a module doc saying what it
      is for and which door uses it.

## Progress
- (pending)

## Notes
- `crates/flux-sdk/src/lib.rs` re-export blocks only; runs LAST in wave 1 (sweeps the final
  surface). Defer any module-doc prose overlapping the concurrent D-141 rewording.
