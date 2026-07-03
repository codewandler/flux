---
id: C-33
title: "Cost-surface follow-ups — app-run/journey turns, TUI unpriced parity, GoalSink live spec"
pillar: Core
status: backlog
epic: multipass-agent-loop
note: "C-30 wired every CliSink surface; still cost-less: `flux app run` journey/agent-target turns (no CliSink at all) and the TUI's unpriced-marker parity (its cumulative header would silently lie once any turn is unpriced — needs a `$?` state, flux-tui/src/lib.rs record_usage)"
---

# Cost-surface follow-ups

## Goal
Finish the cost-display coverage C-30 started: (a) `flux app run` / journey / agent-target turns
render no per-turn cost anywhere (the path has no CliSink; costs land only in `flux usage`); (b)
the TUI prices turns but silently skips table misses — once any turn is unpriced its cumulative
header total is a lie; give it the `$?` state (mirror flux-cli's `cost_suffix` rules,
`flux-tui/src/lib.rs` `record_usage`); (c) `/goal`'s GoalSink captures the spec once per turn —
fine today, but re-derive per iteration if `/model` becomes reachable mid-goal.

## Acceptance
- [ ] App-run/journey turn completions carry the same cost annotation contract (spec TBD with the
      app-run surface owner — the sink seam differs).
- [ ] TUI: a pricing-table miss on any turn switches the header cost segment to a `$?` state
      instead of silently under-reporting; test beside the existing cost test.
- [ ] Failing-first tests for each surface touched.

## Progress
- 2026-07-03 filed as the C-30 follow-up (scope decision: all CliSink surfaces in C-30; these
  non-CliSink surfaces deferred).
