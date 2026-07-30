---
id: L-95
title: "Railflux — render DraftAst as a 7-bit ASCII dataflow diagram"
pillar: Language
status: ready
priority: 6
epic: flux-notation-workbench
design: docs/designs/flux-notation-workbench.md
areas: [flux-lang, flux-cli]
note: "FIRST — output only: a deterministic total ASCII projection exposed through fluxlang; parsing waits for L-100"
---

# Railflux — render `DraftAst` as a 7-bit ASCII dataflow diagram

## Goal

Make Flux plans readable as terminal-native dataflow rails. The first cut is a pure, total renderer
from `DraftAst`; it does not accept Railflux as input.

## Acceptance

- [ ] A failing-first golden test pins the triage flow's parallel fan-out, match arms, confirmation,
      calls, bindings, and returns as the design's Railflux shape.
- [ ] `flux_lang::render` exposes a pure Railflux string projection (and role-tagged spans if needed
      by existing render consumers); identical ASTs produce byte-identical output.
- [ ] Canonical output is strictly 7-bit ASCII, including connectors, quotes, labels, and fallbacks.
- [ ] Every `Node` variant has an explicit rendering test or catalog-driven coverage assertion;
      unsupported horizontal shapes use nested labelled regions and never omit semantic fields.
- [ ] `fluxlang rail [FILE]` reads canonical Flux source from a file or stdin and prints Railflux;
      malformed Flux reports the existing parser diagnostics.
- [ ] No Railflux reader, alternate extension sniffing, AST change, or runtime behavior is added.
- [ ] Flux-Lang CLI-feature tests, clippy, fmt, and `flux-codegate` are green.

## Progress

- (not started)

## Notes

- Reuse the existing `render_styled_spans`/`Role` substrate where that prevents a second styling
  model; Railflux itself is a different dataflow walk, not a relabelled execution tree.
- L-100 is the separate, deferred reader.
