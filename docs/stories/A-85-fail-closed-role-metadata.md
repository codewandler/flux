---
id: A-85
title: Fail closed on malformed role metadata
pillar: Agent
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: malformed YAML currently defaults to tools absent, which inherits the parent's full catalog
---

# Fail closed on malformed role metadata

## Goal

Make file-defined sub-agent roles reject malformed or unreadable metadata instead of silently
broadening their tool ceiling.

## Acceptance

- [x] `try_parse_role` and `RoleRegistry::try_load` (or equivalent) return path-aware errors for
      malformed YAML, wrong field types, invalid effort/loop values, duplicate roles, and unreadable
      role files.
- [x] Failing-first tests prove malformed `tools` metadata cannot become `None` and inherit the full
      parent registry; a spawn attempt fails before provider construction or tool dispatch.
- [x] A valid role that genuinely omits `tools` retains the documented inherit-parent behavior, while
      explicit `tools: []` continues to grant none.
- [x] CLI, App, SDK, and programmatic role discovery use the strict APIs and surface actionable file
      locations; any retained lenient API is deprecated and cannot back production discovery.
- [x] Existing role precedence, provider/model inheritance, and capability-scope intersection remain
      unchanged and covered.

## Progress

- 2026-07-14 — Added strict path-aware role parsing/loading and migrated production discovery;
  lenient helpers are deprecated. Role-unit and CLI startup tests prove malformed `tools` fails
  before provider/tool execution while omitted and explicit-empty tool lists retain distinct meaning.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Coordinate filesystem discovery with C-61; this story owns parse/load failure semantics and the
  least-privilege guarantee.
