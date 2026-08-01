---
id: A-139
title: "The loop view must not become the bottleneck or the liar — bounded redraw, honest elision"
pillar: Agent
status: backlog
design: docs/designs/agent-loop-visibility.md
epic: agent-loop-visibility
areas: [flux-tui]
note: "a fast loop emits events faster than a terminal can usefully redraw, and sub-agent fan-out multiplies it. ⚠ The failure mode is a view that silently drops steps — which in a demo reads as flux doing less than it did"
---

# Fast enough, and honest when it is not

## Goal

The thread stays responsive and truthful under a fast loop and under sub-agent fan-out.

## Acceptance

- [ ] **Failing-first**: a test driving events far faster than a redraw budget and asserting the view
      neither blocks the run nor silently loses steps — failing at the merge base.
- [ ] Redraw is bounded. ⚠ Rendering must never apply backpressure to the loop: a UI that slows the
      agent down is a UI that changes what it is measuring.
- [ ] History is bounded, consistent with the existing `HISTORY_CAP` in
      `crates/flux-tui/src/projection.rs` rather than a second unrelated cap.
- [ ] ⚠ **Elision is stated in the output.** Dropping steps silently is the failure this story exists
      to prevent — in a demo it reads as flux having done less work than it did, which is the opposite
      of the epic's purpose.
- [ ] Sub-agent fan-out is covered, not just a single fast loop. `SpawnActivity` already multiplies the
      event rate by the fleet size.
- [ ] Full gate green.

## Notes

- Depends on [A-137](A-137-the-step-thread.md).
- ⚠ Related known trap: the session-screencast epic's [C-422](C-422-the-render-projection.md) found the
  TUI's durable→screen projection handles 5 observation kinds against 26 live variants. Different
  surface, same lesson — what the view shows and what happened are not automatically the same thing.

## Progress

- Filed 2026-08-01 with the agent-loop-visibility epic.
