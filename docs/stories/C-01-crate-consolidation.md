---
id: C-01
title: Crate consolidation, phases 2–4
pillar: Core
status: done
design: docs/designs/crate-consolidation.md
note: hooks→plugin, browser+datasource→capabilities, context→runtime; removed dead integrations (35 → 31 crates)
---

# Crate consolidation, phases 2–4

## Goal
Continue shrinking the workspace by merging coherent **same-layer** siblings (no crossing an
architectural boundary), so the build graph and mental model get smaller. Phase 1 already collapsed
the provider crates (37 → 33); phases 2–4 are projected to reach ~28–29.

## Acceptance
- [x] **Phase 2 (L4):** `flux-hooks` → `flux-plugin` (`hooks` module). 2 → 1. (353c4b9)
- [x] **Phase 3 (L5):** `flux-browser` + `flux-datasource` → `flux-capabilities`; `flux-auth` kept
      standalone (distinct concern). 2 → 1. (4384a56)
- [x] **Phase 4 (L2/L6):** `flux-context` → `flux-runtime` (`context` module); orphan
      `flux-integrations` removed (confirmed dead). (7e894ef, 9753fc3)
- [x] After each phase: `cargo test -p flux-codegate` (layering) green + the full gate green.

## Progress
- Phase 1 ✅ shipped (providers → `flux-providers`).
- Phases 2–4 ✅ shipped in-place on `main`, one commit per phase. Workspace **35 → 31 crates** (the
  count had drifted up from the docs' "33" as new leaves landed since Phase 1).

## Notes
- Full plan, rationale, and the "do not merge" list (16-crate publish closure, L0 leaves, large L3/L6
  subsystems) live in `docs/designs/crate-consolidation.md`. One phase per commit.
