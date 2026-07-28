---
id: C-131
title: flux policy simulate — replay a proposed policy against recorded history
pillar: Core
status: backlog
priority:
epic:
design:
note: "before adopting a policy edit, replay it over the last N sessions' recorded ops: 'this change would have blocked these 12 ops and newly-allowed these 3', as a diff-style report; pure read over the event log + existing policy evaluator; the trust-builder for approval distillation (C-94)"
---

# flux policy simulate — replay a proposed policy against recorded history

## Goal
Let an operator trust a policy change before adopting it: `flux policy simulate <proposed.toml>`
replays the proposed policy against the recorded op history and reports, diff-style, which
historical ops it would have newly blocked and newly allowed relative to the active policy.

## Acceptance
- [ ] `flux policy simulate <file> [--sessions N]` evaluates both the active and proposed policy
  against recorded op requests and prints newly-blocked / newly-allowed / unchanged counts with
  per-op detail — failing-first test over a seeded event store.
- [ ] Pure read: simulation writes nothing to the event store and constructs no providers.
- [ ] Ops whose recorded context is insufficient to re-evaluate are reported as
  "indeterminate", never silently classified.
- [ ] `--json` output for tooling.

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

## Notes
- Pairs with approval distillation (C-94): distillation proposes a policy; simulation lets you
  trust it before adoption.
