---
id: C-40
title: Docs truth pass — roadmap/board/architecture/README staleness after v0.3.0
pillar: Core
status: done
design:
epic:
note: roadmap header says "0.2.15, 33 crates" (reality 0.3.0, 34); board Status says v0.2.4; architecture.md:99 contradicts itself on flux-datasource and omits flux-audio; README under-lists providers/L6
---

# Docs truth pass — roadmap/board/architecture/README staleness after v0.3.0

## Goal
Align the top-level docs with landed reality (the C-16 discipline, re-run after ~15 releases of
drift). Every fix below was verified against the tree on 2026-07-07.

## Acceptance
- [ ] `docs/roadmap.md` header: version/date current (0.3.0, 2026-07-07), crate count 34, test
      count re-derived (not guessed).
- [ ] `docs/stories/README.md` hand-written `## Status` block: current release + a truthful
      in-flight summary (it still says "Released: v0.2.4 (2026-06-25)").
- [ ] `docs/architecture.md:99`: drop the false "merged … flux-datasource" claim (it is a live
      standalone L0 crate that flux-capabilities *depends on*); keep the accurate flux-browser/
      context/hooks merge history.
- [ ] `docs/architecture.md`: `flux-audio` present in the layer table and the per-crate map (L0,
      D-61).
- [ ] README layer table: providers row lists Bedrock + Codex (line ~304); L6 surfaces row lists
      flux-app + flux-channels (line ~309) — both currently under-list what the same README's own
      provider table shows.
- [ ] Stale doc-comment `examples/strict-review-app.flux` in `crates/flux-app/src/review.rs:121`
      corrected (file does not exist; point at the real example or drop the reference).

## Progress
- 2026-07-07 DONE, all acceptance boxes + two extras found en route:
  - roadmap header → 0.3.0 (2026-07-07), 34 crates, **1900+ tests** re-derived by counting
    `#[test]`/`#[tokio::test]` fns (1528 root + 450 plugins = 1978).
  - board `## Status` → v0.3.0 + a truthful in-flight summary (the hardening/docs/cleanup push).
  - architecture.md:99 → "merged `flux-browser` (P3 ✅); depends on the standalone L0
    `flux-datasource` contract crate"; flux-audio added to the L0 layer row AND the shared-core
    crate table.
  - README providers row → adds Ollama/Bedrock/subscription providers; L6 row → adds app host +
    channels.
  - review.rs stale `examples/strict-review-app.flux` pointer dropped (comment-only change).
  - EXTRA 1: AGENTS.md L0 table was ALSO missing flux-audio (the story note originally claimed it
    was current — wrong) — added.
  - EXTRA 2: roadmap "Plugin distribution" epic section still said "D-48/D-49 are the live next
    stories" (epic completed 2026-07-05) — heading + bullets flipped to done.
- Gate: docs-only except the review.rs doc comment; `cargo check -p flux-app` deferred to the
  final full-workspace gate of this push (comment-only edit, no code change).

## Notes
- Docs-only story: narrow gate, verified via the final full-workspace gate of the push.
