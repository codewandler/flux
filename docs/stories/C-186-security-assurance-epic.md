---
id: C-186
title: "Security assurance — close the gap between the envelope and its proof (epic)"
pillar: Core
status: ready
priority: 1
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW EPIC — every child traces to a CONFIRMED finding in one of the two 2026-07-29 adversarial reviews (desk review + envelope-integrity); architecture rated 8/10 while assurance rated 5/10, and the spread is the work"
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
- [ ] C-192 (the `sqlite_query` guarded-IO bypass), C-193 (statement allowlist) and C-194 (the
      mechanical no-direct-IO lint) are done. These trace to the **envelope-integrity** review, not
      the desk review, and they matter disproportionately: C-192 is the epic's only *confirmed
      bypass* of the envelope rather than a missing assurance step, and C-194 is the check that
      would have caught it at authoring time.
- [ ] A re-run of the [`adversarial-review`](../../.agents/skills/adversarial-review/SKILL.md) skill
      against the then-current version can mark findings 1–4 and classification trust **closed with
      evidence**, diffed against the 2026-07-29 baseline.
- [x] The deferred sandbox-default question (see Notes) has either become its own story or been
      consciously dropped with the reason recorded. → **C-217** files step 1 (make `on` report its
      resolved posture); step 2, the default flip itself, stays deferred behind it by design.

## Progress
- 2026-07-29 — epic opened from the review. Design:
  [security-assurance.md](../designs/security-assurance.md). Source review:
  [`reviews/2026-07-29-security-posture-desk-review.md`](../../reviews/2026-07-29-security-posture-desk-review.md),
  verified claim-by-claim against the tree at `0.33.1` — every child story cites a `path:line`, not
  the reviewer's prose.
- Ordering is **not** the review's ordering. Ranked by risk × reachability ÷ cost, which puts the
  supply-chain item first and the review's own headline finding out of scope (see Notes).
- 2026-07-29 — second review, `envelope-integrity` lens:
  [`reviews/2026-07-29-envelope-integrity.md`](../../reviews/2026-07-29-envelope-integrity.md).
  Added C-192, C-193, C-194. C-192 inserted at priority 2 — ahead of advisory scanning and the
  server limits — because it is model-reachable in any default session with no operator mistake and
  no third party required; C-188/C-189/C-190 shifted to 5/6/7. That pass **confirmed** the dispatch
  chain itself is sound on every path examined (shared `gate` between `dispatch` and a synchronous
  `authorize`, cap-scope checked before hooks, filesystem subjects normalized to physical identity,
  no production `Tool::execute` call, workspace root not model-reachable) — the failure came from
  outside the envelope, which is the argument for C-194 over more envelope hardening.
- 2026-07-29 — **eight of the epic's children landed** in two impl-coord waves, each merged only
  after its gate ran green and (for every envelope-touching change) an independent fresh-context
  review passed: **C-187** (SHA-pin actions + a CI pin guard), **C-192**/**C-193** (the confirmed
  `VACUUM INTO` guarded-IO bypass closed by a statement allowlist; review SOUND on 36k+ fuzzed
  inputs), **C-189** (daemon body limits + timeouts; review SOUND incl. timeout-cancellation
  leaving a valid session log), **C-188** (cargo-audit + cargo-deny advisory scanning; the real
  gate verified locally), **C-190** (unauthenticated-non-loopback refusal now holds at router
  construction, breaking; review SOUND, also closing the C-189 real-router auth-test gap), and
  **C-194** (the mechanical no-direct-IO lint; first cut was review-caught as bypassable in the
  unsafe direction, reworked into a string/comment-aware tokenizer, re-verified against a novel
  bypass). Two adjacent items were split out as their own stories: **C-195** (approval-sheet
  redaction, from C-185) and **C-205** (bump `lru`, drop its unsound-advisory ignore, from C-188).
  Also landed earlier the same day: **C-185** (the shared-redactor diff-marker fix).
- **Still open before this epic closes:** (1) **C-191** — the registry-wide `ToolSpec` invariant
  test — remains `backlog` on purpose: its invariant set must be agreed and written down before it
  is coded, so it was deliberately NOT fanned out. (2) The **re-run of the `adversarial-review`
  skill** against the now-current tree to mark findings 1–4 + classification-trust **closed with
  evidence**, diffed against the 2026-07-29 baseline. (3) The **sandbox-default deferral** (Notes
  below) still needs to become its own story or be consciously dropped.

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
