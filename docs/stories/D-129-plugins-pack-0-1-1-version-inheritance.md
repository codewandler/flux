---
id: D-129
title: "Plugins pack 0.1.1 — ship the slack fixes; manifests inherit the workspace version"
pillar: Core
status: done
note: "delivery vehicle for D-127/D-128; also kills the hand-maintained per-plugin manifest version strings (kubernetes already drifted to 0.2.0 vs its 0.1.0 descriptor)"
---

# Plugins pack 0.1.1 — ship the slack fixes; manifests inherit the workspace version

## Goal
Cut the `plugins-v0.1.1` pack release so installed plugins get the slack fixes (D-127 mrkdwn
panic, D-128 broken file upload), and make per-plugin manifest versions **inherit the pack
workspace version** (`env!("CARGO_PKG_VERSION")` — every plugin crate already has
`version.workspace = true`) instead of hand-maintained string literals. The literals were already
drifting: kubernetes self-reported `0.2.0` against its `0.1.0` descriptor, the exact mismatch
`flux plugin status` warns about, and a pack cut would otherwise need 19 hand-edits every time.

## Acceptance
- [x] All 19 plugin `manifest_builder`s report `env!("CARGO_PKG_VERSION")`; no hardcoded manifest
      version strings remain in `plugins/*/src/main.rs`.
- [x] Plugins workspace bumped to 0.1.1; full workspace tests + fmt + clippy green.
- [x] `release-plugins` workflow dispatched with `version=0.1.1, publish=true` (never a hand-pushed
      tag); the signed `plugins-v0.1.1` release exists with 19 plugins × 5 targets + signed index.
- [x] Post-release: `flux plugin install slack` resolves 0.1.1, `plugin status slack` shows
      v0.1.1 with **no** descriptor/manifest version mismatch, and the default-markdown
      `slack.message.list` live-proof passes on the pack binary.

## Progress
- 2026-07-10 filed; manifest-version inheritance + 0.1.1 bump implemented, plugins workspace gate
  green locally.
- 2026-07-10 **DONE.** `release-plugins` run 29096354676 succeeded: `plugins-v0.1.1` live with 97
  assets (19 plugins x 5 targets + signed index + minisig). Post-release verification:
  `flux plugin install slack` -> 0.1.1 `[verified]`, manifest self-reports v0.1.1 (no
  descriptor/manifest mismatch), stored tokens resolve, and the default-markdown
  `slack.message.list` D-127 live-proof passes on the pack binary; repinned slack to 0.1.1
  (0.1.0 kept for rollback).

## Notes
- Pack release mechanics: `.github/workflows/release-plugins.yml` (workflow_dispatch; it creates
  the `plugins-v*` tag itself — hand-pushing one collides with the dist tag glob).
- The version-match sanity gate in the workflow compares the input to
  `plugins/[workspace.package].version`; the index is built from archive filenames, and the CLI
  compares descriptor vs manifest self-report at `plugin status` — inheritance makes all three
  agree by construction.
