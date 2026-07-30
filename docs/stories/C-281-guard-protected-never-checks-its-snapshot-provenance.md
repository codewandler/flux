---
id: C-281
title: "`guard_protected` restores the anti-cheat paths from a snapshot whose provenance it never checks"
pillar: Core
status: in-progress
priority: 5
epic: security-assurance
design: docs/designs/security-assurance.md
note: "found by C-278 and deliberately left there — a *valid but divergent* commit silently restores the protected paths to an unrelated state, and this is the anti-cheat op, so a wrong restore is an integrity problem rather than a wrong file"
---

# `guard_protected` restores the anti-cheat paths from a snapshot whose provenance it never checks

## Goal

C-278 made `git_reset` prove its licence to destroy uncommitted work: the payload must carry
`clean: true` (a proof-carrying token, since `git_snapshot` emits it only after refusing a dirty
tree) **and** `head` must be an ancestor of `HEAD`. `guard_protected` — which restores the protected
grader/loop paths after a worker has had its way with the tree — got neither check, and does not
validate its snapshot's `head` at all.

A bogus sha fails safe: `git diff --name-only` errors and the op stops. The hole is a **valid but
divergent** commit. `guard_protected` would restore the protected paths to whatever they were on an
unrelated line of history, silently and with no refusal — and this is the **anti-cheat** op. A wrong
restore here is not a wrong file; it is the integrity check itself being quietly reset to a state
nobody chose.

## Acceptance

- [x] A failing-first test demonstrates the hazard concretely: `guard_protected` handed a snapshot
      whose `head` is a valid commit **off this checkout's line** restores the protected paths from it
      with no refusal. That is the behaviour to fix, and the test must distinguish it from the
      already-safe bogus-sha case, which errors for an unrelated reason.
- [x] `guard_protected` refuses a snapshot it cannot place on this checkout's line. C-278 built
      exactly this check in `licence_to_restore` (`git merge-base --is-ancestor <head> HEAD`, with
      its three-way exit-code handling: `0` pass, `1` refuse, anything else "could not tell" and
      still refuse). **Reuse it rather than hand-rolling a second copy** — a hand-copied variant
      drifting from its original is the exact defect C-249 was filed for, and C-278's module doc
      names that history.
- [x] The refusal follows C-249's reconciled wording via the shared `status_breakdown`, so this op's
      message cannot drift from its two siblings'.
- [x] ⚠ **`clean: true` is the wrong half to demand here, and the story must say why in the code.**
      `guard_protected` exists to run *after* the worker has deliberately dirtied the tree — that is
      its entire purpose. Requiring the clean-tree token would break it in normal operation. Only the
      ancestry half applies. Whatever lands, state that asymmetry where the next reader will hit it,
      or someone will "fix" the missing check and break the loop.
- [x] C-278's stated exemption for this op is updated: it argued blast radius is bounded by
      construction (pathspecs filtered through `is_protected`), which remains true and is a claim
      about *which* paths are touched. This story is about *what they are restored to* — a different
      axis. Make the module doc distinguish them so the exemption is not read as covering both.
