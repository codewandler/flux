---
id: A-47
title: TUI time-machine cockpit — scrub / step / branch a run visually (optional)
pillar: Agent
status: backlog
design: docs/designs/time-machine.md
epic: time-machine
note: "Time Machine Phase 4 (optional polish) — visual scrub/step/branch over a replayed run in the TUI; reuses UiEvent/Entry::Plan + the approval modal; UNBLOCKED (A-45/A-46 shipped 2026-07-07), pick up on demand — the CLI verbs are the product"
---

# TUI time-machine cockpit

## Goal
Make the Time Machine visual: scrub through a recorded run statement-by-statement in the TUI, step
its execution, and branch (fork) at the cursor — turning replay/fork from CLI commands into an
interactive cockpit. Optional polish phase; the CLI verbs are the product.

## Acceptance
- [ ] Extend `UiEvent` (`crates/flux-tui/src/lib.rs:1048`) with a replay frame and `Entry::Plan`
      (`lib.rs:146`) with a per-statement cursor; feed `flux replay --stream` frames into the
      existing live-stream renderer. No new rendering primitives (the plan tree + marker styling
      already exist).
- [ ] Key bindings: `←/→` scrub statements, `space` step, `b` branch — `b` invokes `fork_session`
      (A-46) at the cursor and opens the divergence modal, reusing the approval-modal seam
      (`lib.rs:1032`).
- [ ] Fails clearly without a TTY (parity with `flux tui`, D-24).
- [ ] Full gate green; layering intact.

## Progress
- (not started — optional; blocked on A-45 replay + A-46 fork)

## Notes
- Purely a surface; the envelope and determinism guarantees are unaffected. Cut freely if the CLI
  verbs prove sufficient.
