---
id: L-95
title: "Railflux — render DraftAst as a 7-bit ASCII dataflow diagram"
pillar: Language
status: in-progress
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

- [x] A failing-first golden test pins the triage flow's parallel fan-out, match arms, confirmation,
      calls, bindings, and returns as the design's Railflux shape.
- [x] `flux_lang::render` exposes a pure Railflux string projection (and role-tagged spans if needed
      by existing render consumers); identical ASTs produce byte-identical output.
- [x] Canonical output is strictly 7-bit ASCII, including connectors, quotes, labels, and fallbacks.
- [x] Every `Node` variant has an explicit rendering test or catalog-driven coverage assertion;
      unsupported horizontal shapes use nested labelled regions and never omit semantic fields.
- [x] `fluxlang rail [FILE]` reads canonical Flux source from a file or stdin and prints Railflux;
      malformed Flux reports the existing parser diagnostics.
- [x] No Railflux reader, alternate extension sniffing, AST change, or runtime behavior is added.
- [x] Flux-Lang CLI-feature tests, clippy, fmt, and `flux-codegate` are green.

## Progress

- **Landed.** The renderer is `crates/flux-lang/src/rail.rs` (private module), re-exported as
  `flux_lang::render::{render_rail, render_rail_styled, render_rail_spans}` — the same trio and the
  same `Role`/`Palette` substrate as `render_pretty`/`render_styled`/`render_styled_spans`, so there
  is one styling model for every projection.
- **The output shape is specified**, because L-100's blocker is exactly that: see
  [`crates/flux-lang/docs/railflux.md`](../../crates/flux-lang/docs/railflux.md). Two line shapes,
  disambiguated by the first non-space character: a line starting with `[` is a **region** (label +
  two-space-indented body, optionally `--> sink`) and never contains `-->`; every other line is a
  **rail** `sources --> stage --> sink` and always does. Stage tokens are self-delimiting (`op(…)`
  or `[…]`), so the columns never collide.
- **Deliberate deviations from the design sketch** (all argued in the spec doc): regions instead of
  column-aligned `+--`/`` `-- `` arm glyphs (alignment does not nest and makes a line's width a
  function of its siblings); `key: value` rather than `key=value` inside stages, so L-100 reuses the
  existing Flux expression grammar; and stages always keep their parens (`classify(.)`, not
  `classify`) so a bare identifier is unambiguously a sink.
- **Totality** is enforced two ways: `is_rail_shaped` and `stmt` are exhaustive over `Node` (no `_`
  arm — a new variant fails compilation), and `railflux_golden.rs` cross-checks its per-variant
  expectation table against the generated `schema::node_kind_rows()` catalog.
- Nothing was added to the AST, the runtime, the op catalog, or the loaders. No reader exists.

## Notes

- Reuse the existing `render_styled_spans`/`Role` substrate where that prevents a second styling
  model; Railflux itself is a different dataflow walk, not a relabelled execution tree.
- L-100 is the separate, deferred reader.
