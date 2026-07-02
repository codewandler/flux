---
id: A-17
title: Revise wiring — the loop routes on $ran.failure; revision rendering with ✓-done prefix
pillar: Agent
status: ready
priority: 5
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: closes the epic loop — agent-loop.flux routes on failure kind/fatality in flux-lang (the loop stays the program), and the surface renders revisions honestly
---

# Revise wiring in the loop + revision rendering

## Goal
Connect the two tracks: the phased `agent-loop.flux` (A-14) consumes the structured failure
contract (A-16) — routing on `$ran.failure.kind` / `$ran.failure.fatal` as plain flux-lang — and
the surface renders the revise flow (`✗ step 4/9 edit failed — revising…`; a revised plan renders
with its reused prefix marked `✓ (done)` and only the new suffix live).

## Acceptance
- [ ] `agent-loop.flux` routes on `$ran.failure` (e.g. fatal → distinct feedback phrasing/stop-ask
      path; retryable → revise) — behavior pinned by an engine test:
      `loop_routes_fatal_halt_distinctly_from_retryable`.
- [ ] End-to-end revise: mid-plan failure → structured feedback → corrected re-emission →
      prefix fast-forward → completion, in one turn
      (`midplan_failure_revise_and_continue_completes_turn`, mock provider).
- [ ] CLI/TUI render the halt line and the ✓-done prefix on revised plans (extends A-15's
      rendering; snapshot tests).
- [ ] `flux why`/run-trace shows the true story: executed vs skipped vs failed per statement.
- [ ] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic.)

## Notes
- Depends on A-14 + A-16 (the two epic tracks join here).
- Loop text changes must keep phase-less compatibility (old ejected loops).
