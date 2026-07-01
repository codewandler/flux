---
id: C-10
title: Structured op outputs — list-producing ops return arrays, not joined strings
pillar: Core
status: done
note: FIXED — glob binds a JSON-array value (readable joined view preserved); `each` sources and `merge` elements re-parse JSON-array strings (the store's JSON-as-string form); array values summarize by shape (`list (N): …`); the originally-failing glob→merge plan replays green
---

# Structured op outputs — list-producing ops return arrays, not joined strings

## Goal
Ops that conceptually produce lists must bind list VALUES, so downstream plan nodes (`each`,
`merge`, `parallel` fan-out, `jq`) compose without string-coercion luck. Observed live (2026-07-01,
session s_298): the model's first plan —
`$src_files = glob("src/*.py")` / `$test_files = glob("tests/*.py")` /
`$all_files = merge([$src_files, $test_files])` — is exactly the composition the planner prompt
teaches, and it failed deterministically (`merge: element 0 of 'lists' is not an array`) because
`GlobTool` returns `ToolResult::ok(matches.join("\n"))` (`crates/flux-tools/src/lib.rs`, GlobTool)
— a newline-joined string. One full repair round-trip wasted; the model then hardcoded paths. The
same stringly-value trap killed `strict_review.flux`'s aggregation (see L-14).

This undercuts the Language pillar's core claim: plans are "typed" but the values flowing through
them are strings-that-sometimes-parse-as-JSON.

## Acceptance
- [x] Failing-first: `$files = glob("*.md")` → `merge({lists: [$files, $files]})` executes
      (`flux-flow::runtime::glob_results_compose_with_each_and_merge`; also replayed live via
      `flux flow run` with the exact plan shape that failed in the review).
- [x] Failing-first: `each $f in $files` (glob-bound) iterates per-path — the `each` source now
      applies the same string-leaf re-parse rule (`jq_parse_input`) templates/`jq` already use.
- [x] `glob` binds a canonical JSON array via `ToolResult::ok_view` (view = joined lines,
      display unchanged — pinned in the extended glob tests). `merge` additionally accepts
      JSON-array *strings* as elements (the store's JSON-as-string form). Audit outcome for the
      rest: `grep` keeps its `file:line:text` hit lines (display-shaped, not a path list —
      converting would break the planner-prompt examples for no composability gain); datasource
      and plugin ops already return structured JSON.
- [x] Array values summarize by shape in the symbols block: `list (N): first, second, third, …`
      (`summarize` in flux-lang).
- [x] No display regression: glob's step output renders the joined view (`assert !view.contains('[')`
      pinned); merge's step view shows the real nested lists.

## Progress
- **DONE (2026-07-02).** `flux-tools`: GlobTool → `ok_view(JSON array, joined lines)`; MergeTool
  accepts JSON-array-string elements (`parse_json_array_string`). `flux-lang`: `each` source
  through `jq_parse_input`; `summarize` renders array values as `list (N): …`. New composition
  test (glob → merge + each end-to-end through `execute_flow`); glob unit tests extended to pin
  content=array/view=lines. Live-replayed the originally-failing plan: green. Full gate green.

## Notes
- Found during the 2026-07-01 harness e2e review.
- Storage already carries JSON-as-string values (known quirk) — this story is about the op-result
  boundary, not the store format.
- Audit starting points: `rg "join\(\"\\\\n\"\)" crates/flux-tools/src`.
