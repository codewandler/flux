---
id: C-42
title: Relocate DRIFT.md under docs/ — archive the finished-migration ledger
pillar: Core
status: done
design:
epic:
note: 33 KB ledger of D-31/D-36..D-45 schema-migration drift reports sat at repo ROOT; now docs/archive/drift-reports.md — root keeps the canonical top-level files only
---

# Relocate DRIFT.md under docs/ — archive the finished-migration ledger

## Goal
`DRIFT.md` (33 KB) records the schema↔handler drifts found and fixed during the D-31/D-34/D-36..
D-45 schemars migrations — valuable history, wrong address. It reads as archival (every section
describes *finished* work), so it belongs under `docs/archive/`, leaving the repo root to the
canonical files (README, AGENTS, CHANGELOG, CONTRIBUTING, SECURITY, licenses).

## Acceptance
- [ ] `git mv DRIFT.md docs/archive/drift-reports.md` (history preserved), with a one-line
      pointer added where discoverability matters: the docs map (`docs/README.md`) and the
      D-31/D-36 story files that cite it.
- [ ] `rg -n 'DRIFT\.md'` across the repo (code, docs, stories, plans, CI) is clean of dangling
      references — every mention updated to the new path.
- [ ] The file itself gains a two-line header stating what it is and that new drift findings from
      future migrations should append here (it is a living ledger for that one migration family,
      not a graveyard).

## Progress
- 2026-07-07 DONE in one pass: `git mv DRIFT.md docs/archive/drift-reports.md`; ledger header
  added (what it is + append-here instruction, per acceptance); path references updated in
  D-34/D-36/D-42 stories, `plugins/homer/src/main.rs` comments, CHANGELOG pointer mentions, and
  the D-66 story; docs map row added in `docs/README.md`. `rg 'DRIFT\.md'` is clean of dangling
  references (remaining mentions are this story's own history + the board note, regenerated).
  Docs-only change — no code gate run beyond the reference sweep; plugins comment change is
  comment-only.

## Notes
- The ledger's "Not yet migrated" / "deferred" sections are the seed material for D-66 (the
  schema-SSoT increment) — keep the cross-link when moving.
