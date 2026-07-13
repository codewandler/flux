---
id: A-74
title: Keep semantic capability expansion inside the live ceiling
pillar: Agent
status: done
design: docs/designs/adaptive-outer-loops.md
note: "s_1162: a valid late web-search signal was compared with the immutable turn-start surface and aborted Slack exploration as stale."
---

# Keep semantic capability expansion inside the live ceiling

## Goal

Allow intent and exploration to surface a registered semantic capability family after turn start
without mistaking that visibility expansion for registry drift, while preserving the agent tool,
permission, and `with_tools` ceiling.

## Acceptance

- [x] Failing-first: `semantic_capability_signal_expands_beyond_initial_surface_within_live_ceiling`
      reproduces the `s_1162` shape (`plugin.slack` intent, then `plugin.websearch`) and proves the
      next native request receives both families' exact live schemas.
- [x] A semantic signal changes visibility only. Every selected operation remains registered and
      allowed by the live permission/capability ceiling; a bare deny or narrower `with_tools` scope
      cannot be re-granted.
- [x] Inactive non-semantic/operator groups such as `shell` remain unavailable to intent and later
      signals.
- [x] Genuine stale state still fails closed and names the exact unavailable operations plus their
      live status instead of returning one opaque invariant error.
- [x] The no-send live Bitcoin-to-Slack scenario reaches action approval without the stale-state
      abort and executes no Slack write; focused tests and the release gate pass before recut.

## Progress

- 2026-07-13: `s_1162` proved the initial Slack intent succeeded, a second provider round ran, and
  the next round failed at `selected_specs_for_state`. The running binary predates the unrelated
  code-review edits, ruling those out as the cause.

## Notes

- The invariant is visibility-only: authorization, approval, and guarded dispatch remain unchanged.
- The existing adaptive-loop design is updated in place; this is a release-blocking A-73 correction.
