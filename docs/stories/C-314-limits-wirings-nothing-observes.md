---
id: C-314
title: "Two `[limits]` wirings nothing observes, and an occupancy test that guards less than its prose"
pillar: Core
epic: road-to-stable
status: in-progress
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

- [x] **Failing-first**: a test that reds when `review.rs:185`'s wiring is deleted, and a test that
      reds when `lab_cmd.rs:52`'s is. Prove each by making exactly that deletion and showing the test
      name in the failure output. One test observing both is not acceptable — they must be
      independently attributable, the way C-307's two halves are.
- [x] `app_cmd.rs`'s per-child assertion observes the transformation `LocalSpawner::spawn` actually
      applies, rather than one the test performs itself — or, if that is not reachable at this level,
      the doc comment is corrected to say what the test really guards and to point at
      `resource_limits.rs:873` as the binding check.
- [x] Either a journey-level test pins `run_app`'s end-to-end chain (`run_app` →
      `App::try_with_execution_environment` → `build_executor` → `into_executor`), or the story
      records why that fixture is not worth its cost. C-307's reviewer traced the chain by reading
      and found it holds; no test pins it.
- [x] Full gate green in both workspaces.

## Notes

- Filed from the C-307 review (2026-07-31). The reviewer's verdict was PASS; none of this blocked it.
- Related: [C-307](C-307-app-run-ignores-limits.md) wired the surfaces;
  [C-299](C-299-cli-resource-ceiling-wiring.md) established the per-child rule and the deadlock the
  shared-ceiling shape produces.
- The general lesson this story is an instance of: a wiring line with no test is invisible to the
  gate, and mutation is the only cheap way to find one. Consider whether anything more systematic
  than "a reviewer remembered to probe" is available here.

## Progress

**Item 1 is closed by [C-328](C-328-pin-census-wiring-declares-its-test.md); items 2 and 3 are not.**
This story stays `ready` for that remainder rather than being closed on a partial.

C-328 needed two independently attributable tests as its own failing-first proof, so it built them
and closed this story's first item on the way:

- `review_flow_client_bounds_tool_calls_at_the_configured_ceiling` — C-299's observed-occupancy
  idiom (three `parallel` branches, one op inside `Tool::execute`, `Meter`/`Blocker` imported rather
  than copied).
- `record_client_carries_the_configured_ceiling_to_its_executor` — reads
  `client.engine().executor.resource_limits()`, deliberately *past* `Client::resource_limits()`,
  which is an inert self-report. It stops one layer earlier than its sibling because `flux_sdk::Client`
  has no post-build op registration, so no blocking probe can be installed in its registry; that an
  executor carrying those numbers actually bounds occupancy is already pinned by
  `a_configured_limits_table_binds_for_the_cli_executor`.

Independence verified in both directions by the reviewer, not only by the implementor: deleting one
wiring line reds its own test and leaves the other **ok**.

**Both call sites had to be extracted into named seams** (`record_client_from`, `review_flow_client`)
because neither was reachable from any test — `record_client` resolves a cwd, loads config and
resolves a *live* provider before reaching the builder, and `run_review` ends in `println!` +
`process::exit`. That unreachability is *how this story happened*, and it is worth remembering when
items 2 and 3 are picked up.

**Items 2 and 3 are now closed too.** No production line changed for either — item 2 is a corrected
doc comment and assertion message, item 3 is a new test.

### Item 2 — the real observation is not reachable at this level (the sanctioned second route)

`app_run_strict_review_reviewers_inherit_the_configured_ceiling` keeps its `independent_copy()` call,
because the alternative was tried and does not exist. What changed is that the test no longer claims
otherwise: its doc comment now says it binds `build_review_sub_agents`'s
`.with_resource_limits(resource_limits)` **and nothing on the spawn side**, and names
`a_delegated_child_is_bounded_but_never_starved_by_its_parent`
(`crates/flux-sdk/tests/resource_limits.rs:873`) as the binding check for the rest. Both halves were
verified by mutation rather than asserted:

- delete `.with_resource_limits(..)` from `build_review_sub_agents` → this test reds with `saw 4`;
- switch `LocalSpawner::spawn` (`crates/flux-orchestrate/src/lib.rs:440`) from `independent_copy()`
  to `clone()` → this test stays **ok**, and `resource_limits.rs:873` reds with `runs == 0`.

Why the real observation is unreachable, in order of what was tried:

1. The shipped bundle's reviewer roles declare `tools: []`
   (`builtin_review_roles_ship_the_three_reviewers_toolless`), so nothing can execute inside a real
   reviewer child. Spawning one observable means replacing the bundle's `roles`, `child_base` **and**
   `provider_factory` — everything except `resource_limits`.
2. Even then, occupancy cannot see a child's ceiling at all: `execute_batch`
   (`crates/flux-flow/src/loop_host.rs:859`) walks a child's actions strictly sequentially, so a
   child's in-flight count is 1 bounded or not. This is C-299's recorded negative result, re-checked
   against the current code; C-299 also ruled out the op cache (children get
   `PermissionManager::new()`, so every child op is approval-sensitive and uncacheable).
3. The one discriminator left for `independent_copy()`-vs-`clone()` is starvation — an ancestor
   holding the permit while the child asks for one. That *is* constructible in `flux-cli`, and it was
   rejected on attribution, not difficulty: it would red only for a `flux-orchestrate` regression and
   stay green for every `flux-cli` one, i.e. a copy of `resource_limits.rs:873` filed in a crate that
   owns none of the code under test.

### Item 3 — the journey chain is now pinned

`a_configured_limits_table_binds_for_an_app_journey_executor` (`crates/flux-cli/src/app_cmd.rs`)
parses a one-journey program whose body is a three-branch `parallel` block over C-299's parked probe,
assembles the environment through `assemble_app_execution_environment`, builds a real
`flux_app::App` with `try_with_execution_environment`, and drives it with `App::deliver`. It asserts
occupancy 1, so it observes the ceiling arriving at the executor a **journey** actually runs on:
`assemble_app_execution_environment` → `App::try_with_execution_environment` → `Engine::new`'s shared
`execution` template → `build_executor` → `into_executor`.

It was mutation-tested **in the middle**, not at the endpoint, because both sibling tests already
call `.into_executor()` themselves and so cover only the first hop:

| mutation (shipped line) | new test | the two sibling tests | rest of `flux-app` |
|---|---|---|---|
| `Engine::new` inherits `environment.clone()` with the ceilings stripped | **FAILED, 3 in flight** | ok | 74 passed |
| `build_executor` derives its journey executor with the ceilings stripped | **FAILED, 3 in flight** | ok | — |

The `3` in both failures is also the proof the fixture is not vacuous: the `parallel` block really
does put three calls in flight when nothing bounds them.

**One hop short of the story's wording, deliberately.** The test enters at
`assemble_app_execution_environment`, not at `run_app` — `run_app` resolves a program path, opens an
event store and ends in `flux_channels::serve`, so no test can reach it, the same unreachability
C-328 had to extract seams for. The hop it cannot see is `run_app`'s own
`let resource_limits = cli_resource_limits(&cfg)` being replaced by an unbounded default. That is
structurally narrowed rather than pinned: `assemble_app_execution_environment` takes
`resource_limits` as a required parameter, so a caller cannot arrive unbounded by omission — only by
deliberately passing `ResourceLimits::new()`. Extracting a `run_app` seam wide enough to test was
judged not worth its cost against that.
