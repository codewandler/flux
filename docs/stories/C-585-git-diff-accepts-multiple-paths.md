---
id: C-585
title: "Let git_diff inspect multiple paths in one operation call"
pillar: Core
status: backlog
epic: batched-operation-inputs
design: docs/designs/batched-operation-inputs.md
areas: [flux-tools]
note: "make path a bounded string-or-array and invoke one fixed git diff argv with exact per-path permission subjects"
---

# Let git_diff inspect multiple paths in one operation call

## Goal

Allow an agent to request one staged or unstaged diff for several workspace paths without paying for
one operation call and result envelope per path.

## Acceptance

- [ ] A failing-first tool test proves `git_diff { path: ["a", "b"] }` is rejected by today's
      string-only schema even though the guarded Git command supports both pathspecs in one argv.
- [ ] `path` accepts either the existing string or a bounded non-empty string array. Omission still
      means the complete diff; empty, non-string, over-count and over-byte arrays fail before process
      execution with typed actionable errors.
- [ ] The implementation executes one argv containing the existing `--no-ext-diff`, `--no-textconv`
      and optional `--staged`, followed by one `--` and every path as its own argv element. It never
      invokes a shell or one child process per path.
- [ ] Permission subjects contain every supplied path in deterministic order. Multiple paths never
      collapse to broad `git_diff`, workspace or wildcard authority, and the singular subject remains
      compatible.
- [ ] Fixtures cover staged and unstaged changes, one/many/omitted paths, spaces, Unicode, a leading
      dash and Git pathspec metacharacters; unrelated files are absent and output/error/truncation
      behavior remains the ordinary single `git_diff` contract.
- [ ] Live schema/skill/operation references advertise the array form, focused tests and the full
      repository gate pass, and user-visible behavior is recorded in changelogs/embedded docs.

## Progress

- Not started.

## Notes

- One Git invocation returns one combined diff. This is not C-528-style concurrent execution and
  does not concatenate several independently capped results.
