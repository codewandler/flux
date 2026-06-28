---
id: I-01
title: Statistically clean self-improvement headline gain (trials ≥ 3)
pillar: Improve
status: backlog
priority:
design:
---

# Statistically clean self-improvement headline gain (trials ≥ 3)

## Goal
The self-improvement loop has produced exactly one kept gain so far, at trials=1–2. Produce a
**statistically clean, grader-confirmed headline gain** (trials ≥ 3 with a strict keep margin) — the
proof currently missing from `docs/self-improvement/STATUS.md`.

## Acceptance
- [ ] A kept improvement validated over **trials ≥ 3** with the strict keep margin (no noise win).
- [ ] Partial-credit-aware tag scalars + token/cost capture wired in (STATUS "Known gaps" #12).
- [ ] The result recorded in `docs/self-improvement/STATUS.md` with evidence (git tag, asciinema
      casts, `improve-log.jsonl` entries). The agent never grades itself.

## Progress
- (not started — see STATUS.md "What's proven" / "Known gaps")

## Notes
- Loop entry point: `examples/improve-tbench.flux` driven by `bench/run-tbench-loop.sh`.
- This is environment-gated (needs Docker + terminal-bench + a live model key).
