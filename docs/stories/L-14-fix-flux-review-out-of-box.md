---
id: L-14
title: Make `flux review` work out of the box (roles missing from the binary; aggregation dies on real reviewer output)
pillar: Language
status: done
epic: strict-review-flows
design: docs/designs/strict-review-flows.md
note: FIXED — the 3 reviewer roles ship in the binary (embedded from the committed .flux/agents files; project files still override), and the flow passes raw reviewer outputs straight to review.aggregate whose normalizer recovers fenced/prose JSON and quarantines junk as gaps; live-verified end-to-end in a foreign repo (full ranked report; provider-error outputs degrade to gaps instead of aborting)
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
- [x] Failing-first: `flux_app::review::builtin_review_roles()` embeds + parses the three
      committed `.flux/agents/review-*.md` (`builtin_review_roles_ship_the_three_reviewers_toolless`
      pins names, `tools: []`, non-empty prompts); `load_roles` seeds them only when absent, so
      project files still override.
- [x] Failing-first: `review.aggregate`/`review.normalize` accept raw reviewer blobs — the
      normalizer flattens nested arrays and leniently recovers JSON from strings (as-is → fence-
      stripped → first `[...]` slice), quarantining unrecoverable blobs as one gap each
      (`review_aggregate_recovers_findings_from_dirty_reviewer_blobs`). The flow's fragile
      `merge` step is GONE — `strict_review.flux` passes the three raw outputs directly to
      `review.aggregate`.
- [x] The dirty-output variant lives at the op level (fenced + prose + junk + nested array in one
      call), where both the CLI and journey paths bottom out — mock-only clean JSON can't
      green-light this again.
- [x] Live in a scratch repo: `flux review --files src/stats.py -m openrouter-anthropic/...` →
      full ranked markdown report from 3 real reviewers (15.4s). Bonus resilience proof: with the
      credit-less anthropic key, all three reviewer errors quarantined as gaps and the report
      still rendered end-to-end (no abort after sub-agent spend).

## Progress
- **DONE (2026-07-02).** `flux-app::review`: `REVIEW_ROLE_SOURCES` (include_str of the committed
  role files, same pattern as `STRICT_REVIEW_FLOW_SRC`) + `builtin_review_roles()`; CLI
  `load_roles` seeds them when absent. `flux-tools::cognition`: `normalize_entry` (object | array
  → flatten | string → `parse_reviewer_blob` lenient recovery, `strip_code_fence`), shared by
  normalize + aggregate. `examples/strict_review.flux`: aggregation tail rewritten — no `merge`,
  raw outputs into `review.aggregate`. Full gate green; live-verified both degradation and happy
  path.

## Notes
- Found during the 2026-07-01 harness e2e review.
- Related: C-10 (structured op outputs) removes the broader "merge over stringly values" trap this
  flow fell into; this story still stands alone because the roles gap is independent and the flow
  must tolerate dirty model output regardless of value typing.
