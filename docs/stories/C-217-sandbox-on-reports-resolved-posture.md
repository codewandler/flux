---
id: C-217
title: "`sandbox on` reports its resolved posture instead of degrading silently"
pillar: Core
status: in-progress
priority: 7
epic: security-assurance
design: docs/designs/security-assurance.md
note: "the prerequisite the epic deferred the default-flip behind — `on` + no backend returns Ok and says nothing, so an operator who asked to be sandboxed is told nothing when they are not"
---

# `sandbox on` reports its resolved posture instead of degrading silently

## Goal
`SandboxMode::On` means "confine when a backend is available, warn and continue when it isn't". The
continue half works. **The warn half does not exist.** `Sandbox::resolve` records
`Backend::Unsupported { reason }` and `ensure_available` returns `Ok(())` without emitting anything
— pinned, today, by `ensure_available_is_ok_under_on_mode_when_unsupported`. An operator who
configured `on` and is running completely unconfined learns this only if they independently think to
run `flux doctor`.

Make the resolved posture unasked-for output. This is the step the security-assurance epic named as
the **prerequisite** for revisiting the sandbox default: flipping the default to `on` while `on` can
silently mean "unconfined" would manufacture false assurance, which is strictly worse than today's
honest `off`.

## Acceptance
- [x] Under `SandboxMode::On` with an `Unsupported` backend, flux emits **one prominent,
      user-visible line** naming the **resolved** posture and the reason — e.g. "sandbox: requested
      `on`, running UNCONFINED — bubblewrap not available: <reason>". It states what is true, not
      what was asked for. A `tracing::debug` does not satisfy this; the whole defect is that the
      information already exists and nobody sees it.
- [x] **Failing-first test**: assert the disclosure is emitted for `on` + `Unsupported`. ~~It fails
      today because nothing is emitted at all.~~ **Premise corrected — see Progress 2026-07-30:**
      the CLI already emitted an (untested) line; what was missing was the L2 accessor, the
      truth-first wording, and any test at all.
- [x] Emitted **once per process**, not per spawn. A per-`wrap_argv` warning would bury the signal
      in exactly the sessions that spawn most.
- [x] **Silent when confinement actually holds**: no disclosure when a backend is active, and none
      when this process is already confined by an outer flux sandbox (the `FLUX_SANDBOXED` marker
      path that `resolve_under_flux_sandboxed_marker_is_confined_by_parent_and_satisfies_require`
      covers). A warning that fires when nothing is wrong trains operators to ignore it.
- [x] `require` is untouched — it already fails closed via `ensure_available`, and its behaviour and
      tests must not move.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — filed to discharge the security-assurance epic's explicitly deferred item. The
  deferral reasoning is recorded in
  [security-assurance.md](../designs/security-assurance.md) § "Explicitly deferred: the sandbox
  default"; this story is step 1 of the two-step sequence recorded there.

