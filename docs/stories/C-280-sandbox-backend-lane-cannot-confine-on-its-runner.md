---
id: C-280
title: "The with-a-backend sandbox lane cannot confine on the runner it was given, so it fails every run"
pillar: Core
status: ready
priority: 1
epic: security-assurance
design: docs/designs/security-assurance.md
note: "C-266's lane went red on its FIRST real run — bwrap installs fine but ubuntu-latest (24.04) refuses unprivileged user namespaces. The guard fired exactly as its own comment predicted; main is red until the runner can confine"
---

# The with-a-backend sandbox lane cannot confine on the runner it was given, so it fails every run

## Goal

C-266 added `sandbox-backend`, the lane that proves flux still works **with** a live OS sandbox —
the side of C-262's fail-closed switch that no CI host had ever exercised. Its first real run
(`30564156249`, 2026-07-30) failed, and the failure is not in the diff under test: `ubuntu-latest`
is now Ubuntu 24.04, which ships `kernel.apparmor_restrict_unprivileged_userns=1`. `bubblewrap`
installs and `bwrap --version` succeeds, but the kernel refuses the unprivileged user namespace it
needs, so the backend resolves `Unsupported`:

```
"/usr/bin/bwrap" exists but unprivileged user namespaces are refused by this kernel/policy
```

**The guard worked.** C-266 wrote this exact scenario into the lane's own comment as the trap to
avoid — *"installing bubblewrap is not the same as having it work … which would quietly turn this
lane into a second copy of `check`: green, and proving nothing. If that ever happens, this job says
so loudly instead."* It said so loudly. That is a success of the design and a red `main` for the
repo, and only the second half is this story's problem.

## Acceptance

- [ ] The lane confines for real on its runner: `flux doctor --json` reports the `sandbox backend`
      check as `PASS`, and both `a_promised_backend_is_real_and_functional` and
      `an_auto_approved_turn_runs_its_children_inside_the_sandbox` go green in CI.
- [ ] **The chosen mechanism is argued, not just applied.** At least these three are real options and
      they are not equivalent:
      - `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` in the lane's install step —
        smallest change, but it means the lane proves confinement on a host reconfigured to permit
        it, which is worth saying out loud.
      - pin the job to `ubuntu-22.04`, which has no such restriction — but that runner has an
        announced retirement, so it buys time rather than a fix.
      - run the lane inside a container that provides the namespace itself.
      Pick one, state why, and say what it costs.
- [ ] The lane's existing comment block is updated so it records what *happened* rather than what
      might: the trap it names is no longer hypothetical, and the next reader should learn that
      `ubuntu-latest` needed work to host this lane at all.
- [ ] **The vacuity guard stays load-bearing.** Whatever fix lands must not weaken
      `a_promised_backend_is_real_and_functional` into tolerating a `WARN`. That test failing is the
      only reason this defect was visible instead of silently green; a fix that relaxes it converts a
      loud lane back into the decorative one C-266 existed to prevent.
- [ ] A note in the story states plainly whether the fix is durable against the next runner-image
      bump, or whether it will need revisiting — `ubuntu-latest` moves under us by design.

## Progress

- (not started)

## Notes

- Evidence, from run `30564156249`, job `fail-closed sandbox switch · with a real backend`:
  ```
  test a_promised_backend_is_real_and_functional ... FAILED
    left: String("WARN")   right: "PASS"
  test an_auto_approved_turn_runs_its_children_inside_the_sandbox ... FAILED
    `require` + a real backend must START, not fail closed.
  ```
- ⚠ **This lane is invisible to the local gate.** `crates/flux-cli/tests/sandbox_backend.rs` is gated
  on `FLUX_TEST_SANDBOX_BACKEND=1` — deliberately, the way the Postgres suites are gated — so
  `cargo test --workspace` skips it on a developer machine even though this box *does* have a working
  bwrap. Reproduce with `FLUX_TEST_SANDBOX_BACKEND=1 cargo test -p flux-cli --test sandbox_backend`.
  A developer machine will pass it, which is exactly why it went unnoticed until CI ran.
- ⚠ `main` is red on this until it lands. The only other red job is the pre-existing
  `published host-kit is not behind the live protocol version` (the pack-release debt on the C-143
  line), which is unrelated and user-owned.
- Related: [C-266](C-266-sandbox-backend-ci-coverage.md) built the lane and predicted this failure
  mode in its own comment; [C-262](C-262-fail-closed-unattended-sandbox-profile.md) installed the
  switch the lane exists to prove.
