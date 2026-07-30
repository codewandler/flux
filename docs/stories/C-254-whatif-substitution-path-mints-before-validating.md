---
id: C-254
title: "`WhatIf::run`'s pure-substitution path mints before its own refusals, so \"no trace\" is still not a property of minting"
pillar: Core
status: in-progress
priority: 12
epic: typed-session-log
design: docs/designs/typed-session-log.md
areas: [flux-sdk, flux-flow]
note: "the third and last site of C-211's invariant — found by C-247's review, deliberately not widened into its diff"
---

# `WhatIf::run`'s pure-substitution path mints before its own refusals

## Goal
C-211 hoisted validation above the mint at both **fork** sites; C-247 did the same for `WhatIf::run`'s
**re-plan** path and its two bails. The pure-substitution (`!replan`) path was outside both stories'
enumerated Acceptance and still mints first, so C-211's invariant — **a failed operation leaves no
trace** — remains a property of *particular paths* rather than of session minting, which is how it
was framed in C-247's Goal.

Three refusals still fire after the child exists:

1. `build_frozen`'s `substitute_at({node}, _) targets a node with no recorded dispatch`
   (`crates/flux-sdk/src/whatif.rs:293-299`, called at `:465`) — after the mint at
   `crates/flux-sdk/src/whatif.rs:462`.
2. `rerun_pinned`'s `session {src} has no executed plan to rerun in turn {t}`
   (`crates/flux-flow/src/whatif.rs:377-382`).
3. The same `build_frozen` refusal on the **re-plan** path, which sits after C-247's mint
   (`whatif.rs:532` then `:548`) — C-247 hoisted the two refusals its Acceptance named, and this
   third one was not among them.

All three are pre-existing at `a0ad8219` and none is a regression from C-247.

## Acceptance
- [x] **Failing-first test**: a substitution refused because the target node has no recorded dispatch
      leaves **no** child session behind — assert the session count is unchanged across the refusal,
      and `panic!` on the `Ok` arm. It fails today.
      → `substitute_at_a_dead_node_refuses_before_minting_the_child`
      (`crates/flux-sdk/tests/whatif.rs:882`)
- [x] A second failing-first case for `rerun_pinned`'s "no executed plan to rerun" refusal.
      → `a_turn_with_no_executed_plan_refuses_before_minting_the_child`
      (`crates/flux-sdk/tests/whatif.rs:933`)
- [x] A third for the `build_frozen` refusal on the re-plan path, which C-247 left after its mint.
      → `replan_with_a_dead_node_substitution_refuses_before_minting_the_child`
      (`crates/flux-sdk/tests/whatif.rs:990`)
- [x] Every refusal above is hoisted above the mint, so no path in `WhatIf::run` creates before it has
      finished validating. → `mint_gate` (`crates/flux-sdk/src/whatif.rs:266`) owns the only mint;
      `RerunSelection` (`crates/flux-flow/src/whatif.rs:364`) owns `rerun_pinned`'s refusals.
- [x] The tests observe the *absence of a trace* (session count unchanged), not merely `is_err()` — an
      `is_err()`-only assertion passes with the child still minted, and is the specific way this class
      of fix gets faked. → all three assert `session_count(&record) == before`; all three failed at the
      merge base on exactly that assertion (`left: 2, right: 1`), not on the returned error.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — filed from C-247's independent review, which confirmed C-247's own Acceptance was fully
  met and flagged this as the remaining scope rather than as rework. Recorded instead of widened, which
  is the same discipline that produced C-247 out of C-211 — and the reason all three sites are now
  written down rather than rediscovered a third time.
