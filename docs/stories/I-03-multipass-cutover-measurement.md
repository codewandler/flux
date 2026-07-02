---
id: I-03
title: Measure the multi-pass cutover — time-to-first-feedback, rounds, tokens, tbench pass-rate
pillar: Improve
status: ready
priority: 6
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: the epic's acceptance gate — judged on evidence, not vibes; runs after the MVP stories land; baseline = pre-cutover main
---

# Measure the multi-pass cutover

## Goal
The epic is judged on measured evidence: capture a pre-cutover baseline, then compare the phased
loop on time-to-first-feedback, gather/revise rounds per turn, plans-per-turn (tiny-plan-dribble
regression watch), tokens/turn, prompt-cache hit rate (A-03 erosion watch), and terminal-bench
pass-rate. The agent never grades itself — graders are the eval adapters.

## Acceptance
- [ ] Baseline captured on pre-cutover main (same tasks/trials/model) and recorded before the
      comparison run.
- [ ] `PlanAttempt.phase` + C-15 efficiency projections report gather-rounds/turn and
      revise-rounds/turn (extend `efficiency_all` if needed).
- [ ] Time-to-first-feedback measured (planning-state timestamp → first rendered artifact) on a
      small fixed prompt corpus; before/after reported.
- [ ] Terminal-bench: same task set, trials ≥ 3, same model, pre vs post; strict comparison (no
      cherry-picking; regressions reported honestly).
- [ ] Results recorded in the epic design doc (and `docs/self-improvement/STATUS.md` if the loop is
      exercised); the cutover is only called done when this story is.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic.)

## Notes
- Sequenced after A-12..A-17 + L-22 land; listed ready so the board shows the full MVP set.
- Model for eval runs: `-m openrouter-anthropic/anthropic/claude-sonnet-4.6` (Anthropic key out of
  credits as of 2026-07-02).
