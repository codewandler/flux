---
id: I-05
title: Sharpen the improve round — stable scored task set, severity-ordered planner picks
pillar: Improve
status: backlog
note: "ON HOLD (user call 2026-07-06, after round 3) — resume by implementing the two queued fixes below, then fund round 4; the 2026-07-06 funded round proved the machinery and exposed the two odds-killers: chess-best-move is too flaky to score (vision + tb-registry 429s; baseline swung 28↔42%), and the planner skipped the reviewer's severity-5 candidate"
---

# Sharpen the improve round

## Goal
Round quality, not machinery, now blocks I-01's headline gain. Two measured causes from the
2026-07-06 round (STATUS.md journey entry 6): (a) chess-best-move in the scored set adds
vision-dependent variance and tb-registry flakiness — the cross-round baseline swung 28↔42%
while fibonacci-server sat at a rock-stable 83% checks with clear headroom
(`test_negative_number`); (b) the planner ignored the top-weighted candidate even though
`improvements_aggregate` ranks by weight. Make the scored substrate stable and the planner
consume the ranking, so the next funded round measures the change instead of the noise.

## Acceptance
- [ ] The improve flow's scored eval set is fibonacci-server × 5 trials (both legs) — stable
      baseline, real headroom, no vision/registry variance; chess-best-move's exclusion is
      recorded with the evidence. `flows_validate` stays green.
- [ ] The planner prompt instructs weight-order consumption and per-task candidate attribution
      ("name the candidate id each task addresses").
- [ ] `bench/run-tbench-loop.sh` gains an operator env knob for the in-container eval model
      (`FLUX_IMPROVE_EVAL_MODEL`) so openrouter routing no longer needs hand commits on the loop
      branch.
- [ ] A funded round runs on the sharpened setup; outcome recorded here + STATUS.md (keep OR
      revert — the round must measure cleanly either way).

## Progress
- 2026-07-06 filed + implementation started (same session as the proving round).

- 2026-07-06 (round 3, the sharpened setup) — infra worked (stable fibonacci×5 substrate,
  FLUX_IMPROVE_EVAL_MODEL knob, ranked-planner prompt), but a THIRD chain defect surfaced: the
  planner answered in PROSE ("I'll look up the exact system prompt text before writing the…")
  instead of the bare JSON array, so `change_implement` extracted **0 tasks** and the candidate
  leg measured an unchanged tree (tie → correct revert; verdict record in this round's
  improve-log). QUEUED NEXT STEPS (implement before funding round 4):
  1. Flow guard: `when implemented == 0` → skip the candidate eval entirely (a no-op candidate
     can never beat baseline; the leg is pure spend), record the null round, stop.
  2. Planner seam hardening: planner role prompt gets "your FINAL message must be ONLY the JSON
     array — no prose"; `change_implement` names non-empty-but-unparseable `tasks` input in its
     view (mirror the improvements_aggregate hardening from `fbec793`).
- 2026-07-06 — **ON HOLD** (user priority call): machinery proven, three chain defects found+
  fixed or queued, headline gain not yet attempted on a fully-hardened chain. Resume here.

## Notes
- Loop-development work on `main` under the standing invariants (integrity/validity/
  target-clarity) — none of these changes touch scoring strictness, trials floor, or PROTECTED
  enforcement. The eval set change alters what is measured, not how strictly; recorded here so
  cross-era numbers are never silently mixed (fibonacci-only era starts with this story).
- Evidence: `docs/self-improvement/STATUS.md` journey entry 6; the round record in the
  `improve-tbench/20260706-130553` branch's `improve-log.jsonl`.
