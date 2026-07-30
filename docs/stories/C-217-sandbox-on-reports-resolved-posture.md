---
id: C-217
title: "`sandbox on` reports its resolved posture instead of degrading silently"
pillar: Core
status: ready
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
- [ ] Under `SandboxMode::On` with an `Unsupported` backend, flux emits **one prominent,
      user-visible line** naming the **resolved** posture and the reason — e.g. "sandbox: requested
      `on`, running UNCONFINED — bubblewrap not available: <reason>". It states what is true, not
      what was asked for. A `tracing::debug` does not satisfy this; the whole defect is that the
      information already exists and nobody sees it.
- [ ] **Failing-first test**: assert the disclosure is emitted for `on` + `Unsupported`. It fails
      today because nothing is emitted at all.
- [ ] Emitted **once per process**, not per spawn. A per-`wrap_argv` warning would bury the signal
      in exactly the sessions that spawn most.
- [ ] **Silent when confinement actually holds**: no disclosure when a backend is active, and none
      when this process is already confined by an outer flux sandbox (the `FLUX_SANDBOXED` marker
      path that `resolve_under_flux_sandboxed_marker_is_confined_by_parent_and_satisfies_require`
      covers). A warning that fires when nothing is wrong trains operators to ignore it.
- [ ] `require` is untouched — it already fails closed via `ensure_available`, and its behaviour and
      tests must not move.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — filed to discharge the security-assurance epic's explicitly deferred item. The
  deferral reasoning is recorded in
  [security-assurance.md](../designs/security-assurance.md) § "Explicitly deferred: the sandbox
  default"; this story is step 1 of the two-step sequence recorded there.

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
