---
id: C-186
title: "Security assurance — close the gap between the envelope and its proof (epic)"
pillar: Core
status: ready
priority: 1
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW EPIC — every child traces to a CONFIRMED finding in the 2026-07-29 adversarial desk review; architecture rated 8/10 while assurance rated 5/10, and the spread is the work"
---

# Security assurance — close the gap between the envelope and its proof (epic)

## Goal
The 2026-07-29 external adversarial review rated flux's security *architecture* 8/10 and its
security *assurance* 5/10. That spread is this epic. flux claims a non-bypassable authorization →
approval → guarded-IO envelope; today almost nothing outside the envelope proves it stays that way,
and the supply chain that delivers flux to a user is the softest part of the system. Close the
confirmed, actionable half of that gap — and leave a trail that lets the next review verify the
closure instead of re-deriving it.

## Acceptance
- [ ] C-187 (SHA-pin actions), C-188 (advisory scanning), C-189 (server limits) and C-190
      (construction-time auth invariant) are done, each with the failing-first test or failing-CI
      demonstration its story names.
- [ ] C-191 lands a registry-wide `ToolSpec` invariant test, converting the review's
      "classification trust" concern from an assumption into a gate.
- [ ] A re-run of the [`adversarial-review`](../../.agents/skills/adversarial-review/SKILL.md) skill
      against the then-current version can mark findings 1–4 and classification trust **closed with
      evidence**, diffed against the 2026-07-29 baseline.
- [ ] The deferred sandbox-default question (see Notes) has either become its own story or been
      consciously dropped with the reason recorded.

## Progress
- 2026-07-29 — epic opened from the review. Design:
  [security-assurance.md](../designs/security-assurance.md). Source review:
  [`reviews/2026-07-29-security-posture-desk-review.md`](../../reviews/2026-07-29-security-posture-desk-review.md),
  verified claim-by-claim against the tree at `0.33.1` — every child story cites a `path:line`, not
  the reviewer's prose.
- Ordering is **not** the review's ordering. Ranked by risk × reachability ÷ cost, which puts the
  supply-chain item first and the review's own headline finding out of scope (see Notes).

## Notes
- **Why C-187 leads.** It is the only finding exploitable by a third party with no flux bug and no
  operator mistake. The plugin trust model's per-artifact SHA-256 chain terminates in a Minisign
  signature whose key (`MINISIGN_SECRET_KEY`) lives in workflows that run unpinned third-party
  actions. Compromise there invalidates the signing story retroactively.
- **Deferred: the sandbox default.** The review's headline finding (sandbox `Off` by default,
  network open — `flux-system/src/sandbox.rs:39,:50,:64`, pinned by the test at `:1151`) is real but
  is a product decision, not a bug. Flipping it while `on` still degrades silently to unconfined
  (`:463`) would manufacture false assurance — worse than an honest `off`. Correct sequence: make
  `on` report its resolved posture loudly first, then revisit the default with its own design doc
  covering the Windows gap (no backend exists — only Bubblewrap and Seatbelt are implemented).
- **Out of scope by nature:** bus factor, adoption, and "get an external audit". Real risk, but no
  code change addresses them; they are context for the score, not to-dos.
