---
id: C-248
title: "`ops-reference.md` documents none of the eval op family, so a rename there has one unguarded reference"
pillar: Core
status: done
priority: 7
areas: [flux-flow, flux-eval]
note: "found while renaming git_revert→git_reset for C-238: only the website file is coverage-tested, so the in-repo op reference can silently rot for any eval op"
---

# `ops-reference.md` documents none of the eval op family, so a rename there has one unguarded reference

## Goal
flux has two op references: `crates/flux-flow/docs/ops-reference.md` (in-repo, for agents) and
`website/docs/language/ops.md` (published). The catalog-coherence tests
(`operations_reference_covers_the_registered_public_catalog`,
`the_published_risk_column_matches_the_registry`) guard the **website** file against the registry.

`ops-reference.md` documents **none** of the eval-pack ops — `eval_run`, `guard_protected`,
`git_snapshot`, `git_tag`, `gate_check`, and now `git_reset`. Because they are absent rather than
wrong, no coverage test notices. So for any eval op there is exactly one reference that can drift
silently, and the drift is invisible until an agent reads the in-repo file and finds nothing.

This surfaced during C-238's `git_revert` → `git_reset` rename: the rename had to be applied by hand
across both references, and only one of them would have failed the gate if it had been missed.

## Acceptance
- [x] Decide and record which of two shapes is right, then implement it:
      (a) `ops-reference.md` covers the eval family too, and a coverage test pins it the way the
      website file is pinned; or (b) `ops-reference.md` is explicitly and *testably* scoped to the
      builtin catalog, with the eval pack's exclusion asserted rather than incidental.
      → **(a)**, with the decision and the rejection of (b) recorded on the test's doc comment
      (`crates/flux-cli/src/catalog_coherence.rs`,
      `the_in_repo_reference_covers_the_whole_production_catalog`).
- [x] **Failing-first test**: whichever shape is chosen, a test fails today. For (a) that is an
      eval op missing from `ops-reference.md`; for (b) it is the absence of any assertion that the
      exclusion is deliberate.
      → the new test names all 33 undocumented production ops at the merge base.
- [x] No reference can be silently incomplete afterwards: adding an op to either the builtin or the
      eval pack without updating the references it belongs in must redden the gate.
      → in-repo: the new coverage test walks the *production census* (guarded against pack drift by
      `every_registration_seam_in_the_cli_assembly_is_classified`); website: the pre-existing
      `operations_reference_covers_the_registered_public_catalog`.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — filed from the C-238 implementation, which had to rename an eval op across both
  references and observed that only the website side was guarded.
- 2026-07-30 — **implemented, shape (a).** The guard was not blind by accident: C-233's
  `the_published_risk_column_matches_the_production_catalog` walks the *reference* and holds every
  row to the catalog, so it can only ever see ops that are already written down. Absence is
  structurally invisible to it. The new
  `the_in_repo_reference_covers_the_whole_production_catalog` closes that by walking the *catalog*
  and requiring a table **row** (not a prose mention) per op, over the same widest
  `production_catalog()` census — so the two directions together make the file total.
  Running it at the base named **33** undocumented ops, not the 6 the story itemised: the whole eval
  family (19, incl. `grade`), the four datasource ops (`get`/`list`/`relation`/`batch_get`), the five
  `endpoint.*` ops, `review.normalize`/`review.aggregate`, `schedule_wakeup`, `home_dir` and
  `flux_reload`. All 33 are now rows. Two reasoned exclusions carry their reason and must each be
  exercised: `census_board.*` (a board's ops are named by the *program's* datasource) and
  `census_stage` (operator-named config stages).
  Putting the eval rows in a table **with a Risk column** immediately fed them to C-233's guard,
  which caught `grade` documented `Low` against a declared `Medium` — a tier error that had been
  unreachable while `grade` sat only in the risk-column-less agent-loop table.
  Also corrected four wrong *parameter names* in the website file's eval tables
  (`improvements_aggregate` was documented `painpoints, findings` but takes `mined, reviewed`;
  `git_tag`'s required `name` was missing entirely; `improve_log`'s `record`; `change_implement`'s
  `limit`) — coverage tests check that a name is present, never that its signature is real.

## Notes
- The eval pack registers via `flux_eval::try_register_eval_ops`
  (`crates/flux-eval/src/lib.rs:76`), wired into production at
  `crates/flux-cli/src/execution.rs`. So these are **public** ops in a running flux, not test
  scaffolding — which is the argument for shape (a).
- Counter-argument for (b), worth weighing rather than dismissing: `ops-reference.md` is the agent's
  working catalog, and the eval ops are only registered when the eval pack is present. A reference
  that lists ops the current binary may not have is its own kind of wrong. Whichever way it goes,
  the point of the story is that the choice becomes *testable* instead of accidental.
