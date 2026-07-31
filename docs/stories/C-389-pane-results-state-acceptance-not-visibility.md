---
id: C-389
title: Pane results state acceptance, not visibility
pillar: Agent
status: backlog
epic: agent-change-recovery-and-provenance
design: docs/designs/agent-change-recovery-and-provenance.md
note: "the write-only contract is intentional and owned by C-306, but the results say \"pane 'x' open\"/\"updated\"/\"closed\" — accomplished-fact claims about screen state — while C-324 proves a dropped command still returns ok"
---

# Pane results state acceptance, not visibility

## Goal

Meet the condition the harness review actually set: while the channel is send-only, claims say
*accepted*, never *visible*.

## Acceptance

- [ ] The three result strings (`crates/flux-tools/src/surface.rs:376`, `:426`, `:469`) express
      command acceptance rather than accomplished screen state.
- [ ] A test pins the wording so a future edit cannot reintroduce an accomplished-state claim while
      the surface remains write-only.
- [ ] The op descriptions and the ops reference agree with the new wording.
- [ ] Independent of C-306: this lands whether or not a read-back contract is ever adopted, and does
      not wait on that decision.

## Progress

- 2026-08-01 — filed from validation of HAR-06. Read-back itself validates as a design-decision
  owned by C-306; the wording half is a live overclaim.

## Notes

- C-324 (`ready`) documents that `PaneQueue::emit` drops the newest command past
  `MAX_PENDING_COMMANDS` while the op still returns ok — so "updated" can be false today.
