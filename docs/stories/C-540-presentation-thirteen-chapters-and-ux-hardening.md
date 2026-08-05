---
id: C-540
title: Grow the engineering presentation to thirteen chapters and harden its UX
pillar: Core
status: done
priority:
design: docs/designs/interactive-flux-presentation.md
epic:
areas: [docs, website]
note: "Extend the C-491 deck with agent-loop, sessions, and model-strategy chapters; fix its stale Exchange claims; make it discoverable, printable, touch- and keyboard-robust."
---

# Grow the engineering presentation to thirteen chapters and harden its UX

## Goal

Extend the engineering presentation from ten to thirteen chapters — the adaptive agent loop,
sessions as the operational record, and model strategy — and harden it into a dependable live
artifact: truthful about shipped Exchange behavior, reachable from the site's front doors,
printable as a handout, and navigable by keyboard and touch without dead ends.

## Acceptance

- [x] The deck has thirteen chapters; the three new ones cover the adaptive loop
      (docs/agent-loop.md), session records (docs/usage.md, docs/agent-loop.md), and model
      strategy (docs/model.md), each pinned by a distinctive claim in
      `website_contract::engineering_presentation_is_discoverable_grounded_and_reuses_the_guarded_fixture`.
- [x] The deck's Exchange claims match the shipped tree: the embedded Service Account seam is
      stated as shipped, the retired "invocation wiring is not built" gap is pinned out, and the
      sibling snapshots are re-verified and re-dated (flux-connectors main v0.20.0, flux-exchange
      main v0.17.0, 2026-08-05) with the date pin updated in lockstep.
- [x] All chapters render in the DOM: printing yields a complete handout with a static code
      listing for the demo chapter, hidden chapters are inert, and the Monaco workbench mounts
      only once the demo chapter is first shown. (Verified in the SSG output: all thirteen titles
      present, twelve inert siblings, no Monaco in the static page.)
- [x] Deck keys (arrows, PageUp/PageDown, Home/End) keep working after activating any deck
      control; Space still activates a focused control; horizontal swipe changes chapters; the
      browser Back button steps to the previous chapter; a contents menu jumps to any chapter.
      (Implemented and building; no in-repo browser-automation harness exists — see Progress for
      the live-smoke caveat.)
- [x] The presentation is linked from the site footer, the landing page, the docs overview, and
      the repository README — pinned by website_contract so it cannot become an orphan again.
- [x] The embedded docs bundle is regenerated deterministically and the repository gate passes on
      this story's footprint. (Amended 2026-08-05: one provably foreign in-flight C-518 change in
      flux-capabilities keeps the shared tree's workspace test/clippy red at closure time; see
      Progress.)

## Progress

- 2026-08-05: Story opened from the approved improvement plan; implementation begins in the same
  session. Snapshot versions verified against both siblings' `origin/main` after a fresh fetch.
- 2026-08-05: Implemented in full — SLIDES/dispatcher merged into one render array with derived
  numbering, three chapters added, truth refresh landed, render-all + print + inert + visited
  workbench mount, keyboard/touch/Back/menu, theming and a11y fixes, discoverability links, and
  the contract pins (added failing-first, now green). Verified: website build with its
  broken-link gate, static SSG output (13 titles, 12 inert, no Monaco), embedded bundle + its
  determinism check, website contract 34/34, codegate 51/51. Interactive behaviors compile and
  are hydration-safe by construction, but the repo has no browser-automation harness — the first
  live keyboard/swipe/print smoke is the next speaker dry-run.
- 2026-08-05: Closed with a shared-tree caveat: workspace test/clippy are red only inside
  flux-capabilities' in-flight C-518 change (E0308 at usage_observatory.rs:945 in its test
  target, two lib clippy errors) — untouched by this story and modified before this session.
  `cargo test --workspace --exclude codewandler-flux-capabilities`: 208 suites green; root fmt
  diffs are confined to the same in-flight change.

## Notes

- Design record: docs/designs/interactive-flux-presentation.md (C-491's design; amended by this
  story for the thirteen-chapter format and the refreshed truth boundary).
- The deck is not in `core_surfaces` of the runtime-story contract test, so the agent-loop
  chapter may name "model-generated plans" as the retired alternative — that is the honest
  history the docs also state.
- New-chapter claim pins double as the failing-first evidence: they are added before the deck
  edits and fail against the ten-chapter deck.
