---
id: C-549
title: "The Track story format becomes a supported `flux board` backend and CLI"
pillar: Core
status: ready
priority: 44
epic: first-class-board
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli, flux-markdown, flux-capabilities]
note: "byte-compatible with codewandler/agentplugins track — story/epic/design/next/done/render need no Python or prompt-enforced edits"
---

# The Track story format becomes a supported `flux board` backend and CLI

## Goal

Make existing `docs/stories` repositories directly manageable through Flux while preserving their
files, Goal and Acceptance contracts and generated-board format.

## Acceptance

- [ ] Golden parity fixtures compare `flux board render` with the Track `gen_board.py` output across
      all statuses, priorities, epics, notes, warnings and natural-id ordering; hand-written text
      outside the marker pair is byte-preserved and a second render is idempotent.
- [ ] The planning profile parses and validates the existing YAML frontmatter, rejects duplicate or
      filename-mismatched ids, requires integer priority for ready work and never normalizes a file
      silently during a read.
- [ ] CLI coverage includes `init --scaffold`, `ls`, `show`, `items`, `get`, `query`, `next`,
      `create --kind story|epic|design`, `update`, `transition`, `start`, `block`, `unblock`, `done`,
      `comment`, `evidence`, `check`, `render`, `sync`, `graph`, `stats`, `report`, `import` and
      `export`, through flags and C-547 JSON requests.
- [ ] `vision show|set` and `roadmap show|set` manage revisioned singleton documents;
      `decision list|show|create|update|accept|supersede` manages stable decision records; `design`
      manages linked design documents. None appears in the ready queue or receives a story status.
- [ ] Decisions expose `open`, `decided` and `superseded`; open records carry a question, structured
      options/trade-offs, recommendation and linked blocked stories. Deciding restores only those
      stories while unrelated ready work remains schedulable.
- [ ] Story ids allocate without races; create never clobbers; design links an existing document;
      epic and done are recoverable multi-file change sets; every mutation supports dry-run and
      optimistic revision.
- [ ] `done` refuses unchecked Acceptance unless an explicit reasoned override is recorded, updates
      status/priority, adds the supplied changelog entry and renders the board as one recoverable
      operation.
- [ ] `flux board skill` is short enough for routine prompt injection, tells an agent to inspect
      vision, roadmap, applicable decisions, Goal/Acceptance and linked design before mutation,
      tells it to use JSON mode for mutations, and all its examples pass against a fixture.
- [ ] Existing WorkBoard Markdown (`+++` TOML under `board/items`) remains a distinct execution
      backend; Track compatibility does not reinterpret or rewrite it.
- [ ] `board stats` reports status distribution plus epic/story/optional-task/Acceptance-criterion/
      implementation done/remaining/total/percent; vision/roadmap/decision/design counts; canonical
      commits and daily Git history. Track's absent task schema is explicit null, not zero.
- [ ] Website docs, migration guide, `WHATS-NEW.md` and changelog are updated. Targeted CLI/backend
      tests pass; the final board wave owns the full gate.

## Notes

- Depends on C-547, A-134 and L-130. The external plugin shim lands only after this command surface
  is released and proven.
