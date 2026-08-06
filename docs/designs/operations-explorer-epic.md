# Design — Operations explorer (epic)

## Why

Flux has exactly one universal callable unit — the operation — and no way to browse it. The
built-in registry carries ~180 ops, installed plugins project hundreds more, and connector-compiled
ops will join them through the same `ToolSpec` contract. What exists today is partial and
enumeration-hostile: `flux catalog core` exports a hand-curated ~29-op subset
(`crates/flux-cli/src/catalog_cmd.rs`, `OPERATION_NAMES`), the REPL `/tools` prints comma-joined
names with no descriptions, and the only complete reference is a hand-maintained page
(`website/docs/language/ops.md`) kept honest by a contract test. Discoverability of the op surface
is the product; an explorer is how an operator (and eventually a newcomer) learns what the system
can do.

`flux ops` becomes the command group for the operation catalog. Its first tenant is `--explore`,
a full-screen TUI; a headless `flux ops list --json` (the missing complete enumeration) is a
natural later tenant of the same group and the same assembly seam.

## Approach

**Explorer, not another list.** `flux ops --explore` opens a standalone full-screen TUI
(raw mode + alternate screen, the `crates/flux-tui/examples/loop_mocks.rs` loop shape, blocking —
no agent, no tokio). Start state is deliberately search-first, like a search engine's home page:
a small centered animated pictogram (a node constellation — `◆` nodes joined by box-drawing
edges, edges pulsing in sequence, nodes sparkling as "ops firing"; no letters, distinct from the
FLUX wordmark splash), a centered input line beneath it, one muted hint row. First keystroke moves
to results: left pane a ranked list (selection glyph, risk glyph, name, first-sentence
description), right pane the selected op's detail (full description through `flux-markdown`,
schema-derived params, risk/idempotency/effects/group/source, doc links). Tab / ←→ cycle a derived
category filter; `Esc` walks back results → start → quit.

**flux-tui stays neutral; flux-cli assembles.** `flux-tui` deliberately depends on neither
`flux-tools`, `flux-web`, nor `flux-cli` (crate charter; see the C-518 note in its manifest). The
explorer therefore renders a caller-supplied `Vec<OpRow>` DTO and knows nothing about registries:

- `OpRow { name, description, params: Vec<ParamRow>, effects, risk, idempotency, group, category,
  source, doc_public_url, doc_local_url }` — effects/risk/idempotency stay typed via `flux-spec`
  (already a dependency).
- Entry point `flux_tui::run_ops_explorer(rows, OpsExplorerOptions { theme, seed })`, TTY-bailing
  like `run_with_options`.
- `crates/flux-cli/src/ops_cmd.rs` assembles the rows: the registry recipe of
  `build_core_catalog` (`ToolRegistry::new()` + `flux_tools::try_register_builtins` +
  `flux_web::try_register_web`), `registry.specs()` (name-sorted — that order is the empty-query
  order), `registry.source(name)` as the provenance label, params via
  `flux_lang::opspec::schema_params` plus property `type`/`description` extraction.

**Categories are derived, never invented twice.** Resolution order: (1) the canonical resolver
`flux_runtime::effective_group(spec, &flux_tools::builtin_groups())` — git/go/rust/node/python/
make/shell/endpoint/agent_invoke/consult/fleet/cognition, plus `ToolSpec::group` for e.g.
browser; (2) dotted-prefix families (`web.*`, `http.*`, `review.*`, `skill.*`, `pane.*`) — an
accepted convention (`tool_disable_matches` globs); (3) a small file-ops set (`read`, `write`,
`edit`, `glob`, `grep`, …) → files; (4) else core. One pure `categorize` fn in `ops_cmd.rs`, unit
tested. Evidence-gated ops are shown (registration, not advertisement, is the catalog's truth) —
the group label keeps that legible.

**Doc links come from a committed generated index.** No op→doc mapping exists anywhere, but the
website contract test already guarantees every registered op appears backticked in
`website/docs/language/ops.md`. A generated `crates/flux-cli/assets/ops_docs.json` maps op name →
website page ids using exactly that backticked-token scan over `website/docs/**/*.md`, with a
stop-list for generic single-word names (`read`, `list`, `map`, `get`, `task`, …) pinned to
`language/ops` only. The index is generated and drift-checked by a `FLUX_UPDATE_GOLDEN=1`-armed
test (`golden_mode` support copy, same C-326 arming rules as `website_in_sync.rs`); page ids
resolve to both the public URL (`https://codewandler.github.io/flux/docs/<page>`) and the local
`flux docs` server (`http://127.0.0.1:8788/flux/docs/<page>`, labelled as needing `flux docs`).
Links render as plain text with an OSC 52 copy action — the TUI's trust layer strips OSC 8
hyperlinks by design, and no URL-opening dependency exists in the workspace (a trusted opener is
a later story, not iteration 1).

**Interaction model.** Search input is always the default focus; `Enter` switches to a command
focus (j/k move, `y` copy, `?` help, `q` quit — as *typed characters* these all belong to the
query), `Ctrl-C` always quits, `Ctrl-Y` copies from any focus. Poll-based loop with no idle burn:
the animation deadline exists only on the start screen (fast tick for a few seconds after input,
then a slow shimmer), results idle at a coarse timeout, and under `Theme::MONO`/`NO_COLOR` the
pictogram is a static, uncolored frame. The pictogram lives in its own small grid module reusing
the splash kit (`Rgb`, `Cell`, `lerp`, `Pcg32`, the glow sine shader) with a seedable generator so
animation frames are deterministic under test.

**Naming honesty.** `crates/flux-tui/src/operations.rs` is the Board/Fleet overlay and keeps its
name; the explorer lands as `explorer.rs` + `pictogram.rs`.

**Scaling path.** Iteration 2 streams plugin-projected ops in asynchronously (manifest loading is
subprocess-based) behind a source trait, with source/availability facets. Iteration 3 renders the
op's documentation section in-pane and embeds the explorer as an `/ops` overlay inside `flux tui`.
Iteration 4 consumes the connector/Exchange effective catalogue through the declared
`platform`/`reaches` seam — connector ops arrive as ordinary `ToolSpec`-shaped rows with a
different source facet, which is the point of the DTO. The committed grep-derived doc index is a
stopgap by design: once the agent-native docs datasource exists
(docs/designs/agent-native-flux-docs.md, C-579/C-580/C-581), the explorer consumes it instead of
forking a second index.

## Stories

- C-643 — `flux ops --explore`: the core catalog explorer (iteration 1)
- C-644 — plugin operations stream in, with source and availability facets (iteration 2)
- C-645 — docs in the detail pane and an `/ops` overlay in the chat TUI (iteration 3)
- C-646 — connector-ready catalog over the effective catalogue seam (iteration 4)

Related epics: `flux-tour` (scripted onboarding may end a tour step inside the explorer) and
`docs-reader` (iteration 3 is a specialization of the general reader; both must converge on the
same docs corpus rather than two indexes).
