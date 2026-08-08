---
id: C-730
title: "A finished worker turn becomes a verified handoff without an operator"
pillar: "Core"
status: done
epic: delivery-is-verified
areas: [flux-cli, flux-tools]
note: "Nothing converts turn_end into a handoff: grep finds zero wiring, fleet_handoff exists only as a CLI verb an operator types, and drive writes only provisional handoffs by reconstruction. The integrator role is worse — the string integrator appears 0 times in flux-cli, so the configured role and wave-integration.flux are dead config the driver never dispatches. Until a turn produces a handoff carrying its own validation evidence and the tick dispatches an integrator, every wave stops at the writers and the rest of the pipeline is an operator typing verbs"
---

# A finished worker turn becomes a verified handoff without an operator

## Goal

Close the first two breaks in the unattended path from a finished worker turn to a gated wave.

A worker turn that ends with commits on its story branch must produce a handoff that carries its
own evidence — the commit, the observed write set, and the targeted validation argv *with the result
of running it* — without an operator typing `flux fleet handoff`. Where the worker ran a targeted
test, that run is the evidence and it is re-run rather than believed. Where it ran none, the handoff
must say so explicitly instead of claiming what it cannot show.

A drive tick must then dispatch the configured `integrator` role, on the configured `integration`
loop profile, for a wave whose stories have all handed off. `handoffs-ready` is a terminus today:
`.flux/fleet.toml` declares the role and the loop, and nothing in the driver has ever sent one.

Neither half may weaken `flux fleet handoff`'s refusal to accept a handoff with no typed validation
argv. That refusal is the point of the epic; this story makes the evidence real and automatic, not
optional.

## Acceptance

- [x] A finished worker turn records a handoff carrying the commit, the observed write set, the
      targeted validation argv the worker itself ran, and that argv's failing-before/passing-after
      result — with no operator command.
      → `record_turn_handoffs` + `worker_recorded_test_argv`, proved by
      `a_turn_hands_off_the_validation_argv_it_ran_and_records_the_real_result`.
- [x] Evidence is produced by the same code path `flux fleet handoff` uses, so the automatic and the
      operator handoff cannot drift, and `flux fleet handoff` still refuses without typed validation
      argv.
      → both call `run_targeted_validation`; the refusal at `board_fleet_cmd.rs:18436` is untouched.
- [x] A turn that ran no targeted test records the handoff with an explicit no-failing-test reason
      and `verified: false`, never an invented claim.
      → `a_turn_that_cited_no_test_records_the_reason_rather_than_an_empty_claim`.
- [x] Validation that runs and refuses is recorded with the refusal as the handoff's reason, and the
      wave still advances — an unverifiable handoff is reported, never fatal and never invented.
      → `unverified_reason` carries the refusal; the same test asserts `status: handoff-accepted`.
- [x] A drive tick names, and dispatches, the `integrator` role on the `integration` loop profile
      for every wave at `handoffs-ready`.
      → `drive_integration_targets` + `dispatch_wave_integrator`, proved by
      `drive_dispatches_an_integrator_for_a_wave_whose_stories_all_handed_off`.
- [x] A wave that already holds a live integrator is not dispatched a second one.
      → `wave_has_live_integrator`, proved by `drive_does_not_dispatch_a_second_integrator_over_a_live_one`.
- [x] Both are visible in the tick output.
      → `reconstructed[].verified`/`.unverified[]` and the tick's `integration` array.

## Progress

Implemented on `impl/C-730`.

**The evidence half.** `record_provisional_handoffs` is now `record_turn_handoffs`. The comment it
carried — "a turn that has already ended cannot be asked to cite the argv it ran" — was true of the
model and false of the record: every typed tool call is already in the receipt Fleet stores at
`agent.last_turn.events`. `worker_recorded_test_argv` recovers the worker's own `cargo_test` call
(preferring one with a `filter`, then one with a `package`) and `run_targeted_validation` — extracted
from `fleet_handoff`, so the two paths cannot drift — re-runs it at the pinned `verify` checkout and
at the delivered commit. `provisional: true` is replaced by `verified: bool`, and an unverified entry
always carries either `unverified_reason` (the refusal itself) or `no_failing_test_reason`. An empty
`test_argv` with no stated reason is never written.

A bare `cargo test --workspace` is deliberately *not* accepted as targeted evidence: it is the
integrator's gate wearing a worker's clothes, it proves nothing about the story, and re-running it at
both ends per story would cost more than the gate it stands in for.

**The dispatch half.** `DrivePlan.integrate` + `drive_integration_targets` name every wave at
`handoffs-ready`, and `dispatch_wave_integrator` admits the template that declares the `integration`
task kind — which is what routes it through `loop_policy.integration` to the configured
`wave-integration.flux`. It is admitted at the **fleet root**, not in the wave's integration
worktree: `execution.rs:2464` builds the native integrator catalogue from the child's cwd and refuses
unless that cwd holds `.flux/fleet.toml`, which a repository checkout does not. Its ceiling is
unchanged — the closed two-operation `NATIVE_INTEGRATOR_OPERATIONS` — so it cannot merge, push or
apply. That remains C-681's.

Attempts are bounded at `MAX_INTEGRATOR_ATTEMPTS_PER_WAVE` and counted *before* the turn runs, so a
turn that dies mid-flight still spends one. Without that bound, an integrator that completed without
calling `fleet.integrate` would leave the wave at `handoffs-ready` and be re-dispatched every tick
forever. A wave that exhausts its attempts is reported by `drive_integration_exhausted`, naming the
host verb that finishes it, rather than silently skipped.

**Not done here.** The turn-end path does not re-check worktree cleanliness after running validation
the way `fleet_handoff` does. It does not need to: the recorded write set comes from
`git diff base..commit`, so working-tree dirt cannot make it false evidence, and this path must never
fail a wave.
