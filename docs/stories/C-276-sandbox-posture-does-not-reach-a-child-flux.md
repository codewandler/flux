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
  `sandbox::posture_env` (`crates/flux-system/src/sandbox.rs`) renders the posture when this
  process's resolved mode is not `Off`, and `apply_safe_env` applies it after `SAFE_ENV`. Each
  key's safety against the deny-by-default env rule is argued in that function's doc.
- **Every value is rendered from the resolved `Sandbox`, never read back out of `std::env`.**
  The first attempt at this story gated on `sandbox.settings().mode` but sourced the values from
  `std::env::var`, and independent review caught it: `System::with_sandbox` exists so an embedder
  can pin a posture *independent of ambient env* (`flux-sdk/src/lib.rs:653-657`), so the two sources
  legitimately disagree. A pinned `On` sandbox under an ambient `FLUX_SANDBOX=off` passed the gate
  and then shipped the child the **kill switch** — strictly less confined than forwarding nothing.
  Mirrored, a pinned `Require` with a silent env forwarded nothing, making the Goal a no-op for that
  whole consumer class. One source, or the guarantee is fiction. The wrapper path now forwarded is
  the absolute binary discovery resolved *and the preflight probe verified*, not an echo of
  `FLUX_BWRAP_BIN`; a sandbox with no backend of its own forwards no wrapper path at all.
- **An `Off` sandbox forwards nothing**, deliberately — not even `FLUX_SANDBOX=off`. On the reading
  side `off` is not "no opinion": it is `flux-cli`'s explicit kill switch, which beats a child's own
  `[sandbox] require` *and* C-262's unattended fail-closed profile. Forwarding it would have turned
  this fix into a new bypass channel. Withholding it leaves today's behaviour intact (a child
  resolves its own posture), so for `FLUX_SANDBOX` — and, per the bullet below, `FLUX_SANDBOX_NET` —
  `posture_env` can only tighten a child, never loosen one. **Scope that claim precisely**, in both
  directions: `FLUX_SANDBOX_WRITABLE` is a *union* rather than a narrowing (`dispatch.rs:256-270`
  unions the forwarded list with the child's own `[sandbox] writable` and de-dupes), bounded by the
  parent's envelope but not strictly tightening; and the guarantee belongs to `posture_env`, not to
  the whole spawn path, since a caller's explicit `env` overrides land after it and win.
- **An open network forwards nothing either** — the same rule, applied to `FLUX_SANDBOX_NET`, which
  review raised as a MINOR. A truthy value beats both `[sandbox] network` and C-262's
  unattended-closed default (`dispatch.rs:220-227`), so forwarding "open" would be a ceiling. The
  variable is emitted only to say *closed*, mirroring `flux-cli`'s own exporter, which writes it
  when narrowing and otherwise leaves it alone. Rendering from settings would otherwise have
  introduced this: `SandboxSettings`' network default is `true`.
- The marker's own rule is untouched: `sandbox_marker` still stamps `FLUX_SANDBOXED=1` only for a
  genuinely wrapped spawn, and is still applied *after* caller overrides so no call site can forge
  it. The posture is applied *with* the allow-list, before caller overrides, because it is an
  inherited default a trusted call site may legitimately override (the local-eval child host does).
- Proof, in C-266's with-backend lane (`FLUX_TEST_SANDBOX_BACKEND=1`):
  `a_confined_child_inherits_the_posture_and_not_only_the_marker` (the asymmetry: at the base the
  child reported `posture=[] marker=[1] net=[] wrapper=[]`, and the `wrapper` field is the sharpest
  evidence the values come from the resolved sandbox — nothing set `FLUX_BWRAP_BIN` on that run, so
  an env-echoing forwarder has nothing to echo) and `a_child_flux_resolves_the_parents_require_posture`
  (a real child `flux` now emits the C-217 OUTER-CONFINEMENT audit line; at the base it was silent,
  which is what a resolved `off` looks like).
- The **pinned** cases — the ones review found missing — are named tests in `flux-system`:
  `a_pinned_posture_reaches_a_child_when_the_ambient_env_is_silent` and
  `a_pinned_posture_beats_a_contradicting_ambient_env_instead_of_shipping_it`. Both were confirmed
  to fail against an ambient-reading forwarder before being kept (the second reproduces review's
  finding verbatim: `FLUX_SANDBOX=off` in the child env). `an_off_sandbox_forwards_no_posture_so_a_child_keeps_its_own`
  now sets the ambient env to `require`, not `off`, so it can pass for only one reason — review
  noted the old version was satisfied by two causes at once. Four `posture_env` unit tests cover the
  rendering, the backend variants, the `Off` carve-out and the open-network carve-out.
- **Owed elsewhere — deliberately NOT fixed here; pre-existing on `main` and behaviour-identical
  before and after this diff.** Two hand-forwarders read the *ambient env* and push `FLUX_SANDBOX`
  (including `off`) into the **caller-override** slot, which `apply_safe_env` applies after
  `posture_env` — so they can override the floor this story establishes. That is why the guarantee
  above is scoped to `posture_env` rather than to the spawn path:
  - `crates/flux-eval/src/runner.rs:244-246,366` — `SANDBOX_CHILD_ENV_KEYS` + `sandbox_child_env`.
  - `crates/flux-orchestrate/src/worker.rs:309-315` — `SANDBOX_POSTURE_ENV` + `worker_env`; its doc
    at `worker.rs:1579` also still asserts "None of `FLUX_SANDBOX*` is in `build_command`'s
    `SAFE_ENV`", which this change falsifies.
  Neither *conflicts* today: both push the same values, and they win only by landing last. The
  decision owed is whether a call site should be able to hand a child the kill switch at all — a
  behaviour question about the override slot, not a cleanup. The coordinator is filing it as its
  own story.

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
