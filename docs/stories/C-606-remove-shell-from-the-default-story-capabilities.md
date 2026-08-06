---
id: C-606
title: "Remove shell from the default story capabilities"
pillar: "Core"
status: ready
priority: 6
epic: agent-evidence-scope
areas: [flux-cli]
design: docs/designs/agent-evidence-scope.md
note: "bash/proc.run call no path resolver; with reads unrestricted at every other layer this is host-wide read authority"
---

# Remove shell from the default story capabilities

## Goal

A story writer must not hold arbitrary shell. `bash` and `proc.run` take argv verbatim and never call
`Workspace::resolve*`, so a single call defeats the entire ceiling — and because reads are unrestricted
at every other layer, that is unbounded read authority over the whole host, not just the worktree.

## Acceptance

- [ ] Failing first, a test proves `DEFAULT_STORY_CAPABILITIES` does not grant `shell`, and that a
      wave dispatched without an explicit template therefore admits no `bash`/`proc.run`.
- [ ] A writer template still has a path to targeted validation: the typed toolchain bundles
      (`rust`, `node`, …) remain grantable and carry real schemas.
- [ ] A template that explicitly declares `shell` is still honoured — this narrows the *default*, it
      does not remove the capability from the vocabulary.

## Progress

- Not started.

## Notes

- `bash` runs `["sh","-c", command]` verbatim; `proc.run` runs `[program, ...args]` verbatim. Neither
  resolves through the workspace guard, which is the only read guard that exists (the OS sandbox binds
  `--ro-bind / /` on Linux and uses `(allow default)` on macOS — reads are never denied on either).
- The live deployment's `fleet.toml` was edited on 2026-08-06 to drop `shell` from the `story-worker`
  template, but that fixes one directory. `DEFAULT_STORY_CAPABILITIES` in the binary still grants it,
  so any wave dispatched without a template is unaffected. That per-directory/global asymmetry is
  itself the subject of C-604.
- Every worker up to and including `wave-286-worker-1` ran with
  `capabilities: ['edit','git','read','shell']`.
