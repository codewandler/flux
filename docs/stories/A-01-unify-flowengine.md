---
id: A-01
title: Unify SDK onto FlowEngine, retire the classic Agent loop
pillar: Agent
status: backlog
priority:
design: docs/designs/flux-flow.md
---

# Unify SDK onto FlowEngine, retire the classic Agent loop

## Goal
Two turn loops coexist: the pure-DAG `FlowEngine` (used by CLI/TUI/server) and the classic
provider-native `flux-agent::Agent` (still driven by `flux_sdk::Client`). Unify the SDK onto
`FlowEngine`/`FlowClient` so there is **one loop everywhere**, then retire `flux-agent::Agent`. This
honors the project's "no fallbacks / clean cutover" principle and removes a silent divergence.

## Acceptance
- [ ] `flux_sdk::Client` runs on `FlowEngine` (or `FlowClient` becomes the single SDK door), with
      existing SDK behavior preserved (tests).
- [ ] `flux-agent::Agent` removed, or reduced to just the `AgentSink` streaming trait if still
      needed; the `flux-codegate` layering lint stays green.
- [ ] SDK examples (`crates/flux-sdk/examples/`) updated to the unified door.

## Progress
- (not started)

## Notes
- Background: `docs/designs/flux-flow.md` §11 ("deferred follow-ups") flags this as a separate
  decision.
- Files: `crates/flux-agent/src/lib.rs` (classic loop), `crates/flux-sdk/src/{lib.rs,flow.rs}`
  (the two doors), `crates/flux-flow/src/engine.rs` (FlowEngine).
