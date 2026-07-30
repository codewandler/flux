---
id: C-289
title: "`FLUX_SANDBOXED` is forgeable by any call site whenever the sandbox is inactive — the project's default posture"
pillar: Core
status: ready
priority: 3
epic: security-assurance
design: docs/designs/security-assurance.md
note: "found by C-282's review, demonstrated with a probe: `child marker lines: [\"FLUX_SANDBOXED=1\"]` pushed straight through the caller-override slot. Pre-existing, but three comments across two crates assert the opposite"
---

# `FLUX_SANDBOXED` is forgeable whenever the sandbox is inactive

## Goal

`FLUX_SANDBOXED` is the marker that tells a child `flux` *"you are already confined — do not wrap
yourself again."* Its integrity is what stops a process from skipping its own confinement. The code
asserts in three places that a call site can neither forge nor clear it. That is true only when the
spawn is **genuinely wrapped**, and false otherwise — including in the project's default posture,
where the sandbox is inactive.

`sandbox_marker` returns `None` unless `Confinement::Sandboxed && sandbox.is_active()`
(`crates/flux-system/src/sandbox.rs:904-910`), and `build_command` writes the key only on `Some`
(`crates/flux-system/src/lib.rs:2209-2211`). So with an inactive sandbox nothing overwrites a
caller-supplied value, and a `FLUX_SANDBOXED=1` placed in the caller-override slot reaches the child
untouched. Demonstrated by C-282's reviewer:

```
child marker lines: ["FLUX_SANDBOXED=1"]
```

A child receiving that skips `Sandbox::resolve`'s re-wrap and believes it is confined when nothing
confines it. This is the exact defect C-276 fixed *pointing the other way* — there the marker
travelled without the posture; here the marker travels without the confinement.

## Acceptance

- [ ] A failing-first test demonstrates it concretely: a spawn through an **inactive** sandbox with
      `FLUX_SANDBOXED=1` in the caller-supplied env, and the child receiving it. Assert on the child's
      own environment, not on the parent's intent.
- [ ] The marker becomes unforgeable in both directions, or the limit is stated and enforced. The
      obvious shape is that `build_command` **removes** the key when `sandbox_marker` yields `None`,
      rather than leaving whatever the caller put there — but weigh it: is there a legitimate caller
      that must assert confinement it did not perform? `Confinement::Exempt` and the
      `AlreadyConfined` backend are the two places to look before assuming the answer is no.
- [ ] ⚠ Do not break `Backend::AlreadyConfined`. A process running inside an outer flux sandbox
      legitimately *is* confined without having wrapped anything itself, and that is exactly the case
      the marker exists to communicate. A fix that clears the marker unconditionally would make
      nested flux re-wrap inside its own containment. Whatever lands must distinguish "nobody
      confined this" from "someone else already did".
- [ ] The three comments that currently assert the false version are corrected — C-282 corrected them
      to describe the limit; this story either removes the limit or makes the description permanent.
      `crates/flux-system/src/sandbox.rs:933-935`, `crates/flux-orchestrate/src/worker.rs:302-304`,
      `crates/flux-system/src/lib.rs:2128`.
- [ ] Revisit whether `FLUX_SANDBOXED` should now be in `POSTURE_ENV_KEYS`. C-282 deliberately left
      it out on the strength of the "cannot be forged" claim. If that claim becomes true, the reason
      for the omission changes; if it stays false, the omission needs a different reason.
- [ ] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap` and
      `FLUX_TEST_SANDBOX_BACKEND=1 cargo test -p flux-cli --test sandbox_backend`.

## Progress

- (not started)

## Notes

- **Severity: judge it before treating it as urgent.** Every caller of the override slot is in-tree
  and trusted today, so this is not a live bypass — it is an invariant that is asserted and not
  enforced, in the one variable whose whole job is to be trustworthy. C-282's reviewer explicitly
  ranked it "worth a follow-up story rather than a rework", and that judgement is recorded here so
  the next reader does not re-derive it upward.
- Provenance: the independent review of [C-282](C-282-hand-forwarders-bypass-the-posture-floor.md),
  which demonstrated it with a probe rather than arguing it.
- ⚠ Related test gap, worth closing in the same pass: C-282's two refusal tests assert
  `!key.starts_with("FLUX_SANDBOX")` while the filters behind them use `is_posture_env_key`, which
  does not cover `FLUX_SANDBOXED`. They pass only because no fixture names that key. C-282's rework
  reconciles them; check that reconciliation still holds once this story changes what the marker
  guarantees.
- Related: [C-276](C-276-sandbox-posture-does-not-reach-a-child-flux.md) fixed the mirror-image
  defect (marker without posture); [C-262](C-262-fail-closed-unattended-sandbox-profile.md) is what a
  forged marker would let a process skip.
