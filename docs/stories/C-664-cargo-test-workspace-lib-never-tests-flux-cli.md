---
id: C-664
title: "cargo test --workspace --lib never tests flux-cli"
pillar: "Core"
status: done
areas: [flux-codegate]
epic: fleet-harness-throughput
---

# cargo test --workspace --lib never tests flux-cli

## Goal

`cargo test --workspace --lib` runs no `flux-cli` tests at all. `flux-cli` is a binary crate with no
library target, so `--lib` silently matches nothing there — and `flux-cli` holds
`board_fleet_cmd.rs`, which is the board and fleet implementation plus 392 of its tests.

This is a gate hole, not a preference. `task install` runs exactly that command, so the install gate
has never covered the crate most changed by fleet work. The tests only run when someone remembers
`--bins`.

## Acceptance

- [x] The install/verification path runs `flux-cli`'s tests; `--lib` alone must not be what decides
      whether they execute.
- [x] A test asserts that every workspace crate carrying tests is reached by the declared gate
      command, so a future binary-only crate cannot fall out of it silently.
- [x] The story records which other crates, if any, were also uncovered.
- [x] `task install` fails when a `flux-cli` test fails.

## Progress

- 2026-08-08 — Fixed and gated. `task test` and `task install` now run
  `cargo test --workspace --lib --bins` at all four declarations (POSIX and Windows × test and
  install), and `flux-codegate`'s
  `declared_test_gate_reaches_every_workspace_crate_that_carries_tests` reads those command lines
  plus `scripts/release-full-gate.sh` and fails if a declared gate stops reaching a test-carrying
  target. Failing first: with `--lib` alone it reports
  `Taskfile.yaml: cargo test --workspace --lib does not select --bins, so the 500 test(s) in
  flux-cli's bin target flux (crates/flux-cli/src/main.rs) never run`.

- 2026-08-08 — **The census, over all 38 root-workspace crates.** Exactly one crate was uncovered:
  **`flux-cli`**, the only member with no library target, and therefore the only one `--lib` skipped
  entirely — 500 unit tests at the time of this change, `board_fleet_cmd.rs` among them. The
  mechanism is that `--lib` is a filter, not a target: `cargo test -p flux-cli --lib` fails loudly
  with `error: no library targets found in package 'flux-cli'`, while the same filter at workspace
  scope skips the package without a word. Measured with `--no-run`, the old command compiled 37
  unit-test binaries and **not one** of them had a `src/main.rs` or `src/bin/` entry point; the new
  one compiles 44, `crates/flux-cli/src/main.rs` included.

- 2026-08-08 — **A second, narrower hole the same census found, which `--bins` does not close.**
  `codewandler-flux-lang`'s `fluxlang` binary (`src/bin/fluxlang.rs`) carries 11 unit tests that
  `--lib` never ran either. It sits behind `required-features = ["cli"]`, so no target-selection
  flag reaches it — neither `--bins` nor an unfiltered `cargo test --workspace`. That class is
  C-308's and is already owned: `scripts/check-feature-gated-tests.sh` runs
  `codewandler-flux-lang/cli` from its ledger. The gate test therefore excludes
  `required-features` targets deliberately rather than claiming a coverage it does not deliver.

- 2026-08-08 — Nothing else was uncovered. The other binaries in the root workspace carry no unit
  tests of their own: `codewandler-flux-lsp`'s `flux-lsp` (`src/main.rs` is an eleven-line stdio
  bootstrap; its 53 tests are the library's, and `--lib` already ran them),
  `codewandler-flux-plugin`'s five fixture plugins, and `codewandler-flux-sdk`'s
  `flux_sdk_plugin_fixture` (also feature-gated). The nested `plugins/` workspace — 20 members, 19
  of them binary-only — is a separate workspace that `cargo test --workspace` in this one never
  addressed; its CI job already runs the unfiltered form.

- 2026-08-08 — Acceptance 4 verified by observation, not by inference: a temporary failing
  `#[test]` added to `crates/flux-cli/src/splash.rs` made the exact command `task install` runs exit
  101 (`test result: FAILED. 0 passed; 1 failed`), which short-circuits the `&&` chain before
  `cargo install`. The probe was removed; `crates/flux-cli/` is unmodified by this story.

## Notes

- The gate test lives in `crates/flux-codegate/src/lib.rs` beside `workspace_respects_layering`,
  because this is the same kind of claim: a structural invariant over the whole workspace that no
  single crate's tests can state. Two helper scanners carry their own pins —
  `cargo_test_scanner_reads_every_dialect_the_gate_files_use` (a comment quoting the command, a
  `-p`-scoped run, `--` handing off to the harness) and
  `module_walk_follows_declarations_rather_than_the_directory`. The second exists because the first
  cut of this census attributed files by directory, which blamed `flux-lsp`'s binary for its
  library's 53 tests and reported a hole that was not there.
- The Taskfile gate stays the fast one: it is `--lib --bins`, not `--all-targets`. Integration
  tests under `tests/` are covered by `scripts/release-full-gate.sh`'s unfiltered
  `cargo test --workspace`, and the gate test asserts that invocation carries no target filter, so
  the coverage this story does not claim cannot quietly disappear either.
