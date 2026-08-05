---
id: L-130
title: "A first-class `board` declaration — retire the `kind \"board:*\"` datasource spelling"
pillar: Language
status: backlog
epic: first-class-board
design: docs/designs/first-class-board.md
areas: [flux-lang, flux-cli, flux-capabilities]
note: "Decision 0006: the board leaves the `datasource` slot — `board <name>` is its own declaration, and a datasource kind can no longer name something that mutates"
---

# A first-class `board` declaration — retire the `kind "board:*"` datasource spelling

## Goal

Give the work board its own Flux-Lang declaration so a Program writes

```flux
board tasks
  kind "markdown"
  path "./board"
```

instead of smuggling a write-capable surface through the read-only `datasource` slot with a
`board:` kind prefix. This is the Flux-Lang half of Decision 0006's board split: *anything that
mutates is not a datasource*.

## Acceptance

- [ ] `board <name>` is a first-class declaration (parser, analyzer, formatter, and the editor
      mirrors per `crates/flux-lang/AGENTS.md`), binding the same `WorkBoard` backends the
      `datasource` spelling binds today under the declaration's name as the operation prefix.
      Failing-first test: a program declaring `board tasks` resolves `tasks.claim`; it cannot today.
- [ ] `board` kinds are the bare backend names (`markdown`, `memory`); an unknown kind remains a
      hard startup error naming the kinds that exist — no fall-through, matching the existing
      datasource-kind rule.
- [ ] The `kind "board:*"` datasource spelling is retired with a migration note for existing
      programs: the loader's error (or a deprecation window decided in this story) tells the author
      the exact `board <name>` replacement rather than failing generically. The decision — hard cut
      vs. one deprecation release — is recorded here before implementation.
- [ ] A `datasource` declaration can no longer bind a write-capable backend by any spelling, pinned
      by test.
- [ ] Board subjects use the `board:` namespace (`board:<name>/item/<id>`) — coordinated with D-251
      so the grammar changes once.
- [ ] `website/docs/agent/datasources.md`, `website/docs/agent/fleet.md` and
      `website/docs/agent/programs.md` document the new declaration; the board-operation
      enumeration guard in `crates/flux-cli/tests/website_contract.rs` stays green.
- [ ] Standard gate green in both workspaces.

## Progress

- (not started)

## Notes

- Filed 2026-08-04 by C-514 from Decision 0006. Design:
  [first-class-board.md](../designs/first-class-board.md).
- The website currently documents the `board:` prefix as deliberate namespacing
  (`website/docs/agent/datasources.md` "Work boards"); that section moves to the new declaration
  when this lands.
- Coordinate with A-134 (SDK seam) so the Program-side and SDK-side names agree — both call the
  concept `board`.
