---
id: L-76
title: flow_render built-in tool — flux source/plan → SVG (source + tree views)
pillar: Language
status: done
epic: flux-render
design: docs/designs/flux-render.md
note: "model-facing flow_render beside flow_list/flow_run: render_flux_svg pure core in flux-tools (One-Dark theme ported verbatim from the tree-sitter Node script), source|tree views, name-or-source input, SVG returned inline via ok_view — text-only ToolResult keeps it read-only/SVG-only"
---

# flow_render built-in tool — flux source/plan → SVG (source + tree views)

## Goal
A model-facing built-in tool `flow_render` that turns Flux-Lang into a syntax-highlighted SVG —
either the highlighted **source** (via `flux_lang::highlight`, L-74) or the **execution-path tree**
(via `render_styled_spans`, L-75) — rendered entirely from flux's own grammar. Registered beside
`flow_list`/`flow_run`; serves the surfaces that can't run a grammar (GitHub README, Slack, docs,
chat panels). No tree-sitter, no external toolchain, no new deps.

## Acceptance
- [x] NEW `crates/flux-tools/src/render.rs`: `pub enum View { Source, Tree }`,
  `pub fn render_flux_svg(src: &str, view: View) -> Result<String>` (pure core, no `ToolContext`),
  `FlowRenderTool`, `pub fn register_render(registry: &mut ToolRegistry)`; re-exported from
  `flux-tools/src/lib.rs`; registered in `crates/flux-cli/src/main.rs` right after `register_flows`.
- [x] Theme ported **verbatim** from `flux-tree-sitter/scripts/render-example.mjs` (BG `#282c34`,
  keyword `#c678dd`, string `#98c379`, … — full table in the design) so the source view matches the
  existing README image.
- [x] Failing-first pure-core tests: `render_flux_svg(snippet, Source)` output starts with `<svg`,
  contains `<tspan fill="#c678dd">flow</tspan>`, a green string span, a grey comment span;
  `Tree` view yields coloured `├─`/`└─` connectors; width/height scale with longest line/line count.
- [x] Correctness notes covered by tests: a `"""` multi-line string colours on **every** line
  (spans split at line boundaries); the per-line colour grid indexes by `char`, not bytes
  (connectors/sigils/interpolation are multi-byte).
- [x] Robustness: malformed source in `Source` view still renders (no panic); in `Tree` view the
  tool returns `ToolResult::error(msg)`. Tree view is flow-first: `Program.flows` render as trees;
  composite ops get a best-effort statement-head list or fall back to `Source`.
- [x] Tool input mirrors `FlowRunInput`: `source` **xor** `name` (stored flow resolved by reusing
  `flow_files`/`FLOW_DIRS` from `flows.rs`), `view: "source" | "tree"` (default `"source"`).
  Spec: `effects: [Read, Filesystem]`, `risk: Low`, `idempotency: Idempotent`.
- [x] Tool test with the established `ToolContext` pattern (`transform.rs:605` `ctx()`): `flow_render`
  resolves `name` from a `.flux/flows` file and returns SVG via
  `ToolResult::ok_view(svg, "rendered <name|inline> (<view> view) → SVG WxH, N lines")`.
- [x] Gate green: `cargo build/test/clippy -D warnings/fmt` + `cargo test -p flux-codegate`.

## Progress
- 2026-07-09 — implemented: NEW `crates/flux-tools/src/render.rs` (theme constants ported verbatim
  from `render-example.mjs`, char-indexed colour grid over `flux_lang::highlight` spans for the
  source view, `render_styled_spans` role→fill mapping for the tree view, composite ops as
  `op <name>` + connector-prefixed `render_statement` heads, agents-only programs fall back to
  source); `flows::flow_files`/`basename` made `pub(crate)` for name resolution; registered in
  `main.rs` right after `register_flows`. 12 tests (8 pure-core + 4 tool-level with the
  `transform.rs` `ctx()` pattern). Gate: build/test (110 binaries)/clippy `-D warnings`/codegate
  green; `fmt --check` clean for every L-76 file (a concurrent session's in-flight
  `flux-capabilities/src/datasource/ops.rs` had transient drift — not this story's).
- Verified on a real example: both views of `examples/data-transforms.flux` render to well-formed
  SVG with the expected One-Dark colour distribution.

## Notes
- Design: [flux-render.md](../designs/flux-render.md) §§ "3. `crates/flux-tools/src/render.rs`",
  "Theme", "Tool spec & I/O", "Output-channel constraints".
- `ToolResult` is text-only (`crates/flux-runtime/src/lib.rs:49`, `ok_view` at :65) — SVG inlines as
  text; the model-facing tool stays read-only and SVG-only (file output/PNG live in [[L-77]]/[[L-78]]).
- Depends on [[L-74]] (source view) + [[L-75]] (tree view).
