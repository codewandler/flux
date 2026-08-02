---
id: L-127
title: Serve the public docs and a structural Flux-Lang playground from the distributed binary
pillar: Language
status: done
design: docs/designs/embedded-docs-playground.md
note: "one self-contained `flux docs` surface: release-matched Docusaurus docs, a version endpoint, and an effect-free /console over L-126's editor projection"
---

# Serve the public docs and a structural Flux-Lang playground from the distributed binary

## Goal

Let a downloaded `flux` binary serve the same public documentation it shipped with and a browser
playground that visualizes authored Flux through the real editor projection. Make the homepage code
sample visibly step-able without presenting a structural preview as live effect execution.

## Acceptance

- [x] Failing-first router tests prove `/version`, `/flux/`, `/console/`, and projection of valid and
      invalid source; unknown API routes never fall through to the documentation SPA.
- [x] `flux docs [--bind <addr>]` serves an embedded, release-matched website with no checkout,
      Node installation, or runtime asset directory.
- [x] `/console` edits Flux source, renders L-126's graph, and provides play, pause, rewind, previous,
      and next controls over an explicitly effect-free structural trace preview.
- [x] The homepage code block carries the same playback vocabulary and a compact web visualization;
      a binary-served page shows the binary's current version.
- [x] A deterministic regeneration/check command prevents the embedded site bundle drifting from
      `website/`; public docs and both changelogs explain the surface and its safety boundary.
- [x] Website build/tests and the full Rust gate pass.

## Progress

- 2026-08-02: story and design opened from the owner's request; implementation started.
- 2026-08-02: captured the router contract failing first, then shipped the embedded site, version
  endpoint, structural projection API, console, and homepage playback visualization.
- 2026-08-02: verified the real `flux docs` binary end to end and passed the website build and sync
  check, workspace build/tests/clippy, and `flux-codegate`.

## Notes

- Builds on [L-126](L-126-project-authored-flows-into-an-editor-graph.md), which deliberately keeps
  the projection host-neutral and value-free.
- The live debugger semantics remain [A-140](A-140-pause-a-live-run.md),
  [A-142](A-142-inspect-a-paused-run.md), and [A-143](A-143-change-a-value-and-continue.md). This
  story must not imply that stepping a projection pauses or rewinds effects.