- 2026-07-30 — **the premise was wrong, and the corrected version is narrower.** This story (and the
  epic design's note, and `flux doctor`-era commentary) all assert that `on` + no backend "emits
  nothing at all". That is true of **flux-system (L2)** and false of the **CLI (L5)**: since at least
  `aed01f7e`, `apply_sandbox_env` has printed a styled one-per-process stderr line —
  `warning: OS sandbox requested but unavailable (<reason>): shell/plugin processes run WITHOUT
  OS-level confinement this run. …`. The premise was verified against `sandbox.rs` only, so the
  `format!` one layer up was missed. Verified by planting this story's final test file on the merge
  base (`cedef3f4`) and running it there: 4 of 6 tests fail, and **every one fails on wording** —
  `discloses_unconfined` looks for the tokens `UNCONFINED` and `sandbox:`, neither of which the
  merge-base line contains. So the honest failing-first claim is *"the line did not name the resolved
  posture, was not exposed at L2, and was untested"* — **not** "nothing was emitted". The two tests
  that already pass on the merge base (`require_still_fails_closed_…` and
  `no_disclosure_when_the_sandbox_is_off_or_confinement_is_inherited`) are regression pins by design:
  they assert behaviour that must NOT move.

  What was genuinely missing, and is what this story actually shipped:
  1. **No test anywhere pinned it.** `aed01f7e` moved that line between modules with zero coverage;
     nothing would have caught its deletion. This was the real exposure.
  2. **The fact lived only in an L5 `format!`.** Every other consumer that resolves a `Sandbox` —
     `flux-sdk` (`envelope.rs`, `flow.rs`), `flux-app`, `flux-eval`, `flux-runtime` — disclosed
     nothing, because the posture was never exposed as data.
  3. **The wording led with the request, not the truth,** and was framed as a fault — wrong for the
     dominant `NamespacesDenied` case, which is a healthy expected state.

  Kept the ✅ on the Acceptance items because each is now true and tested; corrected the one item
  whose stated *justification* was false rather than silently ticking it.

- 2026-07-30 — **layering decision.** L2 exposes the posture as data
  (`Sandbox::posture_disclosure` → `Option<String>`, plus `take_posture_disclosure` for the
  once-per-process latch); L5 (`apply_sandbox_env`) decides *where* it goes and emits it.
  `flux-system` never reaches upward for an output surface, so the L0→L6 rule holds
  (`cargo test -p flux-codegate` green). The latch is a process-global `AtomicBool` rather than
  per-instance because `Sandbox` is `Clone` and a process resolves several; a sandbox with **nothing**
  to disclose deliberately does not consume it, or the first `Sandbox::disabled()` of any hermetic
  `System::new` would burn the latch and silence the real disclosure.

- 2026-07-30 — **output-routing decision: stderr, unconditionally; NOT suppressed under
  machine-readable modes.** Rationale, in order of weight:
  1. stderr is the channel this CLI already reserves for diagnostics precisely so stdout stays
     parseable, so the line **structurally cannot** corrupt a machine-readable parse. Using the
     existing human/machine split rather than inventing a suppression mechanism.
  2. Suppressing under `--json`/`--stream-json` would silence exactly the operator who most needs
     it — an unattended/daemon deployment — and would require enumerating every per-subcommand
     `--json` flag, a list that silently rots.
  3. Eval stays quiet **on its own merits, not by special-casing**: `flux-eval` resolves
     `SandboxSettings::from_env()`, and nothing in the eval path requests confinement, so the mode is
     `Off` and an `Off` sandbox has nothing to disclose. If an operator *does* set `FLUX_SANDBOX=on`
     for a containerised run, they get one stderr line per process — the same volume the merge base
     already produced, so this is not a regression in eval noise.

  Proven by test, not by assertion: `the_disclosure_does_not_pollute_stream_json_stdout` parses
  **every** stdout line of a real `--stream-json` run as JSON (with a `lines > 0` non-vacuity guard),
  and `the_disclosure_does_not_pollute_json_stdout` parses `doctor --json` stdout as one document and
  asserts a real `checks` report. `doctor --json` was chosen for the second because it is also the
  **on-demand** sandbox surface, so it simultaneously shows the unasked-for disclosure did not
  disturb the asked-for one (`check_sandbox` is untouched — no duplication).

- 2026-07-30 — **what changed about `ensure_available_is_ok_under_on_mode_when_unsupported`, and why
  it is not a weakening.** Its `assert!(sandbox.ensure_available().is_ok())` is **unchanged** — the
  "continue" half of `on` is byte-for-byte what it was. The test gained one assertion, that the same
  sandbox now also has a posture disclosure to offer. So the test no longer pins *silence*; it pins
  *continue + disclose*. Confinement is untouched in both directions: `require` still fails closed
  through the same branch, and `on` still continues. Reporting is purely additive — the envelope did
  not move. Both halves are asserted in one test on purpose, so a future change cannot restore the
  silence by deleting only the disclosure assertion.

## Notes
- Seams: `Sandbox::resolve` and `ensure_available` (`crates/flux-system/src/sandbox.rs`);
  `discover_backend` maps every failure into `Backend::Unsupported { reason }`, so **the reason
  string already exists** — this story is about surfacing it, not about computing it.
- ⚠ **`NamespacesDenied` is an expected, healthy state, not a fault.** Default-seccomp Docker,
  Debian ≤11's sysctl, and Ubuntu 23.10+'s AppArmor userns restriction all land there, and the
  terminal-bench eval containers run in exactly that configuration. So: word the disclosure as a
  posture statement rather than an error, and **check it does not pollute eval or `--json` output**
  — a line on every containerised eval run that nobody can act on is noise, and noise is what gets
  filtered out and then missed when it matters. Routing it to stderr, and suppressing it under
  machine-readable output modes, is the likely shape; decide it explicitly.
- `flux doctor` already has a `sandbox backend` check (`check_sandbox` / `judge_sandbox` in
  `crates/flux-cli/src/doctor.rs`). That stays the **on-demand** surface and should not be
  duplicated. This story adds the disclosure the operator did not have to ask for.
- **Out of scope, deliberately: flipping the default.** That is step 2 and needs its own design doc
  covering the Windows gap — no backend exists there; only Bubblewrap and Seatbelt are implemented —
  plus the migration story. Do not fold it in here; the epic's reasoning is that step 1 must ship
  first so that a default of `on` cannot quietly mean "unconfined".
