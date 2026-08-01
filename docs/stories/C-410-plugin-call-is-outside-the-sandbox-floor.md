---
id: C-410
title: "`flux plugin call` is outside both the sandbox floor and the approval envelope"
pillar: Core
status: in-progress
priority: 7
epic: connector-platform
areas: [flux-cli, flux-sdk]
note: "F4 of the 2026-08-01 security-posture review at 0.47.1. `unattended_sandbox_surface` has no `Commands::Plugin` arm, so `flux plugin call` runs headless with the sandbox at its `Off` default, no approver, and outside `Executor::dispatch`. C-404's hardening exists precisely because that command prints plugin-authored strings to a terminal"
---

# The surfaces the unattended floor forgot

## Goal

Classify every long-running or headless surface against the fail-closed sandbox posture, instead of
enumerating them by hand and missing some.

`unattended_sandbox_surface` (`crates/flux-cli/src/dispatch.rs:6`) enumerates the surfaces pinned to
the fail-closed `Require` posture. The review read every arm against `enum Commands`
(`crates/flux-cli/src/args.rs:255`): **`Commands::Plugin` has no arm.** So `flux plugin call <name>
<op>` executes a plugin operation headlessly with the sandbox at its `Off` default, no interactive
approver, and — per the crate's own scoping rule — outside `Executor::dispatch` entirely
(`crates/flux-cli/src/plugin_cmd.rs:474`).

Two neighbours share the gap:

- **`flux app run <program.flux>` without `--serve`/`--yes`** is long-running and event-driven (cron
  and webhook triggers) but unclassified;
- **SDK embedders never call `apply_sandbox_env` at all**, building `Sandbox::resolve` directly
  (`crates/flux-sdk/src/envelope.rs:66`, `crates/flux-runtime/src/context.rs:139`), so no unattended
  floor applies to a library consumer under any configuration.

C-404's hardening is the tell that this surface matters: it exists precisely because
`flux plugin call --dry-run` prints plugin-authored strings to an operator's terminal.

## Acceptance

- [x] **Failing-first**: a test asserting `flux plugin call` runs under the fail-closed posture —
      failing at the merge base, where it runs at `Off`.
- [x] Each of the three surfaces is **classified explicitly** — pinned to `Require`, or exempt with
      the reason at the definition. No surface is left simply unenumerated.
- [x] ⚠ A check that fails when a **new** `Commands` variant appears without a classification. The
      defect here is a hand-maintained enumeration drifting from an enum, and this repo has a
      standing scar about guards that only restate their own assumptions — so verify it fires.
- [x] The SDK's position is documented at its public surface: an embedder owns the floor, or gets
      one.
- [x] Full gate green in both workspaces.

## Notes

- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F4.
- C-262 is the origin of the fail-closed posture; read it before changing the default.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
- **`unattended_sandbox_surface` is now exhaustive over `Commands`** — the `_ => None` fallback is
  gone and every one of the 28 variants is named by an arm, pinned or exempt-with-a-reason. That
  makes rustc the primary drift check: a new subcommand does not compile until someone chooses a
  side. Verified it fires by adding a `Commands::C410Probe` variant and watching
  `dispatch.rs:18` red with `non-exhaustive patterns: &args::Commands::C410Probe { .. } not
  covered`, then restoring.
- **`flux plugin call` is pinned to the floor.** It invokes a plugin operation with no approver and
  outside `Executor::dispatch`; it now inherits `require` + closed sandbox network like
  `flux run --yes`. The rest of `flux plugin …` is not: `ls`/`status`/`install`/… are operator-driven
  management, and pinning them would only make plugin management impossible on a backend-less host.
- **`flux app run <program>` without `--serve`/`--yes` is exempt, with the reason at the arm** — it
  installs `DenyApprover`, so every call needing approval is refused rather than auto-allowed. That
  premise used to be an inline `if` inside a 300-line `run_app`; it is now `app_run_approver` with
  `the_unflagged_app_run_approver_denies_every_call` holding it, so flipping the approver breaks a
  named test instead of silently invalidating the exemption.
- **The wildcard cannot come back quietly.** `flux-codegate`'s
  `the_unattended_classifier_covers_every_commands_variant` parses `enum Commands` against the
  classifier and fails on a catch-all arm or an unnamed variant, and
  `the_coverage_scanner_sees_a_wildcard_and_a_missing_variant` proves the scanner sees both on
  fixtures. Verified end to end by re-collapsing the exemptions to `_ => None` and watching it red.
- **C-266's spawn census learned subcommand paths.** `FLAGLESS_UNATTENDED_SUBCOMMANDS` now carries
  `"plugin call"` (matched as a contiguous argv window, so `plugin ls`/`status`/`refresh` spawns are
  untouched). It immediately caught one true positive — `plugin_preflight_boundary.rs`'s C-404
  spawns would have passed here and refused to start on a backend-less runner — now fixed with an
  explicit `--no-sandbox`.
- **The SDK's position is stated at its public surface**: crate root, the `Sandbox` re-export (with
  both worked forms), and both doors' `auto_approve`/`with_sandbox`. `SandboxMode`/`Backend` are
  re-exported as `flux_sdk::sandbox` because without them an embedder could not build the `Require`
  settings the docs tell them to build.
- Gate green: workspace build/test/clippy/fmt, `-p flux-codegate`, the nested `plugins/` workspace
  build + fmt, `FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test --workspace` (the no-backend posture CI
  runs in), and `FLUX_TEST_SANDBOX_BACKEND=1 cargo test -p flux-cli --test sandbox_backend`.
- Not done here (fenced from this story): the CHANGELOG entry, the board row, and the customer-facing
  `WHATS-NEW.md` note that `flux plugin call` now requires a sandbox backend — that last one is
  user-visible and is owed.
