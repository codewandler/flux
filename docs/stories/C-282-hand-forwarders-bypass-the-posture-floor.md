---
id: C-282
title: "Two hand-rolled sandbox-env forwarders sit downstream of the posture floor and can push `off`"
pillar: Core
status: ready
priority: 4
epic: security-assurance
design: docs/designs/security-assurance.md
note: "found by C-276's review — the floor guarantee is a property of `posture_env`, not of the whole spawn path; runner.rs and worker.rs read ambient env and push FLUX_SANDBOX into the caller-override slot, which lands AFTER it. Pre-existing on main, behaviour-identical before and after C-276"
---

# Two hand-rolled sandbox-env forwarders sit downstream of the posture floor and can push `off`

## Goal

C-276 made a spawn forward its resolved sandbox posture to its children, and made that forwarding a
**floor**: `posture_env` renders every value from the resolved `Sandbox` alone, an `Off` posture
forwards nothing, and an open network forwards nothing — so it can only ever tighten a child.

That guarantee is real, and it is a property of **`posture_env`**, not of the whole spawn path. Two
call sites hand-roll their own sandbox-env forwarding and push it into the *caller-override* slot,
which `System::build_command` applies **after** the allow-list and therefore after `posture_env`:

- `crates/flux-eval/src/runner.rs:244-246,366`
- `crates/flux-orchestrate/src/worker.rs:309-315`

Both read the ambient environment and can push `FLUX_SANDBOX`, including `off`. So the value a child
finally sees is not necessarily the floor the parent resolved — the last writer wins, and the last
writer is a hand-rolled copy of a decision that now has one correct implementation.

**This is pre-existing on `main` and behaviour-identical before and after C-276.** It is not a
regression, and C-276 was right not to widen its scope into it. It is filed because C-276's review
found that the two mechanisms now overlap, and an overlapping hand-rolled copy of a safety decision
is exactly the drift shape C-249 was filed for.

## Acceptance

- [ ] A failing-first test demonstrates the gap concretely: a process with a resolved non-`Off`
      posture spawns through one of these two paths under an ambient `FLUX_SANDBOX=off`, and the
      child receives `off` rather than the parent's floor. Assert the **whole** forwarded posture,
      not the one key you expect to move — C-276's tests use a `forwarded_posture` helper for exactly
      this reason and it is the shape to copy.
- [ ] Both call sites stop hand-rolling the decision. The values must come from the same source
      `posture_env` reads (the resolved `Sandbox`), not from `std::env` — **this is the identical
      defect C-276's round 1 shipped and was reworked for**: gating on the resolved posture while
      taking the value from the ambient environment. Read that story's Progress before choosing a
      shape; the trap is documented there in full.
- [ ] If either call site has a legitimate reason to override the posture *downward*, it is stated
      and enforced rather than left implicit. "It reads ambient env because it always has" is not a
      reason. If there is no such reason, the override should not be reachable at all.
- [ ] The ordering asymmetry is documented where it will be hit: `posture_env` is applied *before*
      caller overrides (a posture is an inherited default a trusted call site may override) while
      the `FLUX_SANDBOXED` marker is applied *after* them (so no call site can forge or clear it).
      That difference is deliberate and is what makes this story possible; say so at the override
      slot, not only at `posture_env`.
- [ ] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap`.

## Progress

- (not started)

## Notes

- Provenance: the independent review of [C-276](C-276-sandbox-posture-does-not-reach-a-child-flux.md)'s
  rework, filed as a MINOR alongside a `PASS`. The reviewer was explicit that it is not blocking and
  not caused by that diff — quoted here so nobody re-derives the severity: *"Pre-existing on main and
  behaviour-identical before and after this diff … so not blocking, but the diff's floor guarantee is
  a property of `posture_env`, not of the whole spawn path."*
- ⚠ Check whether these two forwarders duplicate each other as well as `posture_env`. C-276's
  Progress section flagged two redundant hand-forwarders (`SANDBOX_POSTURE_ENV` /
  `sandbox_child_env`) and said to file them; the review confirmed no story covered them. If they are
  the same two sites, this story owns both. If they are a third and fourth, say so here — a
  half-converted set is worse than none, because it looks done.
- Related: [C-276](C-276-sandbox-posture-does-not-reach-a-child-flux.md) built the floor;
  [C-262](C-262-fail-closed-unattended-sandbox-profile.md) installed the fail-closed switch that an
  ambient `off` short-circuits; [C-249](C-249-git-family-clean-tree-policy-and-stash-wording.md) is
  the precedent for replacing hand-copied policy with one enforced implementation.
