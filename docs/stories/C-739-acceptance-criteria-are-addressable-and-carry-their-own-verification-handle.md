---
id: C-739
title: "Acceptance criteria are addressable and carry their own verification handle"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-cli]
---

# Acceptance criteria are addressable and carry their own verification handle

## Goal

Acceptance criteria are anonymous bullets, so nothing can reference one. A worker cannot report
evidence per criterion, review cannot be scoped to one, and a partially-satisfied story has no
representation — the only states are "all ticked" and "not done".

Kiro backlinks `_Requirements: 1.1_` and Spec Kit uses `FR-001` for exactly this reason. Our
checkboxes are most of the way there; they need stable ids. And C-723 already gropes toward a
verification handle by scraping backticked symbols out of prose — a criterion should simply name the
command that proves it.

## Acceptance

- [x] Each criterion carries a stable id, allocated once and never renumbered, that a handoff, a
      review finding and a doctor report can all cite.
- [x] A criterion may declare its own verification handle — the exact command, test name or
      observable artifact that proves it — and that handle is what evidence is checked against.
- [x] Coverage is computable: which criteria are claimed, by which commit, with what evidence.
- [x] C-723's `acceptance_artifacts` prefers a declared handle over scraping backticks from prose,
      and says which it used.
- [x] Existing stories without ids keep working. This is additive; a story is not invalid for
      predating it.
- [x] Regression test: a worker's handoff cites a criterion id, and a criterion whose declared
      verification did not run is reported as unproven rather than counted as satisfied.

## Syntax

```markdown
- [ ] `AC-1` A reclaimed wave's worktrees are provably gone.
      verify: `cargo test -p flux-cli reclaim_removes_the_worktrees_it_reports`
```

The example above deliberately omits its `## Acceptance` heading line, and that is not a
typographical preference. `section_contract` decides which section it is in by scanning `## `
headings with no awareness of fenced blocks, so a heading written *inside* a fence re-opens the
section for the parser. The first draft of this story included the heading and thereby gave itself a
seventh acceptance criterion — the example bullet — which could never be ticked and which made the
story permanently unclosable by the very feature it was introducing. See
[C-750](C-750-the-acceptance-parser-is-not-fence-aware-so-a-story-documenting-the-syntax-corrupts-its-own-contract.md).

The id is a backticked `AC-<n>` opening the bullet — written down rather than derived from
position, which is what makes it survive an insertion, a deletion or a reorder. `AC-` collides with
no board id prefix in use (`C-`, `X-`, `D-`, `E-`), so a bullet opening with a story id is citing
that story rather than defining a criterion. The handle is an indented `verify:` continuation line
holding a command, a test name or a path.

Both are optional. Nothing is retrofitted: 1,260 stories declare neither, and every one of them
parses, counts and validates exactly as before.

## Progress

- One parser. `checkbox_counts` — the counter shared by `board done`, `board reconcile`, `board
  stats` and C-723's `verify_already_built` — is now derived from `section_contract`, so ids,
  handles and counts can never be two answers to the same question.
- Counting parity proved on the corpus rather than argued: the legacy algorithm and the new binary
  both report `done=4032 total=6707` over the same 1,260 stories, and `flux board check` stays
  green on all of them.
- `board check` refuses only what makes an id meaningless — a duplicate, a malformed `AC-01`, a
  `verify:` under no criterion, an empty or doubled handle. A story declaring no ids contributes no
  findings by construction.
- `flux fleet handoff --criterion AC-1` resolves each citation against the story *at the handed-off
  commit* and refuses one that does not resolve, before any validation runs. A renumber therefore
  breaks loudly, which is the only thing that makes "allocated once" enforceable.
- `flux fleet coverage BOARD/ITEM` joins the story's criteria to every wave's handoffs: `proven`
  needs the criterion's own declared handle discharged inside a validation that passed; `unproven`
  is claimed-and-not-discharged; `claimed` has evidence but no declared handle; `unclaimed` and
  `unaddressable` are the rest. A claim naming an id the story no longer declares is reported as
  dangling, not dropped.
- `acceptance_evidence` replaces the bare scrape and reports `handle_source: declared|scraped`.
  Declared handles win when any names something checkable; a handle naming nothing checkable falls
  back to the scrape rather than to an empty list, because absence is what releases a story and an
  empty list is not evidence of absence.

## Notes

- Not in scope, deliberately: migrating existing stories to ids, and the story template. Both
  belong with C-738.
- `fleet rework` and `fleet doctor` resolve criterion ids through the same parser but take no
  `--criterion` flag yet; the citation surface wired here is the handoff, which is where a worker's
  claim actually lands.
