---
id: C-280
title: "The with-a-backend sandbox lane cannot confine on the runner it was given, so it fails every run"
pillar: Core
status: done
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

- [x] The lane confines for real on its runner: `flux doctor --json` reports the `sandbox backend`
      check as `PASS`, and both `a_promised_backend_is_real_and_functional` and
      `an_auto_approved_turn_runs_its_children_inside_the_sandbox` go green in CI.
      **Ticked from run output, not from a diff review** — which is what this item existed to insist
      on. CI run `30568751301` (2026-07-30, commit `389f1c95`) reports
      `success  fail-closed sandbox switch · with a real backend`. That is the first time this lane
      has ever been green, and the first time flux's confined path has been exercised on any CI host.
      The implementor left this box unchecked and reported `PARTIAL` rather than tick it from a local
      run — correctly, since a local pass proves nothing here.
- [x] **The chosen mechanism is argued, not just applied.** At least these three are real options and
      they are not equivalent:
      - `sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0` in the lane's install step —
        smallest change, but it means the lane proves confinement on a host reconfigured to permit
        it, which is worth saying out loud.
      - pin the job to `ubuntu-22.04`, which has no such restriction — but that runner has an
        announced retirement, so it buys time rather than a fix.
      - run the lane inside a container that provides the namespace itself.
      Pick one, state why, and say what it costs.
- [x] The lane's existing comment block is updated so it records what *happened* rather than what
      might: the trap it names is no longer hypothetical, and the next reader should learn that
      `ubuntu-latest` needed work to host this lane at all.
- [x] **The vacuity guard stays load-bearing.** Whatever fix lands must not weaken
      `a_promised_backend_is_real_and_functional` into tolerating a `WARN`. That test failing is the
      only reason this defect was visible instead of silently green; a fix that relaxes it converts a
      loud lane back into the decorative one C-266 existed to prevent.
- [x] A note in the story states plainly whether the fix is durable against the next runner-image
      bump, or whether it will need revisiting — `ubuntu-latest` moves under us by design.

## Progress

**The diff.** `.github/workflows/ci.yml`, `sandbox-backend` job only. No Rust file changed, and in
particular `crates/flux-cli/tests/sandbox_backend.rs` is untouched — `git diff main -- crates/` is
empty for this story. Three changes:

1. The job's comment block now records run `30564156249` as history rather than naming the trap as a
   possibility.
2. A new best-effort step clears `kernel.apparmor_restrict_unprivileged_userns`.
3. A new fail-fast step runs `bwrap --unshare-user --unshare-pid --ro-bind / / /bin/true` — a strict
   subset of `bwrap_probe_argv`'s flag set (`crates/flux-system/src/sandbox.rs`), so it cannot fail
   where flux's own probe would have succeeded. It attributes a runner-side failure to the runner in
   seconds instead of after a ten-minute build.

**Why the sysctl and not the alternatives** — the full argument is in the ci.yml comment. In short:
`ubuntu-22.04` is a dated reprieve and would make this the only lane not running on the image the
rest of CI uses; a container is the heaviest option *and* carries a vacuity hazard, because GitHub
container jobs run as root and root can create a user namespace even under the AppArmor restriction,
so the lane would go green for a reason unrelated to the posture flux runs in.

**What it costs, stated plainly.** The lane now proves flux confines on a host *reconfigured to
permit* unprivileged userns, not on a stock `ubuntu-latest`. That is the right subject — the code
under test is flux's confinement path, not Ubuntu's kernel policy — and the refusing-host posture is
not lost, because it resolves to "no backend", which the `check` job asserts hermetically.

**Durability: no, and it is not meant to be.** The knob is Ubuntu/AppArmor-specific and
`ubuntu-latest` moves by design, so this will need revisiting — on the next image bump at the latest.
What is durable is the *failure mode*: the enabling step is deliberately non-fatal, so if the knob
disappears on a host that confines fine the lane still passes, and if the runner genuinely cannot
confine, `a_promised_backend_is_real_and_functional` fails the job with `doctor`'s diagnosis attached.
The fix can rot; it cannot rot quietly.

**Verification — read this before ticking acceptance item 1.**

- **No GitHub runner has executed any of this.** The implementor cannot trigger a CI run. Item 1 is
  unchecked for that reason and must be ticked from run output.
- A local `FLUX_TEST_SANDBOX_BACKEND=1 cargo test -p flux-cli --test sandbox_backend` passes on the
  dev box — **and that is not evidence.** This machine is Manjaro, has no
  `kernel.apparmor_restrict_unprivileged_userns` knob at all, and permits unprivileged userns
  (`/proc/sys/kernel/unprivileged_userns_clone` = 1). Passing here is exactly why the defect reached
  CI unnoticed; it says nothing about a 24.04 runner.
- What *was* proved locally is the diagnosis. Pointing `FLUX_BWRAP_BIN` at a stub that exits 1 with
  `Creating new namespace failed: Operation not permitted` (one of `NAMESPACE_DENIAL_PATTERNS`)
  reproduces run `30564156249` byte-for-byte on this box, at the merge base, with no ci.yml change:
  both tests fail — `a_promised_backend_is_real_and_functional` with `left: String("WARN")` against
  `right: "PASS"`, and `an_auto_approved_turn_runs_its_children_inside_the_sandbox` with the same
  "must START, not fail closed" panic quoted in the Notes below. So the failure is confirmed to be
  the userns denial and not the diff under test — but the *fix* for it lives on the runner, and only
  a runner can confirm it.
- The enabling step's non-fatal path was exercised directly: run under `bash -e` with a `sudo` stub
  that fails the way a missing knob does, the step emits its `::notice::` and exits 0.

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
