---
id: D-46
title: Plugin pack release pipeline — per-plugin artifacts + signed index
pillar: Core
status: ready
priority: 7
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
- [ ] A new `.github/workflows/release-plugins.yml` triggered by **`workflow_dispatch`** (inputs:
      `version`, `publish: bool`) — *not* a tag push (the dist-generated `release.yml` tag glob
      `'**[0-9]+.[0-9]+.[0-9]+*'` would match any semver-ish plugins tag and red-X dist's plan job).
      The workflow itself creates the `plugins-v<version>` tag + release via `GITHUB_TOKEN`.
- [ ] Build matrix on **native runners** (no cross-compilation): `ubuntu-latest`,
      `ubuntu-24.04-arm`, `macos-13`, `macos-latest`, `windows-latest` → the five core targets. Each
      leg runs `cargo build --release --workspace` in `plugins/` and packages
      `flux-plugin-<name>-<version>-<target>.tar.xz` (`.zip` on windows, binary =
      `flux-plugin-<name>.exe`), one archive per plugin.
- [ ] Index generation is a **unit-tested tool**, not inline YAML: a small `pack-index` bin crate in
      the plugins workspace emits `plugins-index.json` per the design's `schema: 1` (pack_version,
      `protocol` = `flux.plugin.v1`, per-plugin per-target `asset`/`sha256`/`size`; asset values are
      **bare file names, never URLs**). Failing-first test `gen_index_matches_schema_and_hashes`
      (fixture: two plugins × two targets; asserts schema shape, hash correctness, and that a
      URL-shaped asset name is rejected).
- [ ] The assemble job signs the index (**minisign**, secret key from an Actions secret) and uploads
      `plugins-index.json` + `plugins-index.json.minisig`; a sanity gate fails the run unless asset
      count = plugins × targets and every index entry names an uploaded asset.
- [ ] The release is created with **`--latest=false`** — the core installer URL
      `releases/latest/download/flux-cli-installer.sh` keeps resolving to a core release.
- [ ] `publish: false` is a dry run: artifacts + index produced and uploaded as workflow artifacts,
      no tag, no release.
- [ ] Core plumbing untouched: `release.yml`, `dist-workspace.toml`, root `Cargo.toml`, and the
      existing `ci.yml` jobs are unmodified; the pack stays excluded from the root gate.
- [ ] One real (or dry-run) workflow execution attached to the story Progress as evidence.

## Progress
- (not started)

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
