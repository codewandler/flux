---
id: D-46
title: Plugin pack release pipeline — per-plugin artifacts + signed index
pillar: Core
status: done
epic: plugin-platform-hardening
design: docs/designs/plugin-distribution.md
note: "workflow_dispatch `release-plugins.yml`: build the pack on 5 native runners, package per-plugin archives, emit a minisign-signed `plugins-index.json`, create the `plugins-v<ver>` release (`--latest=false`); core dist release untouched"
---

# Plugin pack release pipeline — per-plugin artifacts + signed index

## Goal
Give the plugin pack its own release channel (the supply side of
[plugin distribution](../designs/plugin-distribution.md)): a `plugins-v<version>` GitHub release
carrying one prebuilt archive per plugin per target plus a signed machine-readable index — produced
without pulling the excluded `plugins/` workspace into the core cargo-dist release or the root gate.

## Acceptance
- [x] A new `.github/workflows/release-plugins.yml` triggered by **`workflow_dispatch`** (inputs:
      `version`, `publish: bool`) — *not* a tag push (the dist-generated `release.yml` tag glob
      `'**[0-9]+.[0-9]+.[0-9]+*'` would match any semver-ish plugins tag and red-X dist's plan job).
      The workflow itself creates the `plugins-v<version>` tag + release via `GITHUB_TOKEN`.
- [x] Build matrix on **native runners** (no cross-compilation): `ubuntu-latest`,
      `ubuntu-24.04-arm`, `macos-15-intel`, `macos-latest`, `windows-latest` → the five core targets. Each
      leg runs `cargo build --release --workspace` in `plugins/` and packages
      `flux-plugin-<name>-<version>-<target>.tar.xz` (`.zip` on windows, binary =
      `flux-plugin-<name>.exe`), one archive per plugin.
- [x] Index generation is a **unit-tested tool**, not inline YAML: a small `pack-index` bin crate in
      the plugins workspace emits `plugins-index.json` per the design's `schema: 1` (pack_version,
      `protocol` = `flux.plugin.v1`, per-plugin per-target `asset`/`sha256`/`size`; asset values are
      **bare file names, never URLs**). Failing-first test `gen_index_matches_schema_and_hashes`
      (fixture: two plugins × two targets; asserts schema shape, hash correctness, and that a
      URL-shaped asset name is rejected).
- [x] The assemble job signs the index (**minisign**, secret key from an Actions secret) and uploads
      `plugins-index.json` + `plugins-index.json.minisig`; a sanity gate fails the run unless asset
      count = plugins × targets and every index entry names an uploaded asset.
- [x] The release is created with **`--latest=false`** — the core installer URL
      `releases/latest/download/flux-cli-installer.sh` keeps resolving to a core release.
- [x] `publish: false` is a dry run: artifacts + index produced and uploaded as workflow artifacts,
      no tag, no release.
- [x] Core plumbing untouched: `release.yml`, `dist-workspace.toml`, root `Cargo.toml`, and the
      existing `ci.yml` jobs are unmodified; the pack stays excluded from the root gate.
- [x] One real (or dry-run) workflow execution attached to the story Progress as evidence.

## Progress
- 2026-07-03 in-progress. Implemented both halves: (1) `plugins/pack-index` — a workspace-member
  bin crate (deliberately not `flux-plugin-*`-named, so the packaging glob never archives it) that
  scans packaged archives and emits `plugins-index.json` (`schema: 1`, deterministic BTreeMap
  ordering, `--released-at` required so the tool reads no clock); enforces bare-asset-names
  (URL/path shapes rejected) and carries the sanity gate (`--expect-plugins/--expect-targets`).
  Tests `gen_index_matches_schema_and_hashes` (bite-verified: neutering the URL check fails it),
  `expectation_gate_catches_missing_leg`, `asset_parsing_is_strict`. (2)
  `.github/workflows/release-plugins.yml` — `workflow_dispatch(version, publish)`, version input
  checked against `workspace.package.version`, 5 native-runner matrix, per-plugin `tar.xz`/`zip`
  (bare binary at archive root — 7z is cd'd into the bin dir), assemble job runs pack-index +
  minisign signing (publish without `MINISIGN_SECRET_KEY` refused; unsigned dry run allowed with a
  notice), `gh release create plugins-v<ver> --latest=false`; dry run uploads the bundle as a
  workflow artifact instead. Local end-to-end evidence: release-built all 17 plugins, ran the
  workflow's packaging loop verbatim (17 archives; `tar -tf` shows the bare binary), then
  `pack-index --expect-plugins 17 --expect-targets 1` — index correct, spot sha256 matches
  `sha256sum` (`e28eb338…`). Plugins gate green (fmt/clippy/build/test, 21 test binaries). Core
  plumbing untouched. Remaining: the workflow-dispatch execution (needs the workflow pushed to
  GitHub; dry run needs no secret).
- 2026-07-03 **DONE — dry-run evidence.** Workflow-dispatch run
  https://github.com/codewandler/flux/actions/runs/28676061794 (version=0.1.0, publish=false)
  green END-TO-END: all 5 build legs + the assemble job — `expecting 17 plugins x 5 targets` →
  `pack-index: wrote ../dist/plugins-index.json (17 plugins × 5 targets)` (85 archives, sanity gate
  passed), bundle uploaded as the dry-run workflow artifact; unsigned (MINISIGN_SECRET_KEY not yet
  set — required before the first `publish: true` run). One runner correction along the way: the
  x86_64-apple-darwin leg starved on the retired `macos-13` label; switched to `macos-15-intel`
  (what dist's core release resolves to) in `75dbe25`, first run cancelled + re-dispatched.

## Notes
- Design: [plugin-distribution](../designs/plugin-distribution.md) — see "Build & release plumbing"
  and "The pack channel" for tag format, asset naming, index schema, and the `--latest=false`
  rationale.
- The pack version is the plugins workspace's lockstep `workspace.package.version`; the workflow
  should verify the `version` input matches it (or bump-check) to keep manifest `version` fields,
  tag, and index agreeing.
- Key custody: the minisign secret key is a repo Actions secret; the public key lands in flux-cli in
  D-47. Generate the keypair as part of this story and record the pubkey in the design doc.
- Document "never hand-push a `plugins-v*` tag" wherever the release process is described
  (a hand-pushed tag red-Xs the dist plan job harmlessly, but noisily).
- `scripts/smoke-plugins.sh` may run env-gated as a post-release validation step — optional, not a
  gate.
