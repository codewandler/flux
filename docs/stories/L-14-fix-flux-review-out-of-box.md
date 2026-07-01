---
id: L-14
title: Make `flux review` work out of the box (roles missing from the binary; aggregation dies on real reviewer output)
pillar: Language
status: ready
priority: 4
epic: strict-review-flows
design: docs/designs/strict-review-flows.md
note: broken everywhere today — outside this repo it fails "unknown role review-security" (roles never embedded); inside it the flow dies at `merge` on real (fenced/prose) reviewer output AFTER paying for 3 sub-agent calls
---

# Make `flux review` work out of the box

## Goal
`flux review --files <p>` (L-13) claims to be self-contained ("the built-in reviewer roles + the
embedded flow text ship in the binary, so flux review works in any repo"). Verified 2026-07-01: it
works in **no** repo:

1. **Any other repo:** `error: strict_review: runtime error: step 'task' failed: unknown role:
   review-security`. `DEFAULT_ROLES` (`crates/flux-cli/src/main.rs:1108`) contains
   scout/planner/worker/reviewer/evaluator/summarizer — the three `review-*` roles exist only as
   files in THIS repo's `.flux/agents/` (committed, but never embedded), so `load_roles`' built-in
   fallback can't supply them.
2. **This repo (roles load fine):** `flux review --files crates/flux-core/src/pricing.rs` runs the
   three reviewer sub-agents (real model spend) and then dies at
   `step 'merge' failed: merge: element 0 of 'lists' is not an array` —
   `examples/strict_review.flux:50` merges the three raw `task` outputs, which are model TEXT; the
   string→JSON coercion only saves it when every reviewer returns a bare JSON array. Fenced
   (```json), prose-wrapped, or partially-valid output kills the whole flow *after* the money is
   spent — and the L-12 quarantine machinery (`review.normalize`) sits AFTER the merge, where it can
   never help. The L-13 headline test passes because its mock sub-agents return clean JSON arrays.

## Acceptance
- [ ] Failing-first: in a bare temp repo (no `.flux/`), `flux review` role resolution succeeds — the
      three `review-*` roles come from the binary (embed the committed `.flux/agents/review-*.md`
      via `include_str!` into `DEFAULT_ROLES` or a review-roles fallback; a project's own files
      still override).
- [ ] Failing-first: a reviewer sub-agent returning ```json-fenced``` output (and one returning
      prose + junk) does NOT abort the flow — each reviewer's raw text is normalized/quarantined
      individually (per-reviewer `review.normalize`, or `review.aggregate` accepting raw strings)
      before any array-shape op touches it; the report carries the quarantined entries.
- [ ] The strict-review SDK/journey byte-equality test gains a "dirty reviewer output" variant so
      mock-only clean JSON can't green-light this path again.
- [ ] Live: `flux review --files <some file>` in a scratch repo prints a markdown report end-to-end.

## Progress
- (not started)

## Notes
- Found during the 2026-07-01 harness e2e review.
- Related: C-10 (structured op outputs) removes the broader "merge over stringly values" trap this
  flow fell into; this story still stands alone because the roles gap is independent and the flow
  must tolerate dirty model output regardless of value typing.
