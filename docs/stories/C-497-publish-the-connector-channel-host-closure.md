---
id: C-497
title: "Publish the reusable connector-channel host closure"
pillar: Core
status: in-progress
epic: generated-connector-websocket-channels
design: docs/designs/generated-connector-websocket-channels.md
areas: [flux-app, flux-auth, flux-lsp, flux-server, flux-channels, supply-chain]
note: "Exchange cannot consume path-only flux-channels; publish its five-crate internal closure in dependency order without changing imports or shipped binary names"
---

# Publish the reusable connector-channel host closure

## Goal

Make the generic connector-channel runtime consumable by an independent host from crates.io, without
adding a path/git dependency or turning the Exchange into a Flux checkout composition.

## Acceptance

- [x] A failing-first `cargo publish --dry-run -p flux-channels` proves its path-only `flux-app`
      dependency makes the package unresolvable.
- [x] `flux-app`, `flux-auth`, `flux-lsp`, `flux-server` and `flux-channels` use available
      `codewandler-flux-*` package names and exact release-line workspace pins while their library
      imports and the shipped `flux-lsp` binary name remain unchanged.
- [x] The CI-only publisher and runbook carry all five in machine-checked topological order, and the
      feature-gate ledger follows their published package names.
- [x] Packaging checks, the root gate and both sandbox postures pass before the release is cut.
- [ ] The tag-triggered publisher confirms every new crate/version on crates.io before a dependent
      repository moves to the new Flux line.

## Progress

- 2026-08-03: The generic runtime passes the affected Flux tests on the v0.53 baseline, but a dry-run
  package failed before compilation because `flux-app` had no registry version. The full unpublished
  closure is app, auth, LSP, server and channels; all five vanity-prefixed names were unclaimed when
  checked through Cargo.

- 2026-08-03: metadata preserves `flux_app`, `flux_auth`, `flux_lsp`, `flux_server` and
  `flux_channels` imports plus the `flux-lsp` executable. The 34-crate publisher order passes
  codegate's registry-closure test; auth, LSP and app each pass a complete crates.io dry-run against
  the already-published v0.53 dependency line.

- 2026-08-03: server and channels package-file censuses pass; their complete local dry-runs correctly
  stop at the not-yet-published preceding crate, which is why the CI publisher is topological. The
  workspace gate, codegate, missing-bubblewrap posture and real confined backend suite all pass.

## Notes

- Publication remains CI-only. A local `cargo publish --dry-run` is evidence; a local upload is not
  an allowed release path.
- This expands distribution only. It does not make the CLI, TUI, evaluator or codegate reusable API
  promises.
