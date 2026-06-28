---
id: C-01
title: Crate consolidation, phases 2–4
pillar: Core
status: backlog
priority:
design: docs/designs/crate-consolidation.md
---

# Crate consolidation, phases 2–4

## Goal
Continue shrinking the workspace by merging coherent **same-layer** siblings (no crossing an
architectural boundary), so the build graph and mental model get smaller. Phase 1 already collapsed
the provider crates (37 → 33); phases 2–4 are projected to reach ~28–29.

## Acceptance
- [ ] **Phase 2 (L4):** `flux-hooks` → `flux-plugin` (`hooks`/`plugin` modules). 2 → 1.
- [ ] **Phase 3 (L5):** `flux-browser` + `flux-datasource` → `flux-capabilities`; decide whether to
      fold in `flux-auth`.
- [ ] **Phase 4 (L2/L6):** `flux-context` → `flux-runtime`; resolve the orphan `flux-integrations`
      (fold into a surface or remove).
- [ ] After each phase: `cargo test -p flux-codegate` (layering) green + the full gate green.

## Progress
- Phase 1 ✅ shipped (providers → `flux-providers`).

## Notes
- Full plan, rationale, and the "do not merge" list (16-crate publish closure, L0 leaves, large L3/L6
  subsystems) live in `docs/designs/crate-consolidation.md`. Do one phase per commit.