- 2026-07-30 — **implemented** on `impl/C-254` (merge-base `389f1c95`). Fixed *structurally* rather than
  by hoisting statements, because a moved call is correct only until the next reordering of the
  function — which is precisely how this defect reached a third story.
  - **`flux-flow`**: refusal 2 lived one crate down, *inside* the driver the SDK hands `dst` to, so it
    could not fire before the mint at all until the crate grew a seam. New `whatif::RerunSelection`
    carries the resolved execution selection and *is* `rerun_pinned`'s whole refusal surface (empty
    trace / missing turn / no executed plan); `rerun_pinned` now takes a `&RerunSelection` in place of
    its `src` + `turn` pair, so the driver cannot be reached — nor its `dst` writes opened — with a
    refusal outstanding. What stays fallible inside it is *execution*, not refusal.
  - **`flux-sdk`**: new private `mint_gate` module. `create_session_with_context` is private to it, and
    the only route to a `dst` is `ClearedSubstitution::mint`/`ClearedReplan::mint` — methods on values
    whose fallible `resolve` constructors discharge every refusal on their path first. `build_frozen`
    split into `ResolvedSubstitutions::resolve` (fallible, pre-mint) and `::freeze` (**infallible**,
    post-mint, because `OffTape::Live`'s bridge needs a `dst` that cannot exist earlier).
  - The wrong order does not compile — verified by reordering the mint above the resolve:
    `error[E0425]: cannot find value 'cleared' in this scope`. The residual gap types cannot close is
    `run` calling `variant.events.create_session_with_context` directly; that is documented on
    `mint_gate` and the three tests fail the moment it reappears.
  - Also added `rerun_selection_refuses_without_a_destination_session`
    (`crates/flux-flow/src/whatif.rs:791`) — asserts the flux-flow refusals resolve with no `dst`
    argument at all and create nothing, which is what makes them destination-free rather than merely
    early.
  - ⚠ **Surface impact**: `flux-sdk`'s own public API is unchanged (`flux_flow::whatif` is not
    re-exported from it), but `codewandler-flux-flow::whatif::rerun_pinned`'s signature changed, and
    `RerunSelection` is new public API on that crate. That is a **breaking change to a published
    crate** — left as a version decision for integration, not taken here. In-tree callers are the SDK
    and this module's own tests; there are no others.
- 2026-07-30 — **post-review fixes**, after the story passed independent review.
  - Dropped `RerunSelection::is_empty`, which returned the constant `false`, in favour of
    `#[allow(clippy::len_without_is_empty)]` on the impl. It existed only to satisfy the lint that
    pairs with `len`, but on a published crate a receiver-ignoring `is_empty` reads to an external
    caller as a test they can rely on. A resolved selection is non-empty *by construction* — `resolve`
    refuses an empty one rather than returning it — so that invariant is now stated where the type is
    defined instead of hidden inside a method pretending to check it.
  - ⚠ **The breaking change is invisible to CI, and this note is the only thing standing between it
    and a patch release.** `rerun_pinned`'s parameter list lost `src: &str` and `turn: Option<usize>`
    and gained `selection: &RerunSelection` (`crates/flux-flow/src/whatif.rs:435-443`). Removing
    parameters from a `pub async fn` breaks every out-of-tree caller at compile time — this is
    **breaking, not additive**, and under the project's 0.y rule (the minor position is the breaking
    signal) it obliges a **MINOR** at the next cut.
    `scripts/check-crate-versions.sh` reports PASS and always will: by its own design it compares
    *version strings* for crates that set their own version, and deliberately **skips crates that
    inherit `version.workspace = true`** — `cut-release.sh` sweeps those, so "changed but not bumped"
    is their normal state on main and flagging it would be noise. `codewandler-flux-flow` is one of
    those inheriting crates, so it is out of the script's scope entirely; and the script has no notion
    of API *shape* in any case, only of version movement. Nothing mechanical in the gate can catch
    this. Whoever cuts the next release must read the bump off this note, not off a green CI run.

## Notes
- Read C-211's and C-247's diffs first; this is the same move applied to the last three sites.
  C-247's shape is the model: validate, mint, then one checked rewrite.
- Cheaper to hold as an invariant than to re-derive per path. Note the orphan is *self-cleaning* today
  (`prune_empty_excluding` gates on `last_seq <= 0`), so this is invariant hygiene rather than a leak —
  but C-247's review showed the turn-path orphan escaped `prune_empty` because it carried a full
  conversation, so "self-cleaning" does not generalise and should not be assumed for these three.
- `flux-sdk` is published; a refused operation simply stops creating a session, so the behaviour change
  is patch-compatible. It rides `version.workspace = true`, so `scripts/check-crate-versions.sh` owes
  nothing — state the surface impact anyway.
