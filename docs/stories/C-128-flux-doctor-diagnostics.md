---
id: C-128
title: flux doctor — environment & install diagnostics command
pillar: Core
status: done
epic:
design:
note: "one command that checks credentials/OAuth expiry, plugin hash drift (D-48 machinery), sandbox backend availability, events.db + WAL health, egress config sanity, and version skew — each with a fix-it hint; cheap (every check exists as an internal predicate), high leverage for external-beta users"
---

# flux doctor — environment & install diagnostics command

## Goal
One `flux doctor` command that diagnoses a flux install end-to-end and prints actionable fix-it
hints, so external users (the flux-qa beta audience) can self-serve instead of filing
"it doesn't work" reports.

## Acceptance
- [ ] `flux doctor` runs a check suite and reports pass/warn/fail per check with a one-line fix-it
  hint on every non-pass; exit code non-zero iff any check fails.
- [ ] Checks cover at minimum: credential-store entries per configured provider (incl. OAuth token
  expiry), plugin pack signature/hash drift (reusing the D-48 verification), sandbox backend
  availability (bwrap / sandbox-exec probe), `events.db` integrity + WAL size, egress/private-net
  config sanity, and version skew vs the latest release.
- [ ] Every check is hermetic-testable: each has a unit test driving its pass and fail branches
  without live network/credentials (failing-first for the command itself).
- [ ] `--json` output for scripting.

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

### 2026-07-28 — implemented, awaiting review/close
- New module `crates/flux-cli/src/doctor.rs` implements `flux doctor [--json]` end-to-end. Wired
  via `crates/flux-cli/src/args.rs` (`Commands::Doctor { json }`), `crates/flux-cli/src/dispatch.rs`
  (match arm → `run_doctor`), `crates/flux-cli/src/main.rs` (`mod doctor;` + `use doctor::*;`).
- Architecture: `CHECKS: &[CheckDef]` is a plain data table (`name` + `run: fn(&DoctorCtx) ->
  CheckOutcome`) — adding a check is one entry. `run_checks_over` wraps every `run` call in
  `std::panic::catch_unwind` so one panicking probe becomes a `FAIL` row for that check alone,
  never aborts the report (proven by `a_panicking_check_becomes_a_fail_row_without_aborting_the_rest`).
  Every check is split into a pure `judge_*` fn (takes already-collected facts, returns
  `CheckOutcome`; this is what's unit-tested pass/fail/warn) and a thin IO-collecting `check_*`
  wrapper. `CheckOutcome::warn`/`::fail` structurally require a hint string; `::pass` has none —
  so "hint on every non-pass" holds by construction, not by convention.
- Shipped 7 checks (the 6 named in Acceptance + the C-162 addition):
  1. **credentials** — `flux_credentials::auth_status()` + per-provider `load_token` expiry
     (claude/codex OAuth, no-refresh-token case) → warn.
  2. **plugin pack integrity** — `flux_plugin::discover` + `verify_descriptor` (D-48 reuse);
     `Verification::HashDrift` → fail, everything else → pass.
  3. **sandbox backend** — forces `SandboxSettings{mode: On}` through `flux_system::sandbox::
     Sandbox::resolve` so the check reports real availability regardless of the operator's
     configured posture; `[sandbox] require` + unavailable → fail, otherwise unavailable → warn
     (sandboxing is opt-in defense-in-depth).
  4. **events.db integrity** — `PRAGMA integrity_check` via a read-only `rusqlite` connection
     (mirrors the existing read-only pattern in `usage.rs`) + WAL-sibling file size; corruption →
     fail, WAL > 256 MiB → warn.
  5. **egress / private-net config** — pure scan of `flux_config::Config` for any wildcard grant
     (`allow_private_net`, `[private_net] web/plugins/endpoints` = `true` or containing `"*"`) →
     warn.
  6. **version** — reuses `flux_plugin::pack::GithubFetcher`/`Fetcher::list_release_tags` against
     `codewandler/flux` (the same call `flux plugin install` makes), filtered to `v<semver>` tags
     (excludes `plugins-v*`); network failure → warn "could not check" (never fails the suite,
     per the offline-degradation requirement), behind-latest → warn, current → pass.
  7. **tools disable (C-162)** — `ToolRegistry::resolve_disabled` against a registry built from
     `flux_tools::try_register_builtins` only (no plugin spawn — keeps the check fast and
     hermetic); an unmatched pattern → warn, resolved disables → pass (informational).
  - Left a one-line seam comment directly above `CHECKS` for a future "config provenance" check
    once C-165's managed-config tier lands — deliberately NOT implemented now since that config
    layer is a different in-flight session's work and its shape isn't settled yet.
- Exit code: `std::process::exit(1)` iff `any_failed(&reports)` (mirrors the existing
  `flux review --fail-on` pattern in `review.rs`), otherwise `Ok(())` — a WARN never affects it.
- Docs: added a `flux doctor` row + a short "## Diagnostics" section to
  `website/docs/agent/cli.md` (satisfies `cli_reference_covers_every_public_subcommand` in
  `crates/flux-cli/tests/website_contract.rs`).
- Failing-first: wrote the full test module alongside the implementation (36 tests) and confirmed
  each judge function's pass/warn/fail branches before wiring the IO-collecting `check_*` halves;
  additionally verified real end-to-end behavior manually against a fresh `HOME` (all-pass), then
  against a corrupted `events.db` + a bogus `[tools] disable` entry (FAIL + WARN, exit code 1) —
  see Gate below.
- Tests added (`crates/flux-cli/src/doctor.rs`, `#[cfg(test)] mod tests`): 36 tests covering every
  judge function's pass/warn/fail branches, `probe_sqlite_file` against a real fresh sqlite file
  and a bogus non-database file, `check_plugin_pack` end-to-end against a real drifted descriptor
  + binary, `check_sandbox`/`check_tools_disable` against the real host/registry, panic isolation,
  `render_report`/`json_report` shape, and `any_failed`.
- Gate (crate-scoped, `flux-cli`):
  - `cargo build -p flux-cli` — clean.
  - `cargo test -p flux-cli` — 210 lib tests + all integration test binaries green (including
    `cli_command_tree_is_valid`, `help_lists_every_subcommand`,
    `cli_reference_covers_every_public_subcommand`).
  - `cargo clippy -p flux-cli --all-targets --no-deps -- -D warnings` — clean. (Plain
    `cargo clippy -p flux-cli --all-targets -- -D warnings`, without `--no-deps`, currently fails
    on a `doc_lazy_continuation` lint in `crates/flux-app/src/app.rs` — that file is uncommitted,
    in-flight work from a concurrent session (C-183), not touched by this story; confirmed via
    `git status`/`git diff --stat` before attributing it there. Not fixed here per the
    no-cross-session-revert rule.)
  - `cargo fmt -p flux-cli -- --check` — clean (rustfmt applied once to `doctor.rs`).
- Files touched: `crates/flux-cli/src/doctor.rs` (new), `crates/flux-cli/src/args.rs`,
  `crates/flux-cli/src/dispatch.rs`, `crates/flux-cli/src/main.rs`,
  `website/docs/agent/cli.md`.
- Acceptance: all four items appear satisfied by the above (checkboxes intentionally left
  unchecked per this run's orchestration — a coordinating session is closing several stories in
  this batch).

## Notes
- Most checks already exist as internal predicates; this story is mostly assembly + presentation.
