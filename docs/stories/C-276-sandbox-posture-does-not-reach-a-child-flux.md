---
id: C-276
title: "`SAFE_ENV` forwards the confinement *marker* but not the posture, so a child flux runs unconfined"
pillar: Core
status: ready
priority: 2
epic: security-assurance
design: docs/designs/security-assurance.md
note: "found by C-243's implementor: SAFE_ENV carries FLUX_SANDBOXED (the marker CLAIMING confinement) and none of FLUX_SANDBOX/_NET/_WRITABLE/FLUX_BWRAP_BIN — so a spawned flux resolves its posture from an empty env and defaults to `off` while the operator demanded `require`"
---

# `SAFE_ENV` forwards the confinement *marker* but not the posture

## Goal

`flux_system`'s guarded spawn clears the child's environment to `SAFE_ENV`. That list carries
**`FLUX_SANDBOXED`** — the marker whose entire job is to assert *"you are already confined"* — and
**none** of the variables that decide whether confinement actually happens. A spawned `flux` therefore
resolves its sandbox posture from an environment containing no posture, defaults to `off`, and runs
unconfined while the operator demanded `require`. Make the posture travel with the marker, or make the
marker not travel alone.

## Acceptance

- [ ] A failing-first test spawns a child `flux` through the guarded path with `FLUX_SANDBOX=require`
      set on the parent and asserts the child resolves `require` — it currently resolves `off`.
- [ ] `FLUX_SANDBOX`, `FLUX_SANDBOX_NET`, `FLUX_SANDBOX_WRITABLE` and `FLUX_BWRAP_BIN` /
      `FLUX_SANDBOX_EXEC_BIN` reach a child that is meant to inherit the operator's posture — or the
      decision goes the other way and is stated: the marker stops travelling too, so a child that
      cannot see the posture also cannot believe it is confined.
- [ ] Whichever way it goes, the **asymmetry** is gone. Forwarding a claim of confinement without the
      means to enforce it is the defect; either both travel or neither does.
- [ ] Every existing consumer of the guarded spawn is checked for the behaviour change, not just the
      fleet path — `flux-eval`'s runner already hand-forwards these four (`runner.rs:225-245`) and would
      become redundant or conflicting.
- [ ] `crates/flux-cli/tests/sandbox_backend.rs` (C-266's with-backend lane) carries the proof, because
      a test on a host with no backend cannot distinguish `off` from `require`-but-unavailable. That
      lane exists precisely for this class.
- [ ] Full gate green in both postures, plus `scripts/check-no-direct-io.sh`.

## Progress

- (not started)

## Notes

- Verified in the tree at `f29cb0dd`: `crates/flux-system/src/lib.rs:2058-2095` — `SAFE_ENV` contains
  `"FLUX_SANDBOXED"` and zero of the four posture variables.
- Found by **C-243**'s implementor while fixing a related-but-different defect (a worker being
  bwrap-wrapped into its own netns). It worked around this one locally by forwarding the posture
  explicitly for its own spawn; this story is the general fix. Its local workaround should be reviewed
  for redundancy once this lands.
- ⚠ This is a *safety-envelope* change: it decides whether a spawned flux is confined. Treat a
  regression here as a release blocker, and read AGENTS.md's "one guarded path starts every OS process"
  before touching `build_command`.
- Related: [C-262](C-262-fail-closed-unattended-sandbox-profile.md) established the fail-closed posture
  this fails to propagate; [C-266](C-266-sandbox-backend-ci-coverage.md) built the lane that can prove it.
