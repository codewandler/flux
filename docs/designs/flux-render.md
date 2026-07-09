# flux-render — a built-in `flow_render` tool: flux source/plan → SVG (+ later PNG)

**Status:** proposed 2026-07-09 · stories filed 2026-07-09 (L-74…L-78) · **Pillar:** Language · **Epic slug:** `flux-render`

A model-facing built-in tool `flow_render` that turns Flux-Lang into a **syntax-highlighted image**
— either the highlighted **source** or the **execution-path tree** (`--plan` view) — rendered
entirely from flux's *own* view of the code (its lossless CST and its plan renderer). No tree-sitter,
no external toolchain. Sits alongside `flow_list` / `flow_run` in `crates/flux-tools/src/flows.rs`'s
family. Shares its highlight substrate with the LSP semantic-tokens story ([flux-lsp.md](flux-lsp.md)
L-69) and builds on the CST front-end ([flux-lang-cst.md](flux-lang-cst.md)).

## Why

`.flux` has three highlighting stories today, and each is confined to a live program:
tree-sitter (`codewandler/flux-tree-sitter`) colours Helix/Neovim/Zed; a future `flux-lsp`
contributes semantic tokens; the website uses Prism; `flux-editors` ships TextMate/IntelliJ. **None
of them render on a surface that can't run a grammar** — a GitHub README, a Slack message, a design
doc, a chat/tool-result panel. The workaround we just shipped for the tree-sitter README is a Node
script (`flux-tree-sitter/scripts/render-example.mjs`) that shells out to the `tree-sitter` CLI and
scrapes its text output to emit an SVG — external, brittle, and not "flux".

Flux already owns everything needed to do this natively and better:

- a **lossless, tolerant rowan CST** — `flux_lang::parser::parse_cst(src) -> Parse`,
  `Parse::syntax() -> SyntaxNode` (`crates/flux-lang/src/parser.rs:40,34`; `SyntaxKind` / `SyntaxNode`
  / `SyntaxToken` in `crates/flux-lang/src/syntax.rs:216`). Walking the typed tree classifies each
  token by its **role** (the leading `IDENT` of a `WHEN_STMT` / `FLOW_DECL` / … is a keyword; `$x` is
  a `VAR`; `@effect` an `ANNOTATION`; strings/numbers/comments as-is) — more accurate than
  string-matching a keyword list, and `parse_cst` is **total**, so it highlights even incomplete /
  invalid source. (The lexer deliberately does *not* classify keywords — `syntax.rs:39`.)
- a **plan renderer** — `flux_lang::render::render_styled(ast: &DraftAst, &Palette)`
  (`crates/flux-lang/src/render.rs:56`) already produces the coloured `├─`/`└─` tree, and
  `flux-tui` already drives it with an ANSI palette (`crates/flux-tui/src/plan.rs`).

So a built-in `flow_render` is the portable-image counterpart to the editor stories, using flux's
real grammar. It also lets flux **regenerate its own doc images**, retiring the Node script.

## Decision

