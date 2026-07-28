---
id: C-130
title: "Monetary budgets & quotas — hard spend enforcement (epic)"
pillar: Core
status: backlog
priority:
epic: monetary-budgets
design:
note: "EPIC — cost is observed (usage events, pricing, OpenRouter reported cost) but never enforced; add [budget] config with per-session/per-agent/rolling-per-day currency caps: soft threshold warns into context, hard cap stops before the next model call with a resumable suspension; per-principal caps for A2A/serve; distinct from token turn-budgets (A-10/A-26)"
---

# Monetary budgets & quotas — hard spend enforcement (epic)

## Goal
Turn flux's cost *observation* (usage events, pricing tables, provider-reported cost) into cost
*enforcement*: `[budget]` config with per-session, per-agent, and rolling per-day caps in currency.
Crossing a soft threshold surfaces a warning into the model's context; crossing the hard cap stops
before the next model call with a resumable suspension. Per-principal caps make serve/A2A
deployments multi-tenant-safe.

## Acceptance
- [ ] `[budget]` config: per-session, per-agent, and rolling per-day currency caps, each with soft
  and hard thresholds.
- [ ] Soft threshold crossing injects a visible warning into context and the transcript;
  hard cap stops **before** the next model call (never mid-op) — failing-first test with a mock
  provider and a tiny cap.
- [ ] The hard stop is a resumable suspension (await/resume machinery), not a dead session; a
  raised cap resumes the turn.
- [ ] Per-principal budgets enforced on served/A2A agents; one principal exhausting its budget
  never affects another — isolation test.
- [ ] Unpriced turns (`$?`) count conservatively (documented policy), never as zero.

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

## Notes
- Distinct from token-based turn budgets (A-10/A-26): this is currency, cross-turn, enforced.
- Spend accounting already deduplicates sub-agent usage (C-23) — reuse those rollups.
