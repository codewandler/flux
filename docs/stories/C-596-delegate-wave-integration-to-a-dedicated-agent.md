---
id: C-596
title: "Delegate wave integration to a dedicated agent instead of an operator-only CLI verb"
pillar: Core
status: ready
areas: [flux-cli]
note: "integrate/apply/handoff/rework are in nobody's model-facing catalog, so a green wave can only move forward if a human types the command"
---

# Delegate wave integration to a dedicated agent

## Goal

Let the Fleet coordinator hand a finished wave to one dedicated agent whose only job is merge,
integration and the final gate, so a wave of accepted story commits reaches a green candidate without
an operator typing a CLI verb. The agent's authority stops at a green gate; applying and publishing
stay operator authority.

    worker commits
       └─> fleet/wave-N/<repo>/integration   [agent: integrate + gate]
             └─> local main                  [operator: fleet apply]
                   └─> origin/main           [operator: explicit push]

## Acceptance

- [ ] A `fleet.integrate` native operation exists, mapping to `flux fleet integrate <wave>` the same
      way `fleet.run` maps to `flux fleet run` (`board_fleet_cmd.rs`, `NativeCoordinatorOperation`).
- [ ] A dedicated integrator operation set — `fleet.integrate` plus `fleet.status`, and nothing else
      — is installable on a non-main admitted agent. The `spec.id == "main"` gate on
      `--native-fleet-main` (`agent_turn_argv`) is generalized to a role-scoped selection rather than
      duplicated.
- [ ] The coordinator delegates: it does not gain `fleet.integrate` itself, preserving `main.md`'s
      separation between scheduling work and assembling it.
- [ ] Failing first, a test proves the delegated path still records `gate.runs == 1` and leaves the
      source checkout's main branch unmodified — mirroring
      `fleet_verifies_handoff_runs_one_final_gate_and_applies_only_explicitly`
      (`crates/flux-cli/tests/board_fleet_cli.rs:1743`).
- [ ] A test proves the integrator cannot apply, push, or transition a Board item: those operations
      are absent from its ceiling and `validate_admitted_operation_ceiling` refuses if its loop names
      one.
- [ ] `flux fleet validate` accepts the `wave-integrator` template and `integration` loop profile.

## Progress

- Designed and grounded; config artifacts already written, Rust not started.
- Already in the repository, ready to reference:
  - `.flux/fleet/agents/wave-integrator.md` (roadmap repo) — instructions.
  - `.flux/fleet/loops/wave-integration.flux` (roadmap repo) — the authored loop, ceiling
    `["fleet.integrate", "fleet.status"]`. **Not yet referenced from `fleet.toml`**, because a loop
    naming an operation that does not exist is refused at admission.
- Remaining config once the operations exist: `[[agent_templates]] id = "wave-integrator"`,
  `role = "integrator"`, `task_kind = "integration"`; `[loop_profiles.integration]`;
  `[loop_policy] integration = "integration"`. All three are free strings — no schema change.

## Notes

- **Do not rebuild integration in a model loop.** `integrate_wave` (`board_fleet_cmd.rs`) is already
  deterministic host code with no model call: it refuses unless every story is `handoff-accepted`,
  orders by `integration_order` (DFS over story `depends_on`), enforces write-set disjointness before
  touching git, cherry-picks each accepted commit onto `fleet/<wave>/<repo>/integration`, and runs
  the configured `final_gate` exactly once. The agent is the authorized *caller and reporter*; moving
  ordering or gate-once accounting into model judgment would trade a test-proven invariant for prose.
- `git_merge` exists as a tool and its description even names this use case ("the integration loop's
  audit trail"), but it is deliberately outside every capability bundle: *"Integration, checkout,
  worktree mutation, revert and push remain coordinator operations, not worker tools."* Using it
  would mean re-deriving dependency order and `gate.runs == 1` in the loop. Rejected.
- `mode` is a closed enum and neither arm fits an agent that may only call one typed Fleet operation:
  `read-only` rejects every capability but `read`/`git-read`, `write` *requires* `read`+`edit`+`git`.
  Install the native operations outside the capability bundles, exactly as
  `native_fleet_main_tools_at` already does for main.
- Out of scope: `flux fleet run` is hard-coded to `find(|t| t.id == "story-worker")` and there is no
  per-story template selection. Integration is dispatched *after* `handoffs-ready`, so it does not
  need that loop.
- Motivation from a live run: waves 253/257/260 produced accepted commits that could not move
  forward, because `handoff`, `rework`, `integrate` and `apply` are operator-only verbs and the
  coordinator's catalogue (`NativeCoordinatorOperation::name`) contains none of them.
