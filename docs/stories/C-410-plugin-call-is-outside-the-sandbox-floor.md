---
id: C-410
title: "`flux plugin call` is outside both the sandbox floor and the approval envelope"
pillar: Core
status: done
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
- **Program mode is pinned under both spellings — reversed after review.** The first draft exempted
  an unflagged `flux app run <program>` on the grounds that it installs `DenyApprover`. Review
  rejected that reason and was right; the tree disproves it twice, and a probe measured it:
  - `run_app` calls `assemble_integrations` at startup, which **spawns every installed plugin
    binary** before any journey exists and never consults an approver at all;
  - a program declaring no capability policy dispatches under `LEGACY_JOURNEY_ALLOW` (flux-app's
    `app.rs`), whose eight pre-authorised ops resolve to `PermDecision::Allow` and so never reach
    the approver either (`approval_sensitive`, flux-runtime).

  Measured with a probe plugin under a pinned `$HOME`: unflagged, the plugin subprocess reached the
  network (`curl` exit 0) and wrote outside the workspace; under `--yes` — the same surface, one
  flag apart — both were refused (exit 6, write denied). The honest criterion is C-262's own: a
  `<program.flux>` serves its channels until Ctrl-C and cron/webhook/Slack triggers fire turns with
  **no operator attached**, so there is no human boundary to fall back on and no exemption left.
  `flux run <program.flux>` is the same daemon by another spelling and is pinned with it, through
  one shared `run_targets_a_program` predicate that `async_main` itself calls — they are one
  decision, not two copies of `ends_with(".flux")`.

  What the probe also showed, and what keeps the plugin-spawn argument from proving too much: the
  **interactive** surfaces spawn plugin binaries at startup too (`build_agent_with` calls the same
  `assemble_integrations`), and those children are equally unconfined. That is C-262's accepted
  interactive contract — plugin binaries are trusted dependencies and an operator is present — not
  a further gap this story closes. The arm says so rather than implying otherwise.
- **`app_run_approver` survives as a recorded negative result.** Its doc comment and test module now
  state plainly that it is *not* a sandbox boundary and why, instead of the extraction being deleted
  and the lesson with it. The tests still pin the real `--yes`/deny split.
- **The confined case now discloses what it narrowed.** Every other line `apply_sandbox_env` prints
  fires when confinement is absent or was bypassed; a genuinely confined run said nothing, so the
  first symptom was a child failing with `curl: (6) Could not resolve host` and nothing naming the
  sandbox. One stderr note now names both narrowings, pinned by
  `a_confined_unattended_surface_discloses_what_it_narrowed` in the with-backend lane (the only lane
  that can reach the line).
- **The filesystem narrowing, measured deliberately rather than assumed.** Under `require` a plugin
  subprocess may write only to the workspace, `$TMPDIR` and the toolchain caches — a probe writing
  to a directory outside all three was refused (`exit 1`), while the same write succeeded
  unconfined. **A plugin that keeps state in `~/.config/<vendor>` will newly fail under
  `flux plugin call`**; `[sandbox] writable` is the fix. Documented in `os-sandbox.md` alongside the
  network half, which was the only half the docs previously mentioned.
- **The wildcard cannot come back quietly.** `flux-codegate`'s
  `the_unattended_classifier_covers_every_commands_variant` parses `enum Commands` against the
  classifier and fails on a catch-all arm or an unnamed variant, and
  `the_coverage_scanner_sees_a_wildcard_and_a_missing_variant` proves the scanner sees both on
  fixtures. Verified end to end by re-collapsing the exemptions to `_ => None` and watching it red.
  The fixture now exercises all five spellings of a catch-all (`_`, a bare binding, `x @ _`,
  `_ if cond`, `_ | x`) against the **catch-all** assertion specifically, on fixtures that leave no
  unclassified variant — previously only `_` was covered there and the rest survived on the weaker
  `unclassified()` backstop. Fixing that found a real gap: `is_catch_all` did not recurse into a
  binding's subpattern, so `x @ _` was missed outright.
- The exhaustiveness check paid for itself during the rework: it caught that `flux app run` with
  neither a program nor `--serve` had no arm — a case that is a usage error, and is now classified
  as such rather than left to a fallback.
- **C-266's spawn census learned two new keying kinds.** `FLAGLESS_UNATTENDED_SUBCOMMANDS` now
  carries subcommand *paths* (`"plugin call"`, matched as a contiguous argv window, so
  `plugin ls`/`status`/`refresh` spawns are untouched), and program mode is recognised by
  **argument** via `program_mode_argv` — a bare `run` entry would have demanded a posture
  declaration from every interactive `flux run` spawn in the tree. The drift check partitions arms
  across all three kinds so an arm cannot fall out of it unnoticed. Two true positives, both fixed
  with an explicit `--no-sandbox`: `plugin_preflight_boundary.rs` (C-404) and
  `website_contract.rs`'s tutorial SIGINT test — each would have passed here and refused to start
  on a backend-less runner.
- **The `Commands::Plugin` exemption was narrowed to its one load-bearing clause** — management, not
  operation invocation — with the concession stated rather than buried: `status`/`refresh`/`skill`
  do spawn the plugin binary and those spawns stay unconfined. The line is drawn at what the spawn
  is *for*: a protocol-defined manifest read versus an arbitrary declared operation. Recorded as
  deliberately bounded, with the pointer to where to look if the trusted-dependency assumption is
  ever revisited.
- **The SDK's position is stated at its public surface**: crate root, the `Sandbox` re-export (with
  both worked forms), and both doors' `auto_approve`/`with_sandbox`. `SandboxMode`/`Backend` are
  re-exported as `flux_sdk::sandbox` because without them an embedder could not build the `Require`
  settings the docs tell them to build.
- Gate green after the rework, re-run in full in **both** postures: `cargo test --workspace` with a
  live backend and again under `FLUX_BWRAP_BIN=/nonexistent/bwrap` (the posture CI runs in), plus
  workspace build/clippy/fmt, `-p flux-codegate`, the nested `plugins/` workspace build + fmt, and
  `FLUX_TEST_SANDBOX_BACKEND=1 cargo test -p flux-cli --test sandbox_backend`.
- Not done here (fenced from this story): the CHANGELOG entry, the board row, and the customer-facing
  `WHATS-NEW.md` note. That note is owed and is now larger than it was: `flux plugin call` **and any
  `<program.flux>` run** require a sandbox backend, run with the sandbox network closed, and refuse
  child writes outside the workspace / `$TMPDIR` / toolchain caches.
- Follow-up worth its own story, found while measuring and deliberately not fixed here: the
  interactive surfaces spawn every installed plugin binary at startup, unconfined, before a turn
  begins. That is within C-262's stated interactive contract, but it is undocumented, and it means
  `[tools] disable` and the approval envelope have both already been bypassed by the time the
  operator sees a prompt.
