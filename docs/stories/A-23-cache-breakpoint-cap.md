---
id: A-23
title: "Cap prompt-cache breakpoints at 4 — guard the subscription-claude ceiling"
pillar: Agent
status: done
epic: library-hardening
design: docs/designs/library-hardening.md
note: "segmented_system_field stamps cache_control on every cache:true segment uncapped; on subscription-claude prefix + planner-A + phase + base-B = exactly 4 (Anthropic's hard max) — the next cache:true breakpoint → HTTP 400 on every planner call, nothing pins ≤4"
---

# Cap prompt-cache breakpoints at 4 — guard the subscription-claude ceiling

## Goal
Stop the assembled request from ever carrying more than Anthropic's 4 `cache_control` blocks.
`segmented_system_field` marks every `cache:true` segment with no cap
(`crates/flux-providers/src/messages/mod.rs:133`). On the subscription-claude path the credential prepends
its own cache segment (`crates/flux-providers/src/anthropic.rs:110`, inserted `cache:true` at index 0 in
`crates/flux-provider/src/lib.rs:362`) and the planner adds three more (planner-A, phase, base-B in
`crates/flux-flow/src/compile.rs:1293`) = **exactly 4**. The invariant is unpinned, and one breakpoint is
largely wasted on the frequently-changing skill-body segment.

## Acceptance
- [ ] Failing-first test asserting the assembled request carries **≤ 4** `cache_control` blocks across
      system + tools + messages on the subscription-claude planner path (today it sits at exactly 4 with a
      fifth cache:true segment tipping it to a 400).
- [ ] A coalescing rule caps breakpoints: drop `cache_control` on sub-`CACHE_MIN_CHARS` segments and/or keep
      only the last N−1 breaks, so adding a future cache:true segment can't exceed 4.
- [ ] Cache hit-rate on the current layout is not regressed (the stable prefix keeps its breakpoint).
- [ ] Rule documented in the design doc.

## Progress
- 2026-07-03 DONE — `segmented_system_field` caps `cache_control` at `MAX_CACHE_BREAKPOINTS=4` via `cache_breakpoints` (keeps the 4 largest, preserving the stable planner prefix → no hit-rate regression). Tests: `assembled_request_caps_cache_breakpoints_at_four`, `four_cache_segments_are_all_kept`. Full gate green.

## Notes
- Evidence: `crates/flux-providers/src/messages/mod.rs:133`, `crates/flux-providers/src/anthropic.rs:110`,
  `crates/flux-provider/src/lib.rs:362`, `crates/flux-flow/src/compile.rs:1293`.
- Residual of [A-03](A-03-cache-stable-prompt-layout.md) / [A-13](A-13-phase-aware-planner-protocol.md).
  Design: [library-hardening](../designs/library-hardening.md).
