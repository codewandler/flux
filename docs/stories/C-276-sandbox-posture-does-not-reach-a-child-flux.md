---
id: C-276
title: "`SAFE_ENV` forwards the confinement *marker* but not the posture, so a child flux runs unconfined"
pillar: Core
status: in-progress
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

- [x] A failing-first test spawns a child `flux` through the guarded path with `FLUX_SANDBOX=require`
      set on the parent and asserts the child resolves `require` — it currently resolves `off`.
- [x] `FLUX_SANDBOX`, `FLUX_SANDBOX_NET`, `FLUX_SANDBOX_WRITABLE` and `FLUX_BWRAP_BIN` /
      `FLUX_SANDBOX_EXEC_BIN` reach a child that is meant to inherit the operator's posture — or the
      decision goes the other way and is stated: the marker stops travelling too, so a child that
      cannot see the posture also cannot believe it is confined.
- [x] Whichever way it goes, the **asymmetry** is gone. Forwarding a claim of confinement without the
      means to enforce it is the defect; either both travel or neither does.
- [x] Every existing consumer of the guarded spawn is checked for the behaviour change, not just the
      fleet path — `flux-eval`'s runner already hand-forwards these four (`runner.rs:225-245`) and would
      become redundant or conflicting.
- [x] `crates/flux-cli/tests/sandbox_backend.rs` (C-266's with-backend lane) carries the proof, because
      a test on a host with no backend cannot distinguish `off` from `require`-but-unavailable. That
      lane exists precisely for this class.
- [x] Full gate green in both postures, plus `scripts/check-no-direct-io.sh`.

## Progress

- **The posture now travels with the marker**, as a **floor and never a ceiling**.
  `sandbox::posture_env` (`crates/flux-system/src/sandbox.rs`) returns the five posture variables
  when this process's resolved mode is not `Off`, and `apply_safe_env` chains them onto `SAFE_ENV`.
  Each key's safety against the deny-by-default env rule is argued in that function's doc.
- **An `Off` sandbox forwards nothing**, deliberately — not even `FLUX_SANDBOX=off`. On the reading
  side `off` is not "no opinion": it is `flux-cli`'s explicit kill switch, which beats a child's own
  `[sandbox] require` *and* C-262's unattended fail-closed profile. Forwarding it would have turned
  this fix into a new bypass channel. Withholding it leaves today's behaviour intact (a child
  resolves its own posture), so the change can only tighten a child, never loosen one.
- The marker's own rule is untouched: `sandbox_marker` still stamps `FLUX_SANDBOXED=1` only for a
  genuinely wrapped spawn, and is still applied *after* caller overrides so no call site can forge
  it. The posture is applied *with* the allow-list, before caller overrides, because it is an
  inherited default a trusted call site may legitimately override (the local-eval child host does).
- Proof, in C-266's with-backend lane (`FLUX_TEST_SANDBOX_BACKEND=1`):
  `a_confined_child_inherits_the_posture_and_not_only_the_marker` (the asymmetry: at the base the
  child reported `posture=[] marker=[1] net=[]`) and `a_child_flux_resolves_the_parents_require_posture`
  (a real child `flux` now emits the C-217 OUTER-CONFINEMENT audit line; at the base it was silent,
  which is what a resolved `off` looks like). Hermetic regression guards for both halves of the rule
  live in `flux-system` (`the_sandbox_posture_survives_env_clear_so_the_marker_never_travels_alone`,
  `an_off_sandbox_forwards_no_posture_so_a_child_keeps_its_own`, plus two `posture_env` unit tests).
- **Owed elsewhere (not in this diff's write set).** Two hand-forwarders are now redundant for every
  non-`off` posture, and both carry doc comments this change falsifies:
  - `crates/flux-eval/src/runner.rs` — `SANDBOX_CHILD_ENV_KEYS` + `sandbox_child_env`.
  - `crates/flux-orchestrate/src/worker.rs:109-124,311` — `SANDBOX_POSTURE_ENV` + `worker_env`, whose
    doc still asserts "None of `FLUX_SANDBOX*` is in `build_command`'s `SAFE_ENV`" (`worker.rs:1579`).
  Neither conflicts: both push the same values as explicit overrides, which are applied last and
  win. Both do still forward `FLUX_SANDBOX=off`, which `SAFE_ENV` deliberately will not — so
  removing them is a *behaviour* decision about the kill switch, not a pure cleanup. File it.

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
