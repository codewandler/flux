---
id: L-23
title: Streaming plan-emission render — plan skeleton appears while emit_plan streams
pillar: Language
status: backlog
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: deliberately sequenced AFTER L-20's emission-arm decision — if native-text wins, streaming render is nearly free (text deltas already stream); if strict JSON wins, it needs incremental JSON parsing in stream_blocks + a plan_delta sink method. Don't build it twice.
---

# Streaming plan-emission render

## Goal
Render node headlines of the plan as `emit_plan` arguments stream in, so a large execution plan is
visible while it is being composed instead of appearing only when complete.

## Acceptance
- [ ] Plan skeleton (per-node headline) renders progressively during emission on the winning
      emission arm; final render identical to today's tree.
- [ ] No regression to the repair loop (partial/invalid stream still resolves to the same
      rejection/repair behavior).
- [ ] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic; blocked on L-20's decision.)

## Notes
- Prereq: L-20 (emission A/B measured, ready on the board). Touchpoints: `stream_blocks`
  (`crates/flux-flow/src/compile.rs:638-655`), sink protocol, CliSink/TUI render.
