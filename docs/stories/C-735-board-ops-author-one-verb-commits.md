---
id: C-735
title: "Board ops author, one verb commits"
pillar: "Core"
status: in-progress
priority: 1
epic: delivery-is-verified
areas: [flux-cli]
note: "Of nine mutating board ops exactly one commits — create — and it commits the stub whose Acceptance reads 'Define acceptance.', so a story's committed form is the one where its definition of done does not exist. Since a board cannot see an uncommitted story, that leaves a window where a story is schedulable while meaningless. Acceptance is machine-read by board done and by C-723's withhold verification, yet there is no CLI to author it at all"
---

# Board ops author, one verb commits

## Goal

Every board mutation writes to disk and commits nothing; exactly one verb, `flux board commit`,
puts a planning document on the branch.

Today `create` is the only mutating op that commits, and what it commits is the stub — the form of
the story whose Acceptance reads `- [ ] Define acceptance.`. The meaningful content always arrives
as a second, uncommitted edit, and because a board read resolves items at a git ref, the story is
schedulable in exactly the state where its definition of done does not exist. `update`,
`transition`, `start`, `block`, `unblock`, `done`, `comment` and `evidence` leave the tree dirty,
so the operator already has to remember which side of the line each op falls on.

One rule with no exceptions replaces that: no op commits, no op takes `--commit`/`--no-commit`, and
`board commit` is the only verb that touches the branch. Deferring the commit opens a window in
which a document exists on disk and nowhere a board can read it, so `board check` reports an
uncommitted or untracked planning document as a finding — that is what makes deferral safe rather
than a regression, because it turns the blind window from silent into reported.

`board commit` never sweeps. It commits exactly the documents the invocation names, with a `--`
pathspec, refusing mid-merge and mid-rebase; `--all` means every uncommitted planning document
under the board's own document roots, never every dirty file in the repository. It re-reads the
commit it made and reports the paths that actually landed, not the paths it intended to write.

## Acceptance

- [x] No board op commits and none takes a commit flag: `BoardAction::Create` has no `no_commit`
      field, `CreateItemInput` has no `commit` field, `create_item` calls no commit path, and a
      created document is left on disk with `data.commit` null and absent from `HEAD`.
- [x] `flux board commit` is a board operation that commits exactly the planning documents the
      invocation names — explicit paths and `--item ID` — with a `--` pathspec, so a file dirty in
      the same checkout but outside that set is neither staged nor committed.
- [x] `board commit --all` is scoped to planning documents under the board's own document roots: a
      dirty source file, manifest or lockfile elsewhere in the repository is never staged or
      committed by it, and an explicit path outside those roots is refused as `permission`.
- [x] `board commit` refuses mid-merge and mid-rebase with `conflict/precondition`, naming the
      documents, which stay on disk.
- [x] `board commit` is idempotent: a second invocation over the same documents makes no commit,
      leaves `HEAD` where it was, exits 0, and says plainly that there was nothing to commit.
- [x] `board commit` reports only verified effect: the returned sha and document list are re-read
      from the commit that landed, and a commit whose content is not the requested set fails as
      `validation/gate` rather than reporting success from git's exit code.
- [x] `board check` reports every uncommitted or untracked planning document as a finding, naming
      the path, in both the envelope `warnings` and machine-readable `data`.
- [x] The `flux.cli/v1` envelope and exit-code classes are unchanged: `commit` appears in
      `board schema` operations as a mutation, `board call commit` routes like any other operation,
      and the session backend refuses it as it refuses every file-backed op.

## Progress

- Implemented on `impl/C-735`.
- `create` no longer commits. `commit_new_planning_document`'s mechanics are kept and generalised
  from one document to a set as `commit_planning_documents`, which additionally re-reads the commit
  it made (`git diff-tree`) and fails as `validation/gate` when the landed set is not the requested
  set. `data.commit` stays in `create`'s envelope and is always null.
- `board commit [PATH...] [--item ID] [--all] [-m MESSAGE]` added. Two fences:
  `BOARD_DOCUMENT_ROOTS` governs `--all` and the `check` finding; `board_committable_roots()` adds
  `CHANGELOG.md` for explicitly named paths only, because `board done --changelog` authors it but
  `--all` must never sweep the most contended ledger in a repository. `flux board commit --all
  CHANGELOG.md` is the way to land a `done` in one commit.
- `board check` gained the uncommitted/untracked finding, in `warnings` and `data.uncommitted[]`. A
  warning rather than an error: `board sync` runs the check before rendering, and a fatal finding
  would make the board unrenderable for as long as anyone is authoring.
- `require_canonical_member_checkout` now ignores board-owned dirt, because a `create` that no
  longer commits would otherwise make the very next workspace mutation fail as "checkout is dirty".
  Foreign dirt still refuses; if the exclusion probe cannot run, it falls back to refusing.
- The session backend's refusal is code-reviewed, not test-covered: reaching it needs a recorded
  session, and no sibling file-backed op (`reconcile`, `render`, `sync`, `import`) is covered that
  way either.
- Not in scope, and worth its own story: authoring ops for Goal/Acceptance/tick/progress. Acceptance
  is machine-read in two places — `board done` counts its checkboxes and C-723's driver parses the
  symbols and paths it names — so hand-edited markdown there is the same class of problem as
  hand-edited frontmatter.
- C-625 "Board planning mutations commit their writes" (backlog) now states the opposite contract
  (commit by default, `--no-commit` escape). It needs superseding; it is not this story's file.
