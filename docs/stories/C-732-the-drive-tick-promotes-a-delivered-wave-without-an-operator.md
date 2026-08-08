---
id: C-732
title: "The drive tick promotes a delivered wave without an operator"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
note: "C-681 built flux fleet promote and it is idempotent, but no drive tick calls it, so landing on a member's local canonical ref is still operator-invoked. With C-730 dispatching the integrator and C-587 gating on review, promote is the last link the tick does not pull. Wiring it closes worker to local main end to end"
---

# The drive tick promotes a delivered wave without an operator

## Goal


## Acceptance

- [x] A drive tick lands an accepted candidate on its member's local canonical ref with no operator
      command. `promote` existed and was idempotent; nothing called it, so a fleet left alone
      finished every wave and delivered none of them.
- [x] The tick decides on evidence rather than a guess: `awaiting-delivery` is exactly the status
      C-721's `apply` records when it accepted a candidate the canonical ref does not contain, and
      an `applied` wave is never asked again.
- [x] It calls the same `promote_members` the CLI verb does rather than restating its rules, so
      `--dry-run`, the refusal of a remote-tracking `canonical_ref`, the untouched branches on a red
      gate and the containment re-read cannot drift between the attended and unattended paths.
- [x] A promotion failure is reported and journalled, never a reason to lose a tick that already
      recorded handoffs and reviews — the same contract as a failed dispatch or integrator.
- [x] The tick's one summary line says what promotion *answered*, so a fleet that delivered is
      distinguishable from one that only looked busy.
- [x] Regression test: `a_wave_awaiting_delivery_is_what_makes_a_tick_promote`.

## Progress

- Implemented in `crates/flux-cli/src/board_fleet_cmd.rs` as `drive_promotion_ready` plus the tick
  call, on top of C-681's `promote_members`.
- This was the last operator-invoked step between a worker's commit and a member's local `main`.
  With C-730 (verified handoff + integrator dispatch) and C-587 (independent review) it closes the
  chain: dispatch, implement, hand off with evidence, review, integrate, land — none of them typed.
- Full repository gate green on the combined tree with C-587, C-681 and C-730.
