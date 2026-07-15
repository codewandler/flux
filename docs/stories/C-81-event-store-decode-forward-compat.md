---
id: C-81
title: Make event-store decode forward-compatible (don't abort a whole stream on one bad row)
pillar: Core
status: done
priority: 8
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "Upgrade-safety (High) — one unknown/corrupt row makes conversation/turns fail for the whole stream"
---

# Make event-store decode forward-compatible

## Goal
Keep an older reader working when the shared `events.db` gains a new event variant or a single corrupt
byte. `decode_all` `?`-propagates `serde_json::from_str` and `EventKind` is a **closed** enum (no
`#[serde(other)]`), so one undecodable row makes every `conversation`/`turns`/`load_stream` call fail
for that entire stream — conversations unreadable and turns aborting through the whole rolling-upgrade window.

## Acceptance
- [ ] Failing-first test: a stream containing one unknown/corrupt row still decodes the rest (assert the
      good events are returned and the bad one is skipped/logged), instead of erroring the whole read.
- [ ] `decode_all` skips + logs undecodable rows, or `EventKind` gains an inert `#[serde(other)] Unknown`
      variant (choose one and document the trade-off).

## Progress
- (not started) — filed from the 2026-07-15 full code review.

## Notes
- `crates/flux-events/src/store/mod.rs:55` (`decode_all`).
- Design: [harness-hardening](../designs/harness-hardening.md).
