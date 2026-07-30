---
id: C-278
title: "`flux-eval`'s two most destructive git ops carry no tree precondition at all"
pillar: Core
status: in-progress
priority: 6
epic: security-assurance
design: docs/designs/security-assurance.md
note: "found by C-249: git_reset (reset --hard + clean -fd) and guard_protected (checkout + clean -fd) destroy uncommitted work unconditionally, and sit outside the family C-249 made policy — deliberately excluded there, so filed here rather than left in a test comment"
---

# `flux-eval`'s two most destructive git ops carry no tree precondition at all

## Goal

C-249 made the guarded git family's clean-tree precondition **policy** rather than per-op taste, and
proved it structurally. Its scan deliberately excludes two ops that are the most destructive in the
repository, because they live outside the declared family:

- **`git_reset`** — `reset --hard` followed by `clean -fd`
- **`guard_protected`** — `checkout <head> --` followed by `clean -fd`

Both destroy uncommitted **and untracked** work unconditionally. `clean -fd` removes untracked files
outright, which is precisely the case C-249 rewrote the refusal text for, because "commit or stash them
first" does not help someone whose files were never tracked. Decide what precondition these owe, and
make it enforced rather than assumed.

## Acceptance

- [x] A failing-first test demonstrates the hazard concretely: an untracked file present before the op,
      gone after it, with no refusal and no warning. That is the behaviour to argue about — it may turn
      out to be correct for a self-improvement loop, but it should be a decision with a test behind it.
- [x] Each op gets a stated precondition, or a stated and reasoned exemption. "Top-level-loop-only by
      design" is a legitimate answer — but it must be **written down and enforced**, not inferred from
      where the op happens to be called today. If the exemption rests on the caller, say what stops a
      future caller from being different.
- [x] Whatever is decided, C-249's `crates/flux-tools/tests/git_tree_policy.rs` module doc is updated so
      its exclusion of these two points at this story's outcome instead of at "out of scope".
- [x] If a precondition is added, its refusal follows C-249's reconciled wording — tracked and untracked
      separated, with advice that is actionable for both.
- [x] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap`.

## Progress

The two ops got **different** answers, because they are not the same kind of operation. Both are
stated in `crates/flux-eval/src/git.rs`'s module doc ("What licenses these ops to destroy
uncommitted work") and enforced by tests, not inferred from today's call sites.

**`git_reset` — precondition added, and its guarantee stated exactly.** The story's sharp question
was whether the loop's invariant ("everything in this tree was produced by the step I am undoing")
can be *checked* rather than trusted. **Answer: only partly, and the shipped guarantee is the
narrower one** — *a reset can only rewind within this checkout's own line of history*. That is
strictly stronger than the pre-C-278 behaviour (reset to whatever sha the payload named), but it is
not the loop's invariant, and review corrected an earlier draft of this note that claimed it was.
`licence_to_restore` runs two checks of unequal strength:

1. `clean: true` — **forgeable, so a hint rather than a proof.** `git_snapshot` refuses a dirty tree
   and only then emits the field, but nothing verifies a payload *came from* `git_snapshot`:
   `util::arg` accepts any caller-supplied object, so a flow may write
   `git_reset({"head": h, "clean": true})` by hand and be licensed on a tree it never snapshotted,
   reproducing the original hazard. It catches the caller who forgot to snapshot, not one that lies.
2. `git merge-base --is-ancestor` — **unforgeable, and where the real bound lives.** It interrogates
   the repository rather than the payload, so no caller can assert past it. This is what confines a
   forged `clean` to a rewind along history we are actually on. Reflexive, so equality passes.

Neither check looks at *recency*: a snapshot reused from an earlier round still carries `clean: true`
and is still an ancestor, so committed rounds since would be rewound — and `discarded` is built from
`git status --porcelain`, so rewound commits are reported nowhere. Not reachable from the shipped
flows (the snapshot is taken inside the `repeat` body), but the precondition does not prevent it.

The precondition is not a reflex: both `examples/improve-tbench.flux` and
`examples/improve-synthetic.flux` satisfy it on every reject path (`snapshot = git_snapshot()` then
`git_reset(snapshot)`, HEAD at or ahead of the snapshot), so the self-improvement loop still
discards candidates exactly as before. The refusal reuses C-249's tracked/untracked split. A
licensed reset now also reports what it consumed in `discarded`, which answers the "no warning"
half of the hazard for working-tree losses.

**`guard_protected` — reasoned exemption, enforced.** It is *not* a blanket restore, despite
matching C-249's `-fd` selector. Both argv it builds end in `--` followed by an explicit pathspec
list it computed itself and filtered through `is_protected`, so its blast radius is bounded **by
construction**, not by its caller — which is why "what stops a future caller being different" has
no bite for *which paths* it may touch: nothing about the caller can widen that. A clean-tree
precondition would also be incoherent, since the op exists to run *after* the worker has
deliberately dirtied the tree. The bound is now pinned by
`guard_protected_touches_nothing_outside_the_protected_paths`, which specifically holds
**untracked non-protected** files — the case an unscoped `clean -fd` would delete and which the
pre-existing tampering test never covered.

The exemption covers which paths are touched, **not which commit they are restored to**:
`snap["head"]` is unvalidated in `guard_protected`, and since this is the anti-cheat op that is a
live question rather than a settled one. Filed as **C-281**; deliberately not fixed here.

Docs updated: C-249's `git_tree_policy.rs` module doc now points at this outcome and states both
answers; `crates/flux-flow/docs/ops-reference.md` and `website/docs/language/ops.md` carry the
precondition and the exemption.

## Notes

- Found by **C-249**'s implementor while making the `flux-tools` family consistent. It correctly left
  these alone: they are outside its declared family, and it documented the exclusion in the test's
  module doc rather than letting the scan skip them silently. This story exists so the exclusion has an
  owner instead of living forever in a test comment.
- ⚠ The self-improvement loop *relies* on `git_reset` restoring a snapshot — see
  `examples/improve-tbench.flux`, where a red gate calls `git_reset(snapshot)` to discard a candidate.
  A precondition that refuses on a dirty tree would break that loop, which is the whole reason this
  needs thought rather than a reflex. The interesting question is whether the loop's own invariant
  ("everything in this tree was produced by the step I am undoing") can be *checked* rather than
  trusted.
- Related: [C-249](C-249-git-family-clean-tree-policy-and-stash-wording.md) established the policy and
  the refusal wording; [C-277](C-277-stale-confinement-exempt-and-readiness-coupling.md) owns the
  `?`-propagation shapes in the same family, which are a different defect.
