---
id: C-236
title: "Structured `board.query`, a `board.comments` read-back, and raw-string cognition results"
pillar: Core
status: in-progress
epic: fleet-loop
areas: [flux-capabilities, flux-datasource, flux-tools, flux-sdk]
note: "Milestone-1 story F1 of the fleet-loop plan: a coordinator Program cannot reason over a board it can only read as prose. Also fixes C-235."
---

# Structured `board.query`, a `board.comments` read-back, and raw-string cognition results

## Goal
Board ops return human prose with no `output_schema`: `board.list` (`render_compact`) exposes only
`title, state, attempts, assignee` and omits `runner, task_id, depends_on, repo`, so Flux-Lang's
`each`/`match` has nothing typed to iterate — a coordinator cannot reason over its own board. Add a
structured `board.query` op (typed rows + a real `output_schema`, plus a `depends_on` filter so
"ready and unblocked" is one call), a `board.comments` read-back op (the write path exists; nothing
reads it), and fix C-235: a string-returning cognition op must yield the string, not its JSON
encoding.

## Acceptance
- [x] `each $item in board.query({...})` binds `id`/`runner`/`task_id` and a `match` on `state`
      works end to end. **Failing-first test**:
      `crates/flux-sdk/tests/board_iteration.rs::a_program_iterates_the_board_and_matches_on_state`
      — impossible today (`board.query` is not registered).
- [x] `board.query` carries an `output_schema` and returns structured rows, not prose.
- [x] "ready and unblocked" is expressible via a `depends_on` filter that treats an item as blocked
      until every dependency is `done` (an absent dependency is not `done`). Pinned for every
      backend in the shared contract suite.
- [x] `board.comments` reads back what `board.comment` wrote, for both backends — via a new
      `WorkBoard::comments` port method (a deliberate breaking change to
      `codewandler-flux-capabilities`: no default body, both backends implement it, the contract
      suite pins the property).
- [x] A string-returning cognition op yields the raw string; a chained journey (extract a URL, feed
      it to `fleet.status`, which parses a URL) succeeds — the exact shape that failed the 0.36.0
      smoke test (C-235).
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — filed as fleet-loop F1; implementation on `impl/C-236`.
- 2026-07-30 — **complete on `impl/C-236`.** The first implementor died mid-flight leaving 1039
  uncommitted lines with zero commits; those were preserved as a WIP commit, then assessed. The
  inherited state was tests-only plus a corrupted source region: `item_row` had been emitted 15
  times and `query_output_schema` 3 times (a generation loop), no `Query`/`Comments` spec arms
  existed, neither backend implemented `comments` or the `depends_on` filter, and **the C-235 fix
  itself was never written** — only its expectations had been flipped. All of that is now done:
  - `flux-datasource`: `DependencyMatch` + `DEPENDS_ON_FILTER` beside `validate_transition`, so one
    definition of "unblocked" serves both backends.
  - `flux-capabilities`: `query` (typed rows + `output_schema` + the `depends_on` filter) and
    `comments` (`output_schema`); `WorkBoard::comments` with **no default body**; `depends_on`
    reserved in `validate_board_contract` exactly like `state`; `page_schema` shared by `list`/`query`
    so only `query` advertises `depends_on`.
  - `MemoryBoard` + `MarkdownBoard`: both implement `comments` and the `depends_on` filter;
    `MemoryBoard`'s inherent `comments` (which returned `Vec` and swallowed an absent id) is gone,
    superseded by the trait method that errors. Markdown read-back is the exact inverse of
    `comment`'s render — top-level `- ` bullets, in file order — via a new `read_document`.
  - `flux-tools`: `selected_content` applies the workspace's string convention to the four
    *selecting* ops (`regex_extract` single, `first`, `last`, `coalesce`). C-235 discharged.
  - Gate green in full; `examples/bitcoin-price.flux` becomes correct without a source change,
    confirming the story's "no compensating consumer" finding.
  - ⚠ `scripts/check-crate-versions.sh` FAILs on `codewandler-flux-datasource` (content changed,
    still `1.1.0`). Versions are fenced — the coordinator owns that bump. `WorkBoard::comments`
    is a breaking change to the published `codewandler-flux-capabilities` crate.

## Notes
- **C-235 verdict (with evidence): the bug is in the ops, not the engine.** The interpreter binds op
  output verbatim as `Value::String(result.content)` (`crates/flux-lang/src/runtime.rs:466`) and
  never JSON-parses a bare string; the C-10 string-leaf re-parse rule
  (`jq_parse_input`, `runtime.rs:4018`) re-reads only objects/arrays for `each`/`jq`/field access.
  The workspace convention is already "a bare `String` passes through unquoted … anything else
  becomes its compact JSON encoding" (`crates/flux-runtime/src/fn_tool.rs:30-37`). So the fix is
  uniform across the string-leaking ops: `regex_extract` (single match), `first`, `last`,
  `coalesce`. Array/object-returning ops (`split`, `regex_extract all:true`, `keys`, …) were
  already correct — the re-parse rule covers them.
- **Breaking, said plainly:** `regex_extract`/`first`/`last`/`coalesce` string results lose their
  JSON quotes. No compensating consumer exists in-repo (the only quoted expectations are the ops'
  own unit tests; `examples/bitcoin-price.flux` was silently wrong, not compensating).
- `board.list` keeps its prose rendering for humans — `query` is additive, not a rename. It returns
  one page as a bare JSON array so `each` can consume it directly; cursor-paged reading stays with
  `list`.
- The `depends_on` filter is **query-only** at the host: `list` stays the human view and its filter
  vocabulary is unchanged. Both backends implement the semantics in `WorkBoard::list` (the port
  method `query` shares), single-sourced as `DependencyMatch` in `flux-datasource` beside
  `validate_transition`.
- Plan: `~/.claude/plans/compiled-knitting-cupcake.md` (Milestone 1, F1).
