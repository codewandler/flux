---
id: I-04
title: Terminal-bench containers run flux with the shell group disabled — enable it in the harness
pillar: Improve
status: ready
priority: 1
note: "found validating A-40: flux_agent.py forwards only provider keys, so in-container flux has no bash — the agent WRITES a correct server then says it cannot start it; every historical tbench number (I-01/I-03 both legs) is depressed by this"
---

# Terminal-bench containers run flux with the shell group disabled

## Goal
The tb custom agent (`crates/flux-eval/terminal_bench/flux_agent.py`) launches `flux run --yes`
inside the task container forwarding only provider keys (`_env` — ANTHROPIC/OPENAI/OPENROUTER/
FLUX_SECRET), so the off-by-default `shell` group never surfaces and the agent cannot run
`bash` — on fibonacci-server (A-40 live re-run, 2026-07-06) it wrote a correct `server.py` and
then honestly reported "Shell execution is disabled in this workspace (FLUX_ENABLE_BASH=1 not
set)". A terminal-bench container is a disposable, task-scoped sandbox whose whole point is
terminal work — the shell group should be ON there. This has depressed every containerized
tbench result to date (I-01 smokes, I-03 both legs) equally.

## Acceptance
- [ ] `flux_agent.py` sets `FLUX_ENABLE_BASH=1` for the in-container flux invocation, with a
      comment stating the trust rationale (disposable task sandbox; the envelope still gates each
      call — `--yes` auto-approves exactly as before).
- [ ] The setting is covered by a test (whatever seam exists — a python-side check or a Rust-side
      snapshot of the agent invocation/env; failing-first).
- [ ] Live verify (1 fibonacci-server trial, user-funded): the agent starts the server it wrote;
      checks > 0%.
- [ ] Decision recorded here: whether to re-baseline the I-03 comparison with shell on (both legs)
      or let future eval runs carry the corrected harness forward — do NOT silently mix old and
      new numbers.

## Progress
- 2026-07-06 filed — from the A-40 live re-run validation (see A-40's Progress for the transcript
  evidence and `bench/tbench-compare/results/a40-fix/`).

## Notes
- The dedicated toolchain ops (`python`/`node`/…) are also signal-gated and don't surface in a
  bare `/app` container at turn start — worth checking whether the task-workspace signals should
  be re-detected after a plan writes new files, as a separate observation.
- Related: A-40 (the discovery context), I-03 (the depressed comparison), I-01 (the headline-gain
  run that will benefit from the corrected harness).
