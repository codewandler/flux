---
id: L-06
title: Let agents register reusable composite ops
pillar: Language
status: done
priority:
design: docs/designs/composite-ops.md
---

# Let agents register reusable composite ops

## Goal
Give the agent a root op that registers a new Flux-Lang composite op at runtime, with explicit
turn/session/project/global storage scope, so useful behaviors can be reused later without bypassing
the safety envelope.

## Acceptance
- [x] `op.register` accepts exactly one top-level composite `op` declaration and rejects invalid or
      unsafe definitions through existing composite validation.
- [x] The agent can choose `turn`, `session`, `project`, or `global` scope; project/global writes go
      through guarded `System` paths.
- [x] Registered ops appear in later planning/execution catalogs and execute through scoped composite
      dispatch, with all inner real ops still going through `Executor::dispatch`.
- [x] Tests cover session registration/reload, project persistence, and global named-root rejection.

## Progress
- Implemented in `flux-runtime`, `flux-tools`, and `flux-flow`.
- Documented in the composite-op design and flow ops reference.

## Notes
- `global` uses the `@global_ops` named root, prepared by the CLI as `~/.flux/ops`.
