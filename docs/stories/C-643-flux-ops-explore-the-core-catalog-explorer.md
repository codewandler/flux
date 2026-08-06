---
id: C-643
title: "flux ops --explore: the core catalog explorer"
pillar: "Core"
status: backlog
epic: ops-explorer
areas: [flux-tui, flux-cli]
design: docs/designs/operations-explorer-epic.md
note: "iteration 1: Google-like start screen, node-constellation pictogram, fuzzy search, category filter, docs links"
---

# flux ops --explore: the core catalog explorer

## Goal

`flux ops --explore` opens a standalone full-screen catalog explorer over the built-in + web
registry (~180 ops, assembled in-process — no plugin subprocesses in this iteration): a
search-first start screen with a small centered animated node-constellation pictogram and a
centered fuzzy-search input; typing switches to a results split — left a ranked op list, right the
selected op's detail with links to the published docs pages that mention it. `flux ops` becomes
the command group (bare `flux ops` prints help), reserving room for later tenants such as a
headless `flux ops list --json`. The full mechanism is in the design
(docs/designs/operations-explorer-epic.md); honor it — especially the DTO seam that keeps
flux-tui free of flux-tools/flux-web/flux-cli dependencies.

## Acceptance

- [ ] `flux ops --explore` opens the explorer in a real terminal and bails with a clear error when
      stdin/stdout are not TTYs (mirroring `run_with_options`); bare `flux ops` prints the
      subcommand help. `Commands::Ops` is classified in both exhaustive sandbox-posture matches in
      `crates/flux-cli/src/dispatch.rs` (read-only group) — `cargo test -p flux-codegate`'s
      classifier-coverage test is the failing-first proof that the arm exists.
- [ ] flux-tui gains `explorer.rs` (+ `pictogram.rs`) rendering a caller-supplied `Vec<OpRow>`;
      flux-tui's dependency list is unchanged. Assembly lives in `crates/flux-cli/src/ops_cmd.rs`:
      the `build_core_catalog` registry recipe, `specs()` order as the empty-query order,
      `registry.source(name)` as the provenance label, params derived via
      `flux_lang::opspec::schema_params` + property type/description extraction.
- [ ] Categories are derived by one unit-tested pure `categorize` fn ordered exactly:
      `flux_runtime::effective_group` → dotted-prefix families → file-ops set → core; test
      `categorize_representatives` pins at least git/rust/shell/web/browser/files/cognition/
      endpoint/network representatives. No new grouping mechanism is introduced.
- [ ] Doc links come from a committed generated `crates/flux-cli/assets/ops_docs.json` mapping op
      name → website page ids via the backticked-token scan of `website/docs/**/*.md` (the
      `website_contract.rs` rule), generic-name stop-list pinned to `language/ops`; a
      `FLUX_UPDATE_GOLDEN=1`-armed test (`ops_doc_index.rs`, using a `golden_mode` support copy
      with its arming test) regenerates and drift-checks it, and asserts: every registered op has
      an entry, no stale entries, every referenced page exists, stop-listed entries are
      `language/ops` only. Rows carry both the public URL and the local `flux docs` URL
      (labelled as requiring `flux docs`), derived by one unit-tested fn.
- [ ] Fuzzy search reuses the crate's existing ranker (`fuzzy_rank_indices`); two-focus key model:
      typing always edits the query; Enter enters command focus (j/k move, `y` copy, `?` help,
      `q` quit); Esc walks command→input, non-empty query→start, start→quit; Ctrl-C always quits;
      Ctrl-Y copies (OSC 52) from any focus; Tab/BackTab and ←/→ cycle the category filter;
      bracketed paste appends to the query. Failing-first tests:
      `filters_and_ranks_by_fuzzy_query`, `category_cycle_derives_and_filters`,
      `key_table_focus_model`.
- [ ] Start state renders the pictogram (own small grid module reusing the splash kit's
      `Rgb`/`Cell`/`lerp`/`Pcg32` and glow shader; seedable) centered above a centered input;
      animation frames are deterministic for a seed (`frames_deterministic_for_seed`), and under
      `Theme::MONO`/`NO_COLOR` the start state is static and uncolored
      (`mono_theme_start_state_is_static_and_uncolored`). The event loop is poll-based with no
      idle burn: animation deadlines exist only on the start screen (fast window after input,
      then slow shimmer), none in results.
- [ ] Results state: left list (selection glyph `▸`, risk glyph, name, muted first-sentence
      description, category tag when unfiltered) as hand-built lines (no ratatui `List`); right
      detail (description via `flux-markdown` rendering — pre-wrapped, so no wrapping Paragraph;
      params required-first; risk/idempotency/effects/group/source; both doc URLs + copy hint).
      Layout degrades without panicking: single-pane below ~70 cols, "terminal too small" floor,
      1×1 safe (`results_layout_lists_and_details`, `min_sizes_degrade_without_panic`,
      `start_state_renders_pictogram_and_centered_input`). All colors through `Theme` roles
      (pictogram palette excepted, per splash precedent); structure survives `Theme::MONO`.
- [ ] Evidence-gated ops are listed (registration is the catalog truth) with their group label
      visible; nothing filters on advertisement.
- [ ] WHATS-NEW.md gains an Unreleased entry in customer voice; the workspace gate and
      `cargo test -p flux-codegate` are green.

## Progress

- 2026-08-06 filed, with the implementation design recorded in the epic design doc.

## Notes

- Mount pattern: standalone loop like `crates/flux-tui/examples/loop_mocks.rs`; state/render as
  pure functions over an `ExplorerState` so `TestBackend` drives everything headlessly.
- `crates/flux-tui/src/operations.rs` is the Board/Fleet overlay — the name is taken; the new
  module is `explorer.rs`. Its tabs/selection/overlay chrome is the idiom to copy.
- Splash internals `Pcg32`/`lerp` need `pub(crate)` widening; `fuzzy_rank_indices` and
  `osc52_copy` are crate-root-private and already reachable from a new child module.
- OSC 8 hyperlinks are stripped by the TUI trust layer by design — links are plain text + copy
  action; a trusted opener is C-645's scope, not this story's.
- The generic-name stop-list exists because `` `read` ``/`` `map` `` etc. appear in docs as
  ordinary words; tune the final list by eyeballing the generated index diff.
