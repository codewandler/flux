---
id: I-05
title: Sharpen the improve round — stable scored task set, severity-ordered planner picks
pillar: Improve
status: done
note: "ON HOLD + DE-PRIORITIZED (user call 2026-07-06; focus shifts to hardening/docs/cleanup after v0.2.23) — resume by implementing the two queued fixes below, then fund round 4; the 2026-07-06 funded round proved the machinery and exposed the two odds-killers: chess-best-move is too flaky to score (vision + tb-registry 429s; baseline swung 28↔42%), and the planner skipped the reviewer's severity-5 candidate"
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
- [x] The improve flow's scored eval set is fibonacci-server × 5 trials (both legs) — stable
      baseline, real headroom, no vision/registry variance; chess-best-move's exclusion is
      recorded with the evidence. `flows_validate` stays green. _(Landed: `improve-tbench.flux`
      scores `fibonacci-server` only at `trials: 5` on both the baseline and candidate legs.)_
- [x] The planner prompt instructs weight-order consumption and per-task candidate attribution
      ("name the candidate id each task addresses"). _(Landed: the `improve-tbench.flux` planner
      template — "RANKED by measured weight — take them in order … every task must name the
      candidate id it addresses in an `addresses` field".)_
- [x] `bench/run-tbench-loop.sh` gains an operator env knob for the in-container eval model
      (`FLUX_IMPROVE_EVAL_MODEL`) so openrouter routing no longer needs hand commits on the loop
      branch. _(Landed: `bench/run-tbench-loop.sh` rewrites the flow model when
      `FLUX_IMPROVE_EVAL_MODEL` is set.)_
- [ ] A funded round runs on the sharpened setup; outcome recorded here + STATUS.md (keep OR
      revert — the round must measure cleanly either way). **← the one open item; blocked on a
      funded round (see below), which is why this story stays `backlog`.**

## Progress
- 2026-07-29 — **closed as already-shipped board drift, not new work.** Every Acceptance item was
  already ticked with a "(Landed: …)" note, but `status` still read `backlog`. Verified all three
  against the tree rather than trusting the ticks: `examples/improve-tbench.flux` scores
  `fibonacci-server` only at `trials: 5` on **both** legs (`:20`, `:223`) with zero remaining
  `chess-best-move` references; the planner template carries the "RANKED by measured weight"
  instruction and the `addresses` field; and `bench/run-tbench-loop.sh:36` honours
  `FLUX_IMPROVE_EVAL_MODEL`. The implementing commit is `c5943b5c` and CHANGELOG already carries
  the entry, so only the frontmatter was stale.
- 2026-07-06 filed + implementation started (same session as the proving round).

- 2026-07-06 (round 3, the sharpened setup) — infra worked (stable fibonacci×5 substrate,
  FLUX_IMPROVE_EVAL_MODEL knob, ranked-planner prompt), but a THIRD chain defect surfaced: the
  planner answered in PROSE ("I'll look up the exact system prompt text before writing the…")
  instead of the bare JSON array, so `change_implement` extracted **0 tasks** and the candidate
  leg measured an unchanged tree (tie → correct revert, base 667 = cand 667 on the fibonacci×5
  substrate; full planner output preserved in the round's improve-log). SHARPER POST-MORTEM from
  the final record: the planner DID end with a valid 2-task JSON array — but only after a
  **hallucinated tool transcript** (fake `<tool_call>`/`<tool_response>` blocks "reading"
  `crates/flux/src/lib.rs`, a file that does not exist, with a fabricated DEFAULT_SYSTEM_PROMPT),
  and `extract_array`'s first-`[`…last-`]` fallback latched onto a `#[cfg(test)]` bracket inside
  the fake transcript → parse fail → 0 tasks. Even if extracted, both tasks target the
  nonexistent file. QUEUED NEXT STEPS (implement before funding round 4):
  1. Flow guard: `when implemented == 0` → skip the candidate eval entirely (a no-op candidate
     can never beat baseline; the leg is pure spend), record the null round, stop.
  2. Planner seam hardening, three parts: (a) role contract — FINAL message must be ONLY the JSON
     array, and fabricated tool transcripts are forbidden (the planner role has no tools — give
     it read/grep so it can GROUND file paths instead of inventing them, or forbid file-level
     detail); (b) `extract_array` gains a last-`[`-first tail scan (an LLM's answer array is at
     the END; the current first-`[` heuristic is bracket-trapped by prose/code); (c)
     `change_implement` validates each task's `files` exist before counting it implementable and
     names non-empty-but-unparseable `tasks` input in its view (mirror `fbec793`).
- 2026-07-06 — **ON HOLD** (user priority call): machinery proven, three chain defects found+
  fixed or queued, headline gain not yet attempted on a fully-hardened chain. Resume here.
- Acceptance status: the three infra items (fibonacci×5 substrate, ranked-planner prompt,
  `FLUX_IMPROVE_EVAL_MODEL` knob) are **landed** and checked above; only the final item — a
  cleanly-measured funded round — remains, so the story stays `backlog` (blocked on funding a
  round after the two queued chain fixes above).

## Notes
- Loop-development work on `main` under the standing invariants (integrity/validity/
  target-clarity) — none of these changes touch scoring strictness, trials floor, or PROTECTED
  enforcement. The eval set change alters what is measured, not how strictly; recorded here so
  cross-era numbers are never silently mixed (fibonacci-only era starts with this story).
- Evidence: `docs/self-improvement/STATUS.md` journey entry 6; the round record in the
  `improve-tbench/20260706-130553` branch's `improve-log.jsonl`.
