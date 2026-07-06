---
id: I-04
title: Terminal-bench containers run flux with the shell group disabled — enable it in the harness
pillar: Improve
status: done
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
- [x] `flux_agent.py` sets `FLUX_ENABLE_BASH=1` for the in-container flux invocation, with a
      comment stating the trust rationale (disposable task sandbox; the envelope still gates each
      call — `--yes` auto-approves exactly as before).
- [x] The setting is covered by a test (whatever seam exists — a python-side check or a Rust-side
      snapshot of the agent invocation/env; failing-first).
- [x] Live verify (1 fibonacci-server trial, user-funded): the agent starts the server it wrote;
      checks > 0%.
- [x] Decision recorded here: whether to re-baseline the I-03 comparison with shell on (both legs)
      or let future eval runs carry the corrected harness forward — do NOT silently mix old and
      new numbers.

## Progress
- 2026-07-06 filed — from the A-40 live re-run validation (see A-40's Progress for the transcript
  evidence and `bench/tbench-compare/results/a40-fix/`).

- 2026-07-06 implemented + live-verified, story closed:
  - `flux_agent.py` `_env` now sets `FLUX_ENABLE_BASH=1` with the trust-rationale comment
    (disposable task sandbox; envelope still gates each call; `--yes` semantics unchanged).
  - Failing-first test `adapters::terminal_bench::tests::container_agent_enables_the_shell_group`
    (flux-eval) pins the bundled python agent — confirmed FAILED pre-fix, green post-fix; whole
    flux-eval suite 43 passed, clippy/fmt clean, python syntax validated.
  - **Live verify (fibonacci-server x 1 trial, same model, fixed harness,
    `bench/tbench-compare/results/i04-verify/`): checks 0% -> 83%** (5/6 — the server the agent
    wrote now STARTS; only `test_negative_number` fails, a genuine agent-behavior edge case, not
    harness). 9 steps, 61.7s, $0.2149, zero truncation signatures (A-40 holding; C-35 caching
    likely helped the cost vs the $0.35 a40-fix trial).
  - **Re-baseline decision: carry the corrected harness FORWARD; do NOT re-run I-03.** Both I-03
    legs were equally depressed (shell off on both), so its relative verdicts (TTFF win, rounds
    parity, truncation regression -> A-40) stand; a re-baseline would spend ~$8-16 to change no
    decision. Result dirs are labeled (`i03-go` = shell-off era, `i04-verify` onward = shell-on),
    so numbers can't silently mix. Future runs (I-01 headline gain) use the corrected harness.

## Notes
- The dedicated toolchain ops (`python`/`node`/…) are also signal-gated and don't surface in a
  bare `/app` container at turn start — worth checking whether the task-workspace signals should
  be re-detected after a plan writes new files, as a separate observation.
- Related: A-40 (the discovery context), I-03 (the depressed comparison), I-01 (the headline-gain
  run that will benefit from the corrected harness).
