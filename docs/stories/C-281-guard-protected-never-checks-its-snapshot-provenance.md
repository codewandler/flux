---
id: C-281
title: "`guard_protected` restores the anti-cheat paths from a snapshot whose provenance it never checks"
pillar: Core
status: ready
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

- [ ] A failing-first test demonstrates the hazard concretely: `guard_protected` handed a snapshot
      whose `head` is a valid commit **off this checkout's line** restores the protected paths from it
      with no refusal. That is the behaviour to fix, and the test must distinguish it from the
      already-safe bogus-sha case, which errors for an unrelated reason.
- [ ] `guard_protected` refuses a snapshot it cannot place on this checkout's line. C-278 built
      exactly this check in `licence_to_restore` (`git merge-base --is-ancestor <head> HEAD`, with
      its three-way exit-code handling: `0` pass, `1` refuse, anything else "could not tell" and
      still refuse). **Reuse it rather than hand-rolling a second copy** — a hand-copied variant
      drifting from its original is the exact defect C-249 was filed for, and C-278's module doc
      names that history.
- [ ] The refusal follows C-249's reconciled wording via the shared `status_breakdown`, so this op's
      message cannot drift from its two siblings'.
- [ ] ⚠ **`clean: true` is the wrong half to demand here, and the story must say why in the code.**
      `guard_protected` exists to run *after* the worker has deliberately dirtied the tree — that is
      its entire purpose. Requiring the clean-tree token would break it in normal operation. Only the
      ancestry half applies. Whatever lands, state that asymmetry where the next reader will hit it,
      or someone will "fix" the missing check and break the loop.
- [ ] C-278's stated exemption for this op is updated: it argued blast radius is bounded by
      construction (pathspecs filtered through `is_protected`), which remains true and is a claim
      about *which* paths are touched. This story is about *what they are restored to* — a different
      axis. Make the module doc distinguish them so the exemption is not read as covering both.
- [ ] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap`.
- [ ] The self-improvement loop still discards a candidate correctly:
      `examples/improve-tbench.flux` and `examples/improve-synthetic.flux` both call this op. Say how
      you verified it — and note that C-278 verified the equivalent claim by *reading* the flows, not
      by executing a round, because no automated coverage exercises them.

## Progress

- (not started)

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
