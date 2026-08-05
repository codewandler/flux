---
id: C-558
title: "Restore the Flux release gates after native board and fleet land"
pillar: Core
status: ready
priority: 0
areas: [release, build-ownership, flux-datasource, flux-policy]
depends_on: [C-242, C-549]
note: "release stop-line — remove scanner fixture false positives and bump changed independent protocol crates before v0.56.0"
---

# Restore the Flux release gates after native board and fleet land

## Goal

Make canonical `main` releasable after the native board-and-fleet landing without weakening build
ownership or publishing changed independently-versioned crates under versions already on crates.io.

## Acceptance

- [ ] Failing first, `python scripts/test_build_entrypoints.py` reproduces only the two false
      positives in `scripts/check-release-integrity.sh`'s adversarial `bare-cargo-dist-build`
      fixture. The scanner stops treating Ruby string predicates and replacement literals as
      executable entry points while its self-test still rejects a real unowned `dist build` in a
      generated workflow.
- [ ] `./scripts/check-crate-versions.sh --self-test` and `./scripts/check-crate-versions.sh` pass.
      The changed `codewandler-flux-datasource` and `codewandler-flux-policy` crates receive the
      smallest correct SemVer bumps from 1.3.0 and 1.0.0 respectively, with every workspace,
      plugin, manifest and lockfile dependency identity reconciled. `codewandler-flux-spec` remains
      on its already-bumped 1.4.0 line, and no unrelated independent crate is bumped.
- [ ] Focused regression tests prove the scanner distinction and version-line closure before the
      full repository gate runs. No build-ownership bypass, release-policy exception or
      independently-versioned-crate exemption is introduced.
- [ ] The full repository gate is green from the final committed tree, including embedded public
      documentation fixed-point verification and the exact CI commands that failed on canonical
      `2cb6bbc6947355bc6be91c0847aa1ebb85f46b00`.
- [ ] The story, changelog and generated board record the repair as done, and the repair lands on
      canonical `main` before C-543 integration or the v0.56.0 release promotion begins.

## Progress

- Canonical `main` at `2cb6bbc6947355bc6be91c0847aa1ebb85f46b00` contains the completed native
  board and fleet program, but `python scripts/test_build_entrypoints.py` reports fixture-string
  matches at `scripts/check-release-integrity.sh:240` and `:242`.
- The same commit's `independently-versioned crates moved their version` CI job reports
  `codewandler-flux-datasource` changed since v0.55.0 while still 1.3.0 and
  `codewandler-flux-policy` changed while still 1.0.0. `codewandler-flux-spec` is already 1.4.0.
