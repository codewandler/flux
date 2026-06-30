---
id: I-01
title: Statistically clean self-improvement headline gain (trials ≥ 3)
pillar: Improve
status: in-progress
priority: 1
note: offline half done (partial-credit scalar + durable token capture + synthetic `trials = 5` loop); the trials ≥ 5 grader-confirmed run is **staged** on a funded provider key
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
