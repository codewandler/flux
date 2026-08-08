---
id: C-681
title: "fleet promote accumulates, gates and merges every member in dependency order"
pillar: "Core"
status: done
epic: fleet-harness-throughput
areas: [flux-cli]
note: "decision 0017 point 6; the bash driver's snapshot_and_merge is flux-only and carries one machine's absolute paths"
---

# fleet promote accumulates, gates and merges every member in dependency order

## Goal

Accepted candidates reach a member's **local** canonical branch through a binary verb, for every
member, in the order decision 0005 fixes: flux, then connectors, then exchange.

*(Amended during implementation. This clause previously read "connectors, then exchange, then flux",
which inverts the decision it cites: 0005 fixes `Flux → flux-connectors → flux-exchange`. AGENTS.md
requires amending the story rather than implementing contradictory prose. The order itself is not
written into Flux at all — it is read from a `depends_on` graph in the deployment's own
`fleet.toml`, per the first acceptance item.)*

Today this is `snapshot_and_merge()` in `autopilot.sh`. It is called once, hardcoded for flux, with
this machine's absolute path as its argument — so in any other deployment the release train silently
never runs, and in this one connectors and exchange are simply not promoted. `fleet apply` is not the
same thing: since C-619 it merges nothing at all — it accepts one recorded green candidate and pins
it with an annotated tag — while promotion is the accumulate-tag-gate-land step that turns several
accepted candidates into one gated advance of the canonical branch.

Decision 0017 requires this as a primitive because both of its designs need it and no deployment may
reimplement it. It is one of the two stories Decision 6 names as missing.

The bash version's learned invariants are the contract, not a starting point: the gate runs in a
**throwaway worktree branched from the canonical ref**, never in a working checkout, so a long gate
cannot be disturbed and a red gate leaves the branch untouched; a candidate that conflicts is left
out and reported rather than forced; and nothing pushes, releases or deploys — decision 0021 §0
keeps publication and release out of scope.

A red gate withholds the **whole train**, not just its own member: the members are ordered by a
dependency graph, so a half-promoted set is a state no later operation can reason about. Refusals
and below-threshold skips are not red gates and withhold nobody.

## Acceptance

- [x] `flux fleet promote` accumulates applied candidates per member and promotes each member whose accumulation threshold is met, in decision 0005's dependency order, with no member id or filesystem path hardcoded.
- [x] The gate runs in a throwaway worktree branched from the member's canonical ref; a red gate leaves the canonical ref untouched and retains the tag for triage.
- [x] A candidate that conflicts with the accumulated tree is excluded and reported by name; the remaining candidates still promote.
- [x] Promotion merges only the member's local canonical ref. No push, release or deployment happens, and a dry run reports the exact merges it would make.
- [x] The threshold is configuration (a `[drive]`-style table), not an environment variable read by a shell script, and its default is documented where the verb is documented.
- [x] Exercised across at least two members in one run, proving the ordering rather than asserting it.

## Progress

`flux fleet promote` is implemented in `crates/flux-cli/src/board_fleet_cmd.rs` (`promote_members`,
with `promotion_order`, `accepted_candidates`, `promotion_target`, `worktrees_on_branch`). Two
end-to-end tests cover it in `crates/flux-cli/tests/board_fleet_cli.rs`.

Decisions worth knowing before changing this:

- **Order is configuration.** `[[repositories]].depends_on` is topologically sorted, stable by
  declaration. No member id or path appears in Flux's source; the roadmap workspace expresses
  decision 0005 in its own `fleet.toml`. The test declares the *downstream* member first and
  alphabetically first, so both orders a naive implementation falls into are wrong.
- **The threshold is `[promote] threshold`**, default `1`, documented in
  `website/docs/coding/fleet.md` and in `fleet skill`.
- **Landing is `git update-ref <branch> <new> <old>`**, not a merge in a checkout. It is atomic,
  refuses if the branch moved while the gate ran, and touches no working tree — which is how the
  gate stays out of shared checkouts. The consequence is that a checkout sitting on that branch
  keeps a stale index; promotion enumerates those checkouts and warns by name that `git commit -am`
  there would revert the landed work. It never writes them.
- **The verdict is re-read from git**, never taken from an exit code: the canonical ref is resolved
  again and each candidate's containment is asked of git. That is also what flips a wave from
  `awaiting-delivery` to `applied`, through the same `wave_delivery_verdicts` helper C-721 uses, so
  promotion cannot make a claim `fleet doctor` would contradict.

Not done here, deliberately: promotion does not run a repository's `prepare` step on the
accumulation. Where a member derives artifacts from a whole candidate, an accumulation of two
candidates can carry a stale mirror — the gate catches it and goes red, which leaves `main`
untouched and the tag retained. Making promotion regenerate and commit derived artifacts would
change the gated tree in a way this story does not sanction; it wants its own story.
