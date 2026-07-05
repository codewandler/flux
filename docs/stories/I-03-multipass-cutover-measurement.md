---
id: I-03
title: Measure the multi-pass cutover — time-to-first-feedback, rounds, tokens, tbench pass-rate
pillar: Improve
status: done
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
- [x] Baseline captured on pre-cutover main (same tasks/trials/model) and recorded before the
      comparison run.
- [x] `PlanAttempt.phase` + C-15 efficiency projections report gather-rounds/turn and
      revise-rounds/turn (extend `efficiency_all` if needed).
- [x] Time-to-first-feedback measured (planning-state timestamp → first rendered artifact) on a
      small fixed prompt corpus; before/after reported.
- [x] Terminal-bench: same task set, trials ≥ 3, same model, pre vs post; strict comparison (no
      cherry-picking; regressions reported honestly).
- [x] Results recorded in the epic design doc (`docs/self-improvement/STATUS.md` untouched — the
      improve loop itself was not exercised); the cutover is only called done when this story is.

## Progress
- 2026-07-02 — everything that doesn't spend API credits is built, verified, and gate-green
  (1168 workspace tests). Remaining: the paid legs (user decides spend), then recording results.
  - **Efficiency projections (acceptance 2) DONE**: `EfficiencySummary` now folds
    `orient_rounds`/`gather_rounds`/`revise_rounds`/`accepted_plans` from `PlanAttempt.phase`
    (gather = every gather-phase attempt, a repair round is a paid round; revise = execute-phase
    attempts that accepted a further plan — the terminal chat round is not a revision; plans/turn
    is phase-blind so pre-A-14 logs report it too). `flux usage` prints `plans/turn` always and
    `gather/revise per turn` only when the log carries phase data (`has_phase_rounds` — on a
    pre-A-14 log the figures are unrecorded, not zero). Tests in
    `crates/flux-events/src/projection.rs`.
  - **TTFF harness (acceptance 3) BUILT**: `bench/run-ttff.sh` (dry-run by default, `--go` to
    spend, `--smoke` for a 1-run check) drives the fixed 5-prompt corpus
    (`bench/ttff/corpus.jsonl` + `bench/ttff/fixture/`) against baseline (`b528772`, parent of
    cutover `e3ba495`) and post binaries under `bench/ttff/record_run.py` (PTY recorder — raw
    timestamped chunks kept, so metrics re-derive without re-running); `bench/ttff/report.py`
    derives spawn→first-rendered-artifact medians (spinner frames/labels/elapsed and the
    `· session s_N` banner excluded; failed turns flagged via the CLI's error marker since a
    failed turn still exits 0). Validated free: fake-CLI self-test with known timings +
    bogus-key probe against the real binary.
  - **tbench compare harness (acceptance 1+4 plumbing) BUILT**: `bench/run-tbench-compare.sh`
    (dry-run default) — the post binary drives a generated `eval_run` flow per leg with
    `rebuild: false` and each leg's own prebuilt musl `flux_binary`
    (tasks `chess-best-move,fibonacci-server` × 3 trials, dataset `terminal-bench-core==0.1.1`,
    both raw reports kept). Flow shape validated free via the mock adapter. Baseline worktree at
    `~/projects/flux-ttff-baseline` (b528772), release binaries prebuilt; `tb`/Docker/musl
    target confirmed present.
  - Model for both harnesses: `openrouter-anthropic/anthropic/claude-sonnet-4.6`
    (override via `FLUX_TTFF_MODEL`/`FLUX_TBC_MODEL`).

- 2026-07-05 — **paid legs ran** (results in the design doc "I-03 measurement results"):
  - **TTFF DONE, clean (30/30, `failed_trials: 0`)**: post wins all 5 prompts, −0.4s (trivial) to
    −5.1s (explore-complex); post `planning_ms` ≈ 71ms vs baseline null (A-12 silence gone).
  - **Rounds/tokens DONE**: no tiny-plan dribble, no call inflation, revise 0.0/turn; honest
    regression — gather-shaped prompts double uncached-in (corpus spend +35%), root-caused to
    `prompt_caching: false` in the openrouter profile → filed **C-35**.
  - **tbench baseline leg DONE** (0/2 pass-all, 14% checks); first post attempt **402'd**
    (key credit; kept as `post-report-402.txt`, excluded) → key topped up, post leg re-run valid.
- 2026-07-05 (later) — **post tbench leg DONE (valid re-run)**: 0/2 pass-all, 0% checks. Pass-all
  ties 0/6 vs 0/6; baseline keeps 14% partial checks, post keeps 0% and pays ~4× to fail on
  fibonacci-server — mechanical signature: execute-phase plans truncate at the 16384 emission
  ceiling and retry whole (→ filed **A-40**). Full verdict + small-n caveats in the design doc
  "I-03 measurement results". **Story complete** — all legs measured and recorded; regressions
  reported as found.

## Notes
- Sequenced after A-12..A-17 + L-22 land; listed ready so the board shows the full MVP set.
- Model for eval runs: `-m openrouter-anthropic/anthropic/claude-sonnet-4.6` (Anthropic key out of
  credits as of 2026-07-02).
