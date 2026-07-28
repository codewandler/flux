---
id: A-93
title: "Typed session log — session-shape validity by construction (epic)"
pillar: Agent
status: backlog
epic: typed-session-log
design:
note: "EPIC — make the invalid provider-history shapes (split tool_use/tool_result, empty assistant, user-after-user) unrepresentable in the session log's type; the thrice-recurred bug class becomes unwritable instead of test-guarded"
---

# Typed session log — session-shape validity by construction (epic)

## Goal
The "session shape is always a valid provider history" safety invariant has broken three times
(cancel, compaction, the iteration cap) — each time on a newly added turn-termination path. Today
it holds by discipline, not by construction: termination paths funnel through one `finish_turn`
(`crates/flux-flow/src/engine.rs`) and compaction snaps its boundary so a `tool_result` is never
orphaned, but nothing stops a fourth termination path from bypassing the funnel. Make the session
log a typed state machine whose API cannot express an empty assistant message, a split
tool_use/tool_result pair, or a user-after-user sequence — the bug class becomes unwritable, and
the pre-release live-provider gate stops being the only net that catches it.

## Acceptance
- [ ] A design doc (`docs/designs/typed-session-log.md`) covering: the typed log states and legal
      transitions, how every turn-termination path (stop, cancel, compaction, iteration cap, and
      any future path) appends through the one typed API, the migration of existing history
      handling in `flux-flow`, and the provider-wire seam where the typed log projects to each
      codec's message shape.
- [ ] The epic is broken into implementation stories on the board; each behavioral change ships
      with a failing-first test.
- [ ] Headline proof: the three historical invalid shapes are unrepresentable (rejected at compile
      time or by the log's only constructors), pinned by a hermetic shape gate that would have
      caught all three past regressions without a live provider 400.

## Progress
- (not started — epic filed from a code-reading re-assessment of the engine's termination paths)

## Notes
- Downgraded from "design smell" to "hardening opportunity" during the code review: both current
  termination paths do funnel through `finish_turn`, and compaction explicitly protects the
  tool_use/tool_result boundary — the residual risk is the *next* termination path someone adds.
- The mock provider does not catch this class (see the safety invariants in AGENTS.md and the
  pre-release gate in docs/roadmap.md); validity-by-construction closes that structural gap.
- Smallest of the three re-assessment suggestions; purely internal, no user-visible behavior
  change when done right.
