---
id: C-143
title: Put the protocol crates on their own version line and stop the flux cut touching plugins/
pillar: Core
status: done
priority: 12
epic: plugin-protocol-decoupling
design: docs/designs/plugin-protocol-decoupling.md
note: cut-release.sh rewrites plugins/ pins + host-kit version + relocks the nested workspace on EVERY flux cut — plugins/Cargo.lock changed in 5 of the last 8 commits that touched it, all release cuts
---

# Put the protocol crates on their own version line and stop the flux cut touching `plugins/`

## Goal

A flux release stops modifying the plugin pack. The protocol crates carry a version that moves
only when the wire format moves, and `scripts/cut-release.sh` produces a diff with no file under
`plugins/`.

## Acceptance

- [x] `codewandler-flux-plugin-protocol` starts at `1.0.0` with its own `version` key (not
      `version.workspace = true`), and the serde-only leaves the wire surface needs — `flux-spec`,
      `flux-evidence`, `flux-datasource`, `flux-secret` — join that line.
- [x] `plugins/host-kit` versions on the protocol line (`1.x`) and depends on `^1`; the lockstep
      comment block in `plugins/host-kit/Cargo.toml` is deleted.
- [x] `plugins/Cargo.toml`'s `[workspace.dependencies]` use caret requirements on the protocol
      line, keeping path deps for local development.
- [x] `scripts/cut-release.sh` no longer: `sed`s `plugins/Cargo.toml` or
      `plugins/host-kit/Cargo.toml`, rewrites publish-closure pins for the protocol crates, runs
      `cargo update --manifest-path plugins/Cargo.toml`, or stages any `plugins/` file. The
      plugins-workspace `cargo fmt --check` stays in the gate.
- [x] Verified: `scripts/cut-release.sh <next> --no-gate` on a scratch branch produces a diff
      touching no file under `plugins/`.
- [x] AGENTS.md records the documented exception to the single-version rule and why C-146's
      changed-crate assertion replaces the guarantee it provided.

## Progress
- Done. See the CHANGELOG `[Unreleased]` entries and `docs/designs/plugin-protocol-decoupling.md` ("As built").
- Verified for real by the 0.29.0 cut (`f66f7e3`): the release commit touches `Cargo.toml`, `Cargo.lock`, both changelogs, the website mirror and the roadmap stamp — **zero paths under `plugins/`** — while the seven protocol-line crates stayed at 1.0.0 and the workspace moved 0.28.0 -> 0.29.0.

## Notes
- Depends on C-142. Land with C-144's guards — an independent version line without a drift guard
  is how stale code ships.
