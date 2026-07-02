---
id: I-01
title: Statistically clean self-improvement headline gain (trials ≥ 3)
pillar: Improve
status: in-progress
priority: 1
note: offline half done; 2026-07-02 calibration VERDICT — the synthetic suite is stable but SATURATED (Sonnet 4.6 AND Haiku 4.5 via OpenRouter both score 1000/1000, mean_iters 1.0, twice) → zero headroom, it is a regression floor not a gain vehicle; the headline gain must come from terminal-bench (tb + Docker + musl all present; OpenRouter key forwards into the container) — full loop run postponed by user 2026-07-02
---

# Statistically clean self-improvement headline gain (trials ≥ 3)

## Goal
The self-improvement loop has produced exactly one kept gain so far, at trials=1–2. Produce a
**statistically clean, grader-confirmed headline gain** (trials ≥ 3 with a strict keep margin) — the
proof currently missing from `docs/self-improvement/STATUS.md`.

## Acceptance
- [ ] A kept improvement validated over **trials ≥ 3** with the strict keep margin (no noise win).
- [x] Partial-credit-aware tag scalars + token/cost capture wired in (STATUS "Known gaps" #12).
- [ ] The result recorded in `docs/self-improvement/STATUS.md` with evidence (git tag, asciinema
      casts, `improve-log.jsonl` entries). The agent never grades itself.

## Progress
- **Offline half done (gate-green).** Partial-credit-aware tag scalar (`score.rs`,
  `round(mean_check_pass_rate*1000)`); durable per-turn token capture (persisted on the event store's
  `TurnEnded`, summed back into `RunResult.tokens` so `mean_tokens` is a real tiebreaker); and the
  stable-baseline vehicle — `examples/improve-synthetic.flux` + `bench/run-synthetic-loop.sh` (synthetic
  suite, no Docker, **trials = 5**, strict `score_compare`), added to `PROTECTED` + flow validation.
- **Remaining (staged — needs a funded provider key):** calibrate the synthetic baseline for
  stability + headroom (`flux eval synthetic --trials 5 …`, twice), then drive
  `bench/run-synthetic-loop.sh` until a strict kept gain, and record it in STATUS.md with evidence.

## Notes
- Loop entry point: `examples/improve-tbench.flux` driven by `bench/run-tbench-loop.sh`.
- This is environment-gated (needs Docker + terminal-bench + a live model key).
