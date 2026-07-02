---
id: A-18
title: Multi-pass plan mode — read-only gather inside flux plan / REPL /plan
pillar: Agent
status: backlog
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: plan mode stays single-shot in the epic MVP; this brings gather to it — gather plans auto-run (non-mutating, same trust run_plan already grants), then the final execution plan is shown for approval
---

# Multi-pass plan mode

## Goal
Plan mode's contract is "show me the full plan before anything runs", which today forces the model
to plan blind (docs/usage.md: "Plan mode is single-shot per turn"). Let `flux plan` / REPL `/plan`
run read-only gather plans automatically (they are non-mutating — the same trust level `run_plan`
already grants without approval), then present the grounded final execution plan for approval.

## Acceptance
- [ ] `compile_once`/`plan_turn` accept the orient/gather contract; gather plans execute (read-only
      enforced per A-13), the final execution plan is shown and NOT run.
- [ ] Piped/`-o json|yaml` behavior stays print-and-exit (with gather having run).
- [ ] docs/usage.md plan-mode section updated (the "single-shot" caveat retired).
- [ ] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic; deliberately post-MVP.)

## Notes
- Depends on A-13/A-14 shipping and proving the gather contract in normal mode first.
