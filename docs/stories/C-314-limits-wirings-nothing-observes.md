---
id: C-314
title: "Two `[limits]` wirings nothing observes, and an occupancy test that guards less than its prose"
pillar: Core
status: ready
priority: 10
areas: [flux-cli]
note: "filed from C-307's review, not from planning — `flux review` and `flux record` newly honour [limits], but deleting BOTH wirings leaves the entire flux-cli suite green; this is the exact rot C-299 and C-307 exist to close, newly planted on two more call sites"
---

# Two `[limits]` wirings nothing observes

## Goal

C-307 wired `[limits]` into every shipped surface that assembles a runtime without ceilings. Its
headline surfaces — `flux app run`'s executor and its review sub-agents — are both pinned by tests
that go red when the wiring is deleted. Two of the surfaces it wired along the way are not.

C-307's reviewer deleted `.resource_limits(resource_limits)` at `crates/flux-cli/src/review.rs:185`
**and** `.resource_limits(cli_resource_limits(&cfg))` at `crates/flux-cli/src/lab_cmd.rs:52`
simultaneously, and the whole `flux-cli` suite stayed green — `268 passed`, plus all 16 integration
binaries. These are real behavioural changes on shipped surfaces: `flux review` and `flux record`
now honour the operator's ceilings, and nothing would notice if they stopped.

This did not fail C-307, whose Acceptance only asked that each surface be wired or its exemption
recorded. But it is precisely the defect class the limits stories exist to close — C-299 shipped
with a ticked box no test observed, and it was caught only by mutating the wiring line — and it has
now been planted on two more call sites. The pattern is worth closing where it starts rather than
rediscovering it in the next review.

A second, smaller gap: `crates/flux-cli/src/app_cmd.rs:947` passes
`bundle.resource_limits.independent_copy()` into the child environment, and that `independent_copy()`
call is **test code**. So the `in_flight == 2` assertion at `app_cmd.rs:962` cannot detect a
regression where `LocalSpawner::spawn` (`crates/flux-orchestrate/src/lib.rs:440`) switches from
`independent_copy()` to `clone()` — the very shape that produced a real deadlock during C-299. The
test's doc comment claims it applies "verbatim the transformation `LocalSpawner::spawn` applies",
which is true today, but a comment is not a binding. The invariant itself is genuinely held by
C-299's `a_delegated_child_is_bounded_but_never_starved_by_its_parent`
(`crates/flux-sdk/tests/resource_limits.rs:873`), so nothing is unguarded — the newer test simply
guards less than its prose implies.

## Acceptance

- [ ] **Failing-first**: a test that reds when `review.rs:185`'s wiring is deleted, and a test that
      reds when `lab_cmd.rs:52`'s is. Prove each by making exactly that deletion and showing the test
      name in the failure output. One test observing both is not acceptable — they must be
      independently attributable, the way C-307's two halves are.
- [ ] `app_cmd.rs`'s per-child assertion observes the transformation `LocalSpawner::spawn` actually
      applies, rather than one the test performs itself — or, if that is not reachable at this level,
      the doc comment is corrected to say what the test really guards and to point at
      `resource_limits.rs:873` as the binding check.
- [ ] Either a journey-level test pins `run_app`'s end-to-end chain (`run_app` →
      `App::try_with_execution_environment` → `build_executor` → `into_executor`), or the story
      records why that fixture is not worth its cost. C-307's reviewer traced the chain by reading
      and found it holds; no test pins it.
- [ ] Full gate green in both workspaces.

## Notes

- Filed from the C-307 review (2026-07-31). The reviewer's verdict was PASS; none of this blocked it.
- Related: [C-307](C-307-app-run-ignores-limits.md) wired the surfaces;
  [C-299](C-299-cli-resource-ceiling-wiring.md) established the per-child rule and the deadlock the
  shared-ceiling shape produces.
- The general lesson this story is an instance of: a wiring line with no test is invisible to the
  gate, and mutation is the only cheap way to find one. Consider whether anything more systematic
  than "a reviewer remembered to probe" is available here.
