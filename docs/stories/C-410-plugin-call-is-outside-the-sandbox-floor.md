---
id: C-410
title: "`flux plugin call` is outside both the sandbox floor and the approval envelope"
pillar: Core
status: ready
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

- [ ] **Failing-first**: a test asserting `flux plugin call` runs under the fail-closed posture —
      failing at the merge base, where it runs at `Off`.
- [ ] Each of the three surfaces is **classified explicitly** — pinned to `Require`, or exempt with
      the reason at the definition. No surface is left simply unenumerated.
- [ ] ⚠ A check that fails when a **new** `Commands` variant appears without a classification. The
      defect here is a hand-maintained enumeration drifting from an enum, and this repo has a
      standing scar about guards that only restate their own assumptions — so verify it fires.
- [ ] The SDK's position is documented at its public surface: an embedder owns the floor, or gets
      one.
- [ ] Full gate green in both workspaces.

## Notes

- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F4.
- C-262 is the origin of the fail-closed posture; read it before changing the default.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
