---
id: C-266
title: "Both sides of the fail-closed sandbox switch are unproven in CI"
pillar: Core
status: ready
priority: 2
epic: security-assurance
design: docs/designs/security-assurance.md
note: "SURFACED BY the 0.38.0 cut — C-262's fail-closed switch cost four fix commits because no CI job exercises either side of it; the without-backend path was proven only by hand"
---

# Both sides of the fail-closed sandbox switch are unproven in CI

## Goal

C-262 made auto-approved non-interactive commands and every serving surface refuse to start without
a working OS sandbox backend. That is a *fail-closed security default*, and today CI proves neither
half of it: no job asserts the refusal happens on a host without a backend, and no job asserts flux
still works on a host **with** one. The gap is not theoretical — it cost the 0.38.0 release four
successive fix commits (`040c70cf`, `f244bfc3`, `34f29e4d`, and a doc correction), each found only
by pushing and reading a red CI run, because every developer machine has `bwrap` installed and the
runners have none. This epic's thesis is that the envelope is well-designed but under-proven; this is
that thesis in miniature, on the newest control in the envelope.

## Acceptance

- [ ] A failing-first demonstration that the current gate cannot detect the regression class: on the
      tree **before** this story, a deliberately reintroduced auto-approved spawn without
      `FLUX_SANDBOX=off` passes the full local gate on a developer machine and is caught only by CI.
      Record the exact command and its output.
- [ ] The workspace test suite passes with **no** usable sandbox backend, asserted by CI rather than
      by hand. `FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test --workspace` is the reproduction used
      during the 0.38.0 cut and is the cheapest form of this check.
- [ ] The workspace test suite also passes **with** a backend present — install `bubblewrap` in one
      CI lane — so the sandboxed path is exercised at all. Today it is exercised on no runner.
      ⚠ This lane must not break `sandbox_posture.rs`, several of whose tests require the *absence*
      of a backend; scope the install to a lane those tests tolerate, or make them explicit about
      which posture they need instead of inferring it from the host.
- [ ] `scripts/smoke-live.sh --shapes` is covered by the same two-sided check, since step 5
      (`app run --serve`) is a serving surface and was the third and last site found during the cut.
- [ ] A guard prevents silent recurrence: a new auto-approved or serving spawn added to a test
      without declaring its posture fails a named check, rather than passing locally and reddening
      CI. State plainly in the story what the guard does and does not cover.
- [ ] Full Rust gate green (`cargo build/test/clippy -D warnings/fmt` both workspaces,
      `cargo test -p flux-codegate`), and the seven `scripts/check-*.sh` policy gates pass.

## Progress

- (not started)

## Notes

- The three sites fixed during the 0.38.0 cut, as the inventory of what this must keep green:
  `crates/flux-cli/tests/{agent_lab,export_smoke,fleet_activity_smoke,mock_smoke,role_startup,saved_flows,stream_json_smoke}.rs`
  (14 tests, all now setting `FLUX_SANDBOX=off`) and `scripts/smoke-live.sh`'s `run_shape_checks`.
- Why installing `bwrap` everywhere is the wrong fix: `crates/flux-cli/tests/sandbox_posture.rs`
  owns the posture assertions, and `sandbox_on_without_a_backend_discloses_the_unconfined_posture_on_stderr`
  plus `require_still_fails_closed_instead_of_disclosing_and_continuing` need **no** backend. Both
  postures need a home; that is the design question this story has to answer.
- `check_parses` in `smoke-live.sh` tolerates a non-zero exit by design (it fails only when clap
  never parsed, exit 2), which is why steps 1–4 passed while step 5 failed. Any guard here should not
  assume a non-zero exit means shape drift.
- Related: [C-262](C-262-fail-closed-unattended-sandbox-profile.md) introduced the switch,
  [C-217](C-217-sandbox-posture-disclosure.md) made `on` disclose its resolved posture.