- **Home:** flux runtime built-in tool `flow_render`, registered model-facing like `flow_list` /
  `flow_run`. Not a plugin op (rendering flux isn't domain-specific).
- **Views:** `view: "source" | "tree"` — highlighted source *and* the plan tree (both, per the
  request).
- **Engine:** flux-native (CST for source; `render_styled` for tree). No tree-sitter dependency.
- **Format:** **SVG** in Phase 1 (text, friction-free). **PNG** is Phase 2, opt-in (needs a
  rasterizer + a bytes-write path — see Constraints).

## Output-channel constraints (they shape the tool's surface)

- `ToolResult` is **text-only**: `{ content: String, view: Option<String>, is_error }`
  (`crates/flux-runtime/src/lib.rs:49`; `ok_view` at :65 splits the canonical value from the
  model-facing rendering). There is no binary/artifact channel.
- `system.write_file(path, &str)` is **`&str`-only** (`crates/flux-system/src/lib.rs:470`); there is
  **no bytes writer**.

Consequences: SVG is just text — return it inline (canonical `content`) with a compact `view`
summary. Binary PNG can't inline and can't go through `write_file`, so **the model-facing tool stays
read-only and SVG-only**; file output and PNG live in the CLI subcommand / Phase 2.

## Architecture — layered for reuse

flux enforces downward-only crate deps via `flux-codegate` (`crates/flux-codegate/src/lib.rs:44`).
Layers: `flux-lang` = **L0**, `flux-tools` / `flux-runtime` = **L2**, `flux-cli` = **L6**. The split
below keeps the reusable *semantics* low and *presentation* in the tool:

### 1. `flux_lang::highlight` — NEW module (L0, pure, no deps)

```rust
pub enum HighlightClass { Keyword, Op, Var, Annotation, String, Number, Comment, Punct, Type, Error }
pub fn highlight(src: &str) -> Vec<(rowan::TextRange, HighlightClass)>;
```

Implemented as `parse_cst(src)` → walk `Parse::syntax()`; classify each `SyntaxToken` by its kind and
its parent node's kind. Total (never errors). This is exactly the substrate the LSP **semantic
tokens** story wants ([flux-lsp.md](flux-lsp.md) L-69 — "CST token stream classified by
`SyntaxKind`"); building it here means L-69 is a thin LSP adapter over it later. Unit-testable in
isolation against small snippets.

### 2. `flux_lang::render` — extend with a span form (L0)

Add `pub fn render_styled_spans(ast: &DraftAst) -> Vec<Vec<(String, Role)>>` (lines of
`(text, Role)`), and refactor the existing `render_styled` to be the ANSI stringifier over those
spans. `Role` mirrors the current `Palette` fields (`keyword`/`op`/`symbol`/`string`/`lit`/`effect`/
`connector`/`thing`, `render.rs:13`). Both `flux-tui` (ANSI) and `flow_render` (SVG) then build on
one walk — no "render to ANSI then parse it back" round-trip. (`sink.rs` is unrelated: it's the
interpreter observation sink.)

### 3. `crates/flux-tools/src/render.rs` — NEW module (L2): theme + SVG + the tool

```rust
pub enum View { Source, Tree }
pub fn render_flux_svg(src: &str, view: View) -> Result<String>;   // pure core, no ToolContext
struct FlowRenderTool;                                             // thin Tool wrapper
pub fn register_render(registry: &mut ToolRegistry);
```

`render_flux_svg` maps `HighlightClass` / `Role` → colours and emits the SVG. It is the shared core
for the tool, the CLI subcommand, and doc-image regeneration — and is directly unit-testable with no
`ToolContext`.

**View wiring inside `render_flux_svg`:**
- `Source` → `flux_lang::highlight::highlight(src)` → colour grid → SVG. Robust to parse errors.
- `Tree` → `flux_lang::parse::parse(src) -> Result<DraftAst>` (`parse.rs:41`) → `render_styled_spans`
  per flow. **Tree view is flow-first:** `render_styled` is flow-only, and composite ops
  (`CompositeOpDecl`, `program.rs:158`) have no tree renderer — render `Program.flows`
  (`program.rs:191`) as trees and, for `Program.ops`, either a best-effort list of statement heads
  via `render_statement` (`render.rs:86`) or fall back to `Source`. On a hard parse error, the tool
  returns `ToolResult::error(msg)`.

### Theme (port verbatim from `flux-tree-sitter/scripts/render-example.mjs`, so the source view matches the README image)

Canvas: `BG #282c34`, `FG #abb2bf` (fallback), window dots `#e06c75 #e5c07b #98c379`, title `#5c6370`.
Layout: `font-size 15`, char-advance `9.4` (width sizing only), line-height `22`, pad `22`, header
`44`, bottom `18`; monospace `font-family` stack.

| class / role | colour | | class / role | colour |
|---|---|---|---|---|
| Keyword / `keyword` | `#c678dd` | | Number / `lit` | `#d19a66` |
| Op / `op` | `#61afef` | | Comment | `#7f848e` |
| Var / `symbol` | `#e06c75` | | Type / `thing` | `#e5c07b` |
| Annotation / `effect` | `#56b6c2` | | Punct | `#828997` (brackets `#abb2bf`) |
| String / `string` | `#98c379` | | `connector` (├─) | `#5c6370` |

### Tool spec & I/O

Input (`schemars`-derived, mirroring `FlowRunInput` in `flows.rs`): `source: Option<String>` **xor**
`name: Option<String>` (a stored flow, resolved by reusing `flow_files` / `FLOW_DIRS` from
`flows.rs`); `view: "source" | "tree"` (default `"source"`). Spec: `effects: [Read, Filesystem]`
(Filesystem only because `name` reads the flow dirs), `risk: Low`, `idempotency: Idempotent`.
Register in `crates/flux-cli/src/main.rs` (~line 2133) with `flux_tools::register_render(&mut
registry);`, right after `register_flows`, and re-export `register_render` from
`crates/flux-tools/src/lib.rs`.

Output: `ToolResult::ok_view(svg, "rendered <name|inline> (<view> view) → SVG WxH, N lines")` — the
canonical `content` is the SVG markup (a UI can render it), the compact `view` keeps the transcript
from being flooded with markup.

### Correctness notes the JS port skipped

- **Multi-line tokens:** a `"""triple"""` `STRING` (and `@json` blocks) span lines — split coloured
  spans at line boundaries.
- **`char` indexing, not byte/UTF-16:** tree connectors `├─└│`, `$`/`@` sigils, and `{interpolation}`
  are multi-byte; build the per-line colour grid over `char`s.

## Files to change

**Phase 1 — SVG, source + tree (no new deps):**
- **NEW** `crates/flux-lang/src/highlight.rs` (+ `pub mod highlight;` in `crates/flux-lang/src/lib.rs`).
- `crates/flux-lang/src/render.rs` — add `render_styled_spans`; refactor `render_styled` onto it.
- **NEW** `crates/flux-tools/src/render.rs` — `render_flux_svg`, `FlowRenderTool`, `register_render`,
  theme + SVG emitter.
- `crates/flux-tools/src/lib.rs` — `pub mod render;` + re-export `register_render`.
- `crates/flux-cli/src/main.rs` (~2133) — `flux_tools::register_render(&mut registry);`.

**Phase 1 bonus — CLI subcommand (non-model entry point + doc-image generator):**
- `crates/flux-cli/src/main.rs` — `flux render <file.flux> [--view source|tree] [-o out.svg]` calling
  `render_flux_svg` and writing via `system.write_file` (SVG is text). Non-gated way to use/verify it;
  **replaces** `flux-tree-sitter/scripts/render-example.mjs` for regenerating doc images.

**Phase 2 — PNG (opt-in, adds deps):**
- `crates/flux-system/src/lib.rs` — add `write_file_bytes(path, &[u8])` (parallels `read_file_bytes`
  at :481).
- `crates/flux-tools/Cargo.toml` — `resvg` + `usvg` + `tiny-skia` + `fontdb`, embed a monospace font
  (e.g. JetBrains Mono) so text lays out headlessly; rasterize SVG → PNG bytes → the CLI's `-o
  out.png`. ⚠️ Confirm `flux-codegate` accepts the new external deps on `flux-tools`.

## Stories (filed 2026-07-09)

1. **[L-74](../stories/L-74-flux-lang-highlight-substrate.md) — highlight substrate** —
   `flux_lang::highlight` (CST → `HighlightClass` spans) + tests. Standalone; also unblocks
   flux-lsp L-69.
2. **[L-75](../stories/L-75-render-styled-spans.md) — span renderer** — `render_styled_spans`
   refactor of `render.rs` (keeps `flux-tui` green).
3. **[L-76](../stories/L-76-flow-render-tool-svg.md) — `flow_render` tool (SVG)** — `render.rs` in
   flux-tools (theme + SVG emitter + tool), wire the registry, `source` + `tree` views,
   `name`/`source` input.
4. **[L-77](../stories/L-77-flux-render-cli-subcommand.md) — `flux render` CLI** — subcommand +
   retire the tree-sitter Node script (update `flux-tree-sitter` README/AGENTS to point at
   `flux render`).
5. **[L-78](../stories/L-78-flux-render-png.md) — PNG (opt-in, backlog)** — `write_file_bytes` +
   resvg rasterization + embedded font.

## Verification

- **Pure core (no ctx):** `render_flux_svg("flow greet(name: String)\n  do notify \"hi\"  # c",
  View::Source)` → SVG contains `<tspan fill="#c678dd">flow</tspan>` (keyword), a green `"hi"` span, a
  grey comment span; `View::Tree` → coloured `├─`/`└─`; width/height scale with longest line / line
  count; output starts with `<svg`; a `"""` multi-line string colours on every line; malformed source
  in `Source` still renders (no panic), in `Tree` returns an error result.
- **Tool:** build a test `ToolContext` with the established pattern (`transform.rs:605`'s `ctx()`;
  `ToolContext::new(Arc::new(System::new(Workspace::new(&dir)?)))`, `toolchains.rs:572`); assert
  `flow_render` resolves `name` from a `.flux/flows` file and returns SVG.
- **The gate** (must stay green): `cargo build --workspace` · `cargo test --workspace` · `cargo clippy
  --workspace --all-targets -- -D warnings` · `cargo fmt --all` · `cargo test -p flux-codegate`.
- **End-to-end:** `flux render examples/*.flux --view tree -o /tmp/x.svg` then open it; `--view
  source` to eyeball highlighting. Model-facing path: an agent in a session invokes `flow_render`.
- *(Phase 2)* rasterize to PNG, open it, confirm the embedded font renders text (not tofu).

## Scope & non-goals

- **Flow-graph diagram** (boxes + arrows / Graphviz) is out of scope — no such pipeline exists;
  `render_styled`'s tree is the "flux perspective" used here.
- **Editor highlighting is unchanged** — tree-sitter (Helix/Neovim/Zed), `flux-lsp` semantic tokens,
  Prism (website), TextMate/IntelliJ remain the in-editor stories. `flow_render` serves the surfaces
  that can't run a grammar (GitHub, Slack, docs, chat), and shares the `flux_lang::highlight`
  substrate with L-69.
- **PNG** is deferred to Phase 2 (rasterizer deps + embedded font + a `flux-system` bytes writer);
  Phase 1 SVG is self-contained string generation.
