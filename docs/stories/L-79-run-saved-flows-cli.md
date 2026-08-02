---
id: L-79
title: Run saved flows directly from the CLI
pillar: Language
status: done
note: "flux flow list|ls + name-or-path flow run; strict --inputs/--arg contract; opt-in --map-inputs lowered into recorded ai.extract/parse/assert/bind nodes"
---

# Run saved flows directly from the CLI

## Goal

Make project and global saved flows first-class deterministic CLI programs: discover them without
starting an agent session, run them by filename stem or declared flow name, and pass typed inputs
without involving a model unless natural-language mapping is explicitly requested.

## Acceptance

- [x] One `flux-tools` catalog/resolver, backed by `System`, is shared by `flow_list`, `flow_run`,
  `flow_render`, `flux flow list` (`ls`), and CLI name resolution. Project/global precedence,
  filename/declaration aliases, malformed-file entries, and actionable op-only errors are tested.
- [x] `flux flow run` resolves an existing file before a stored name and accepts `--inputs <JSON>`,
  repeatable `--arg key=value`, and opt-in `--map-inputs <text>` alongside the existing model and
  resume flags.
- [x] Declared flow parameters are a strict CLI contract: explicit unknown/missing keys, malformed or
  non-object JSON, and deterministic TypeRef mismatches fail before the engine or flow effects run.
  Merge order is mapper base → JSON → args, with the last duplicate arg winning.
- [x] Deterministic values become literal `Bind` nodes. Missing mapped values become collision-free,
  recorded `ai.extract` → `parse(..., as: "json")` → one-object assertion → strict field binds. A
  fully deterministic invocation skips mapping even when `--map-inputs` is present.
- [x] Explicit module-local composite ops shadow auto-loaded names before validation; saved-name runs
  use the already auto-loaded declaration once.
- [x] Isolated binary tests prove offline deterministic execution, no-session listing, path priority,
  mixed flow/op execution from the global flows home, JSON/arg delivery, successful stub mapping,
  and malformed mapper output stopping before the original body.
- [x] Full workspace build/test/clippy/fmt and `flux-codegate` gates are green.

## Progress

- 2026-07-10 — Implemented the shared catalog in `flux-tools`, the CLI list/name/input surface, mapper
  AST lowering, composite shadowing, focused catalog/parser/input tests, and real-binary regression
  coverage. Full workspace build, test, clippy, formatting, and architecture gates are green.

## Notes

- C-376 deliberately revises the agent-side address policy: `flow_run` now accepts exactly one
  stored-flow `name` or workspace-confined `path`, so checked-in examples have a model-facing route.
  "Compatibility-lenient" continues to describe only input-key/type validation; the strict declared-
  parameter policy still belongs to direct CLI execution.
- Saved homes remain `.flux/flows` and `~/.flux/flows`; legacy `.flux/ops` homes remain readable.
- Standalone composite-op execution from the CLI, arbitrary extra flow directories, and JSON output
  for `flow list` are deliberately out of scope.
