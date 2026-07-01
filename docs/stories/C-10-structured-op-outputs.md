---
id: C-10
title: Structured op outputs — list-producing ops return arrays, not joined strings
pillar: Core
status: ready
priority: 5
note: `glob` returns matches.join("\n") — so `merge`/`each` can't consume it and the model wastes a repair round-trip; the "typed plan" composability story breaks at stringly op values
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
- [ ] Failing-first: a flow `$files = glob("*.md")` → `merge({lists: [$files, $files]})` executes
      (today: "element 0 is not an array").
- [ ] Failing-first: `each $f in $files` (glob-bound) iterates per-path, not per-character/one blob.
- [ ] `glob` (and an audited sweep of other list-shaped ops — `grep` match lists, datasource
      `search`/`list`, `plugin` list ops) bind a canonical JSON array value; the model-facing
      *view* stays the readable joined form (the value/view split in `ToolResult` exists for
      exactly this — the transcript keeps showing joined lines).
- [ ] The symbols block renders list-typed symbols with their type/length (`$files: list (3)`), so
      the planner knows the shape it can compose on.
- [ ] No behavioral regression on plain display paths: CLI step output for `glob` unchanged
      (pinned by an existing or new snapshot test).

## Progress
- (not started)

## Notes
- Found during the 2026-07-01 harness e2e review.
- Storage already carries JSON-as-string values (known quirk) — this story is about the op-result
  boundary, not the store format.
- Audit starting points: `rg "join\(\"\\\\n\"\)" crates/flux-tools/src`.
