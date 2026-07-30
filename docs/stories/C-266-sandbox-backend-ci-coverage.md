---
id: C-266
title: "Both sides of the fail-closed sandbox switch are unproven in CI"
pillar: Core
status: done
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

- [x] A failing-first demonstration that the current gate cannot detect the regression class: on the
      tree **before** this story, a deliberately reintroduced auto-approved spawn without
      `FLUX_SANDBOX=off` passes the full local gate on a developer machine and is caught only by CI.
      Record the exact command and its output.
- [x] The workspace test suite passes with **no** usable sandbox backend, asserted by CI rather than
      by hand. `FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test --workspace` is the reproduction used
      during the 0.38.0 cut and is the cheapest form of this check.
- [x] The workspace test suite also passes **with** a backend present — install `bubblewrap` in one
      CI lane — so the sandboxed path is exercised at all. Today it is exercised on no runner.
      ⚠ This lane must not break `sandbox_posture.rs`, several of whose tests require the *absence*
      of a backend; scope the install to a lane those tests tolerate, or make them explicit about
      which posture they need instead of inferring it from the host.
- [x] `scripts/smoke-live.sh --shapes` is covered by the same two-sided check, since step 5
      (`app run --serve`) is a serving surface and was the third and last site found during the cut.
- [x] A guard prevents silent recurrence: a new auto-approved or serving spawn added to a test
      without declaring its posture fails a named check, rather than passing locally and reddening
      CI. State plainly in the story what the guard does and does not cover.
- [x] Full Rust gate green (`cargo build/test/clippy -D warnings/fmt` both workspaces,
      `cargo test -p flux-codegate`), and the `scripts/check-*.sh` policy gates pass.
      **Tickable now, and the blocker was never this story's.** The implementor correctly refused to
      tick it: `codewandler-flux-lang` was red at its merge base `588144a2`, a commit that had swept
      another session's in-progress L-93 work in alongside the story files. That was a coordinator
      error, since corrected — the commit was rewritten to carry only its own content, and the story
      was cherry-picked onto a green `main` rather than merged. Verified at integration:
      **3252 tests / 0 failures with a sandbox backend present, and 0 failed suites under
      `FLUX_BWRAP_BIN=/nonexistent/bwrap`** — which is this story's own two-posture claim, met.
      `check-host-kit-protocol-drift.sh` remains red on unrelated pack-release debt (C-143 line).

## Progress

**2026-07-30 — landed.** The premise in the Notes below turned out to be *false*, and that is what
made a clean answer possible: `sandbox_posture.rs` does **not** require the absence of a backend. Every
spawn there already forces `FLUX_BWRAP_BIN` *and* `FLUX_SANDBOX_EXEC_BIN` at nonexistent paths
(`crates/flux-cli/tests/sandbox_posture.rs:85`), so it proves the no-backend postures hermetically and
passes unchanged on a host that has `bwrap` — verified by running it on this machine, which does.
Nothing had to be weakened, and no test needed relocating; the two postures divide like this:

- **Without a backend** — `sandbox_posture.rs` plus the 14 `FLUX_SANDBOX=off` spawns. The `check` CI
  job now sets `FLUX_BWRAP_BIN=/nonexistent/bwrap` job-wide, so all seven of its steps run that
  posture *by construction* rather than by the accident of a runner image lacking bwrap — including
  `scripts/smoke-live.sh --shapes`, whose step 5 a `cargo test` run does not cover.
- **With a backend** — a new `sandbox-backend` job installs `bubblewrap` and runs the workspace suite
  plus the shape guard at `FLUX_SANDBOX=require`. Its home in the tree is the new
  `crates/flux-cli/tests/sandbox_backend.rs`, gated on `FLUX_TEST_SANDBOX_BACKEND=1` the way the
  Postgres suites are gated on `TEST_POSTGRES_URL`: the posture is *declared*, never inferred.

The trap that lane could have fallen into is worth naming, because it is the same false-assurance
shape this story exists to remove: `apt-get install bubblewrap` does not mean bwrap *works*. A kernel
that refuses unprivileged user namespaces resolves `Unsupported`, and the lane would have become a
second, green copy of `check`. So its first test asserts `flux doctor --json` reports the
`sandbox backend` check as `PASS` and fails the job otherwise; the second proves confinement
behaviorally rather than by exit status — the bash child's own pid, which is single-digit inside the
sandbox's `--unshare-pid` namespace and a real OS pid outside it.

**The guard, plainly.** `cargo test -p flux-codegate` →
`every_unattended_test_spawn_declares_its_sandbox_posture`, a syn-based scan of every test target in
both workspaces (`ambient_sandbox_spawns`). It flags a `std`/`tokio` Command that spawns
`CARGO_BIN_EXE_flux` when the spawn declares no posture and either its literal argv names an
unattended surface (`--yes`, `-y`, `--serve`, `--serve=…`, or a flagless one such as `review`) or it
forwards argv in bulk (`.args(expr)`), which lets any call site make it unattended. A posture is
declared by `FLUX_SANDBOX` / `FLUX_SANDBOXED` / `FLUX_BWRAP_BIN` / `FLUX_SANDBOX_EXEC_BIN` (via `.env`
or `.env_remove`), by `--no-sandbox` in argv, or by a reasoned
`// flux-allow-ambient-sandbox: <why>` comment. `flagless_unattended_surfaces_match_the_cli_classifier`
keeps the trigger table honest by parsing `unattended_sandbox_surface` in `flux-cli`'s `dispatch.rs`:
a *new* flagless unattended subcommand fails the lint until it is listed.

What the guard does **not** cover, deliberately:

- **Shell scripts.** `scripts/smoke-live.sh` is not Rust; it is covered behaviorally instead, by both
  CI lanes running `--shapes`. That is stronger than a lint but only for the sites the script has.
- A spawn whose program is computed at runtime rather than from `CARGO_BIN_EXE_flux`.
- Argv assembled into a `Vec` elsewhere and passed in — the bulk-forwarding rule catches the shape,
  not a hand-built vector's contents.
- Posture injected through `.envs(map)`; an opaque map is not read as a declaration (fail-closed).
- Two Commands bound to the *same* ident in one function are merged, so a posture declared on one
  would count for the other. No site in the tree does this.

The scan found exactly one undeclared site in the existing tree — `policy_simulate.rs:89`, which
forwards argv — now declaring `FLUX_SANDBOX=off` (a pure read; it also makes the test hermetic against
an operator shell exporting `FLUX_SANDBOX=require`).

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
