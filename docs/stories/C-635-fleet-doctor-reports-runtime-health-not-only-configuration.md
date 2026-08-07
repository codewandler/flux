---
id: C-635
title: "fleet doctor reports runtime health, not only configuration"
pillar: "Core"
status: ready
priority: 8
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
design: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
note: "agents recorded active whose supervisor is gone, waves wedged in a transient state, worktrees a topology names but disk lacks, items claimed by more than one live wave, branches holding nothing unique"
---

# fleet doctor reports runtime health, not only configuration

## Goal

Make the fleet's recorded runtime truth askable. `flux fleet doctor` validated configuration and said
nothing about the running system, so every runtime question was asked with `jq` over `state.json` —
or, worse, reimplemented: the driver grew its own `/proc` scanner because nothing reported a worker
recorded `working` with no process behind it, and four separate waves each holding an attempt at the
same story were noticed by reading state by hand.

Each of these is a mechanical question with a single correct answer, asked of data the fleet already
owns. Doctor answers them and names the next action, without changing any state.

## Acceptance

- [x] `fleet doctor` reports five runtime checks alongside its existing configuration verdict:
      `agent-supervisor-gone`, `wave-wedged`, `worktree-missing`, `item-double-claimed`,
      `branch-without-unique-work`.
- [x] Every finding carries `check`, `subject`, `detail` and a one-line `fix`, and reaches human
      output as a warning as well as `data.runtime` for a machine caller.
- [x] Doctor stays read-only and does not fail the run on a runtime finding: `data.runtime.healthy`
      is the machine-readable verdict, so a driver can keep polling a fleet that needs attention.
- [x] `fleet validate` is unchanged — configuration only, no `runtime` key, no warnings.
- [x] A wave that finished for good is exempt from the worktree and claim checks, because its
      worktrees are supposed to be gone and its items are released.
- [x] Failing first, `fleet_doctor_reports_runtime_health_not_only_configuration` drives one
      deliberately unhealthy state through the real `FleetAction::Doctor` arm and asserts the exact
      set of findings; `a_branch_left_at_its_pinned_base_holds_nothing_unique` covers the fifth check
      with an injected head resolver.

## Progress

- Implemented in `crates/flux-cli/src/board_fleet_cmd.rs`. The `Doctor | Validate` arm stays shared
  for the configuration question and branches only for the runtime half.
- `wave_worktrees` is now the single definition of "the worktrees a wave's topology names", used by
  both `reclaim_wave_storage` and the worktree/branch checks. A checker walking a different set than
  the reclaimer would report healthy exactly the worktree the other had removed.
- Branch heads resolve with one `git for-each-ref` per repository, not one call per branch: a fleet
  that has run for a day holds hundreds of branches.

## Notes

- Design: [recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven](../designs/recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven.md).
- Pid reuse can make a recycled pid read as alive, exactly as it can for `worker_activity`. That is
  the pre-existing behaviour, so this is a strict improvement rather than a guarantee.
- `wave-wedged` covers `integrating`, the one wave state with a recorded owning process. Other
  statuses have no owner to be gone.
