---
id: C-147
title: Make the cut transactional and restamp release docs mechanically
pillar: Core
status: done
priority: 16
epic: plugin-protocol-decoupling
design: docs/designs/plugin-protocol-decoupling.md
note: cut-release.sh rolls both changelogs and bumps versions BEFORE the gate — the 0.28.0 gate failure left them rolled, and a re-run would have minted a phantom version section (the documented 0.14.3 gap); it happened again and had to be finished by hand
---

# Make the cut transactional and restamp release docs mechanically

## Goal

A failed gate leaves the working tree exactly as it was, and the version stamps scattered through
the docs stop being hand-maintained.

## Why (evidence)

Cutting 0.28.0: the script bumped every version, re-locked both workspaces, rolled `CHANGELOG.md`
and `WHATS-NEW.md`, regenerated the website mirror, then failed the gate on
`shipped_flux_corpus_agreement`. Per the documented hazard the script could not be re-run (it
would have created a second `[0.28.0]` section), so the remaining steps were run by hand.
Separately, `docs/roadmap.md`'s "Status as of **X**" line and the hand-written Status block in
`docs/stories/README.md` were both edited manually.

## Acceptance

- [x] `scripts/cut-release.sh` is transactional: either it runs the gate before mutating anything,
      or it snapshots every file it touches and restores them on failure. Verified by forcing a
      gate failure and confirming `git status` is clean afterwards.
- [x] The script restamps `docs/roadmap.md`'s status line to the version being cut.
- [x] A test fails when the roadmap status line drifts from the workspace version (same shape as
      the existing `website_in_sync` guard).
- [x] The `⚠️ do not re-run` hazard note in the script header is either removed as fixed, or
      narrowed to whatever genuinely remains.

## Progress
- Done. See the CHANGELOG `[Unreleased]` entries and `docs/designs/plugin-protocol-decoupling.md` ("As built").

## Notes
- Related: C-39 (live smoke gate) — steps 7/8 report "no claude/codex credential" while the
  script's own pre-flight lists both as configured, so the subscription legs never run and the
  live gate had to be driven by hand for 0.28.0.
- Related: C-47 — cause fixed in `a707a35`; v0.27.0 still needs its `dist-manifest.json` asset
  backfilled before that story leaves Blocked.
