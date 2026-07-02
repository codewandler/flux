---
id: A-15
title: Phase-aware surface — loop.phase spinner labels, brief render, compact gather render
pillar: Agent
status: ready
priority: 5
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: CLI + TUI parity; observations already pass drain_event unfiltered so no plumbing change — pure rendering
---

# Phase-aware surface

## Goal
Make the phases visible: the spinner reads "orienting… / planning… / revising…" (`loop.phase`
observations emitted at `plan()` entry), the brief renders the moment it's accepted
(`flow.brief` → `◆ goal: …` + dim needs list), gather plans render as a compact one-liner
(`gathering · read Cargo.toml, src/lib.rs · grep "LoopHost"`), and execution plans keep the full
tree + risk badge.

## Acceptance
- [ ] Host emits `loop.phase {phase, round}` at `plan()` entry and `flow.brief` on brief acceptance;
      `flow.plan` observations gain `phase`. Failing-first test:
      `phase_observations_emitted_per_pass`.
- [ ] `CliSink` (`crates/flux-cli/src/main.rs`): phase-labeled spinner, brief render, compact
      gather render, full tree for execute-phase plans (snapshot-style render tests).
- [ ] flux-tui renders the same observations (parity pass).
- [ ] Machinery filtering unchanged: phases visible without `--show-loop` only via spinner/brief
      (observations), full loop internals still gated behind `--show-loop`.
- [ ] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic.)

## Notes
- Depends on A-14 (the observations exist). `drain_event` filters only machinery
  ToolCall/ToolResult (`engine.rs:939-950`) — observations flow already.
- Revision rendering (`✗ step 4/9 — revising…`, ✓-done prefix marks) lands with A-17.
