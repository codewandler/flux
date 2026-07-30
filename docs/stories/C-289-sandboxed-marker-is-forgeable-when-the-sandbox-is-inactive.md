---
id: C-289
title: "`FLUX_SANDBOXED` is forgeable by any call site whenever the sandbox is inactive — the project's default posture"
pillar: Core
status: in-progress
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

- [x] A failing-first test demonstrates it concretely: a spawn through an **inactive** sandbox with
      `FLUX_SANDBOXED=1` in the caller-supplied env, and the child receiving it. Assert on the child's
      own environment, not on the parent's intent.
- [x] The marker becomes unforgeable in both directions, or the limit is stated and enforced. The
      obvious shape is that `build_command` **removes** the key when `sandbox_marker` yields `None`,
      rather than leaving whatever the caller put there — but weigh it: is there a legitimate caller
      that must assert confinement it did not perform? `Confinement::Exempt` and the
      `AlreadyConfined` backend are the two places to look before assuming the answer is no.
- [x] ⚠ Do not break `Backend::AlreadyConfined`. A process running inside an outer flux sandbox
      legitimately *is* confined without having wrapped anything itself, and that is exactly the case
      the marker exists to communicate. A fix that clears the marker unconditionally would make
      nested flux re-wrap inside its own containment. Whatever lands must distinguish "nobody
      confined this" from "someone else already did".
- [x] The three comments that currently assert the false version are corrected — C-282 corrected them
      to describe the limit; this story either removes the limit or makes the description permanent.
      `crates/flux-system/src/sandbox.rs:933-935`, `crates/flux-orchestrate/src/worker.rs:302-304`,
      `crates/flux-system/src/lib.rs:2128`.
- [x] Revisit whether `FLUX_SANDBOXED` should now be in `POSTURE_ENV_KEYS`. C-282 deliberately left
      it out on the strength of the "cannot be forged" claim. If that claim becomes true, the reason
      for the omission changes; if it stays false, the omission needs a different reason.
- [x] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap` and
      `FLUX_TEST_SANDBOX_BACKEND=1 cargo test -p flux-cli --test sandbox_backend`.

## Progress

- **The shape that landed.** `sandbox_marker` no longer answers "should I write a marker?" with an
  `Option`; it answers "is this child confined?" with a two-valued `Marker::{Confined, Unconfined}`,
  and `build_command` applies *both* answers after the caller-override slot — `cmd.env(MARKER_ENV,
  "1")` or `cmd.env_remove(MARKER_ENV)`. That is what makes the marker unforgeable in both
  directions: a call site can neither claim a confinement that did not happen nor talk a real one
  down (`FLUX_SANDBOXED=0` in the override slot is now overwritten, not honored).
- **How `AlreadyConfined` survives it.** A child counts as confined when *either* this spawn is
  wrapped (`Sandboxed` + `sandbox.is_active()`) *or* an outer flux sandbox already confines this
  whole process tree (`Sandbox::confined_by_parent()`, or the truthy ambient `FLUX_SANDBOXED` that
  resolves to it). The second clause is what distinguishes "nobody confined this" from "someone else
  already did", and it holds for `Confinement::Exempt` spawns too — the local-eval child host and
  `spawn_debug_pipe` sit inside an outer boundary exactly as a wrapped spawn does.
- **`Exempt` is not a legitimate forger.** The Acceptance asked whether some caller must assert
  confinement it did not perform. No: the exempt call sites either sit inside a real outer boundary
  (covered by the clause above, without anyone asserting anything) or sit inside none, in which case
  the child *must* confine its own descendants — which is precisely what `flux-eval`'s runner and
  `flux-orchestrate`'s `worker_env` already refuse to let a fixture override.
- **Why the outer-confinement clause reads `std::env` when `posture_env` deliberately does not.** A
  posture is *configuration* an embedder may pin against the ambient env (`System::with_sandbox`), so
  reading it back from the environment ships the wrong one — C-276's rework. Whether this process is
  inside somebody else's namespaces is *inherited process state*: no pinned `Sandbox` makes it true
  or false, and `Sandbox::resolve` already trusts exactly that source for exactly that question. Read
  through `env_truthy`, so the two can never disagree on the falsy spelling.
- **`FLUX_SANDBOXED` removed from `SAFE_ENV`.** It used to ride the allow-list like any other
  forwarded value, which is what left it forgeable — the override slot landed after it and nothing
  landed after *that* unless the spawn was wrapped. The marker is now single-sourced through
  `sandbox_marker`, which reads the ambient value itself. Behaviour for a genuinely nested process is
  unchanged; D-130's `flux_sandboxed_marker_survives_env_clear_like_other_safe_env_entries` still
  passes, and its doc now names the observable rather than the mechanism.
- **`POSTURE_ENV_KEYS` unchanged (Acceptance item 5).** The marker stays out, and the reason is now
  stronger rather than weaker: `POSTURE_ENV_KEYS` is defined as exactly what `posture_env` emits (a
  completeness test holds that), and the marker is not emitted there at all — it is rendered past the
  override slot, so no posture filter needs to reach it. `SANDBOX_ENV_KEYS` keeps it, but its
  rationale is rewritten: it is no longer what stops a forgery, it is what keeps a forwarder from
  describing a child environment it cannot produce, and what lets `flux-eval` name the refusal to an
  operator instead of letting the value vanish a layer down.
- **Tests.** `crates/flux-system/src/lib.rs`: `an_inactive_sandbox_refuses_a_forged_confinement_marker`
  (the failing-first one, reproducing C-282's reviewer's probe output verbatim),
  `an_outer_flux_sandbox_still_marks_its_descendants_and_a_caller_cannot_clear_it`,
  `an_exempt_spawn_inherits_outer_confinement_but_forges_nothing_without_it`.
  `crates/flux-system/src/sandbox.rs`: three `sandbox_marker` unit tests, including the
  `Backend::AlreadyConfined` and falsy-ambient cases.
- **Related test gap from the story's ⚠ note.** C-282's two refusal tests still assert
  `!key.starts_with("FLUX_SANDBOX")` against a filter built on `is_sandbox_env_key`, and that
  reconciliation still holds — `SANDBOX_ENV_KEYS` continues to carry the marker, so the assertion and
  the filter cover the same set. Their *docs* were the stale part and are corrected.

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
