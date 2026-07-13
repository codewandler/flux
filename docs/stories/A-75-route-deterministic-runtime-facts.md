---
id: A-75
title: Keep every live operation discoverable to intent routing
pillar: Agent
status: done
design: docs/designs/adaptive-outer-loops.md
note: "s_1169: `get the current time` selected no capability because `now` was hidden inside the generic `core` family."
---

# Route deterministic runtime facts to explicit capabilities

## Goal

Keep arbitrary requests for live facts on the evidence path by ensuring every live operation has
semantic routing metadata. Family compression may omit schemas, but it must never silently hide an
ungrouped operation from the intent router.

## Acceptance

- [x] Failing-first: a virtual family containing more than eight arbitrary operations still exposes
      every operation name to the intent router.
- [x] Requests requiring live/runtime/external facts are instructed to select evidence capabilities
      rather than answer from model memory.
- [x] Low-risk, side-effect-free reads remain gather-safe when their result must stay fresh;
      non-cacheability alone does not turn them into an approval-gated action.
- [x] `get the current time` selects the live `core` family, invokes `now`, and answers from its returned evidence in
      a live CLI run with the release candidate.
- [x] The intent stage remains model-routed; there is no lexical bypass or special-case execution.
- [x] Focused tests and the release gate pass before release.

## Progress

- 2026-07-13: Session `s_1169` selected zero operations and answered that the clock was unavailable,
  even though startup evidence confirms `now` was registered and wired.

## Notes

- The PoC's evidence-to-capability mapping was precise. The default integration regressed it by
  grouping untagged always-visible operations under virtual summaries that exposed only their first
  eight names.
