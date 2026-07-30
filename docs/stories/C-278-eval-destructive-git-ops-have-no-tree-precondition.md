---
id: C-278
title: "`flux-eval`'s two most destructive git ops carry no tree precondition at all"
pillar: Core
status: ready
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

- [ ] A failing-first test demonstrates the hazard concretely: an untracked file present before the op,
      gone after it, with no refusal and no warning. That is the behaviour to argue about — it may turn
      out to be correct for a self-improvement loop, but it should be a decision with a test behind it.
- [ ] Each op gets a stated precondition, or a stated and reasoned exemption. "Top-level-loop-only by
      design" is a legitimate answer — but it must be **written down and enforced**, not inferred from
      where the op happens to be called today. If the exemption rests on the caller, say what stops a
      future caller from being different.
- [ ] Whatever is decided, C-249's `crates/flux-tools/tests/git_tree_policy.rs` module doc is updated so
      its exclusion of these two points at this story's outcome instead of at "out of scope".
- [ ] If a precondition is added, its refusal follows C-249's reconciled wording — tracked and untracked
      separated, with advice that is actionable for both.
- [ ] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap`.

## Progress

- (not started)

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