- [x] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap`.
- [x] The self-improvement loop still discards a candidate correctly:
      `examples/improve-tbench.flux` and `examples/improve-synthetic.flux` both call this op. Say how
      you verified it — and note that C-278 verified the equivalent claim by *reading* the flows, not
      by executing a round, because no automated coverage exercises them.

## Progress

**Landed.** `guard_protected` now refuses a snapshot it cannot place on this checkout's line of
history, and the refusal is the *same code path* as `git_reset`'s, not a second copy of it.

**The reuse is a split, not a call-through.** C-278's `licence_to_restore` bundles two checks of
unequal strength, and only one of them belongs here. Its ancestry half is now
`off_this_checkouts_line(ctx, op, head, because)` in `crates/flux-eval/src/git.rs`: `git_reset`
reaches it through `licence_to_restore` exactly as before, and `guard_protected` calls it directly.
The three-way exit handling (`0` pass, `1` refuse, anything else "could not tell" and still refuse),
the sha rendering and the `status_breakdown` tail live in one place, so this op's message cannot
drift from its siblings'. Only the per-op *consequence* clause is a parameter — mirroring the
existing `dirty_tree_refusal(op, because, status)` in the same module.

**`clean: true` is deliberately not demanded, and that is written where it will be read.** Stated
three times, deepest first: at the `guard_protected` call site (the comment a would-be "completer"
hits), in the tool's doc comment, and in the module doc's licence section. It is also *enforced*,
which matters more than the prose: `guard_protected_restores_grader_and_loop_tampering` and
`guard_protected_touches_nothing_outside_the_protected_paths` both hand the op a payload with **no**
`clean` field on a deliberately dirty tree and expect a restore, so adding the clean half turns them
red rather than shipping. The new test's doc comment names that property.

**The two axes are now separated wherever C-278's exemption was stated.** The module doc's
`guard_protected` bullet is split into (1) *which paths it may touch* — the C-278 exemption, still
standing and still resting on `is_protected` filtering — and (2) *which commit those paths are
restored from* — this story's precondition. `crates/flux-flow/docs/ops-reference.md` got the same
split; `crates/flux-tools/tests/git_tree_policy.rs`'s module doc (which C-278 was required to
update) now says the exemption is from a *clean-tree* precondition specifically and points at the
ancestry check C-281 added.

**Failing-first, with the distinction the story asked for.**
`guard_protected_refuses_a_snapshot_taken_off_this_checkouts_line` runs two acts against a fixture
whose protected grader differs between the two lines. Act 1 hands it a bogus sha and asserts only
that the op stops and restores nothing — deliberately tolerant of *how*, because before this story
it halted on `git diff --name-only` failing to resolve the object and now it refuses on the
ancestry check; that act exists to show the bogus case proves nothing. Act 2 hands it a real commit
made on another branch. At the merge base it returned `{"restored":["crates/flux-eval/src/score.rs"],
"tampered":true}` — the grader silently replaced by the divergent line's version. It now refuses,
with the tree listed and untouched.

**The loop was verified by reading the flows, not by executing a round** — the same method C-278
used for the equivalent claim, and for the same reason: nothing automated exercises them. All three
callers (`examples/improve-tbench.flux:15,19`, `improve-synthetic.flux:15,17`,
`improve-multi.flux:15,17`) take `snapshot = git_snapshot()` and call `guard_protected(snapshot)`
inside the same `repeat` body, with nothing between them that can move HEAD off the line. Ancestry
therefore holds — reflexively when HEAD has not moved at all, which is the normal case, since
`change_implement`'s workers edit files and the git ops are registered top-level only. The check
cannot fire on a healthy round.

**What this does *not* fix.** Staleness is still out of scope, exactly as for `git_reset`: a
snapshot from an earlier round remains an ancestor and is still honoured. And `head` remains
caller-supplied — the ancestry check bounds *where* it may point, never establishes that the payload
came from `git_snapshot`.

## Notes

- Found by **C-278**'s implementor, which fixed the sibling op and deliberately did not fold this in:
  folding it would have muddied the *exemption* it was arguing for `guard_protected` on the
  tree-precondition axis. That was the right call — the exemption and this defect are about different
  properties — and it reported the finding rather than leaving it in a comment.
- The fix is small. C-278's implementor estimated three lines against `licence_to_restore`'s existing
  ancestry check. The value is in the test and in the doc that stops the `clean: true` half from
  being added back.
- Related: [C-278](C-278-eval-destructive-git-ops-have-no-tree-precondition.md) built the licence
  machinery and stated the exemption this narrows;
  [C-249](C-249-git-family-clean-tree-policy-and-stash-wording.md) established the refusal wording and
  the anti-drift rule.
