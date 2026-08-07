---
id: C-681
title: "fleet promote accumulates, gates and merges every member in dependency order"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli]
note: "decision 0017 point 6; the bash driver's snapshot_and_merge is flux-only and carries one machine's absolute paths"
---

# fleet promote accumulates, gates and merges every member in dependency order

## Goal

Accepted candidates reach a member's canonical branch through a binary verb, for every member, in
the order decision 0005 fixes: connectors, then exchange, then flux.

Today this is `snapshot_and_merge()` in `autopilot.sh`. It is called once, hardcoded for flux, with
this machine's absolute path as its argument — so in any other deployment the release train silently
never runs, and in this one connectors and exchange are simply not promoted. `fleet apply` is not the
same thing: it merges one recorded green candidate, while promotion is the accumulate-tag-gate-merge
step that turns several applied candidates into one gated advance of the canonical branch.

Decision 0017 requires this as a primitive because both of its designs need it and no deployment may
reimplement it. It is one of the two stories Decision 6 names as missing.

The bash version's learned invariants are the contract, not a starting point: the gate runs in a
**throwaway worktree branched from the canonical ref**, never in a working checkout, so a long gate
cannot be disturbed and a red gate leaves the branch untouched; a candidate that conflicts is left
out and reported rather than forced; and nothing pushes, releases or deploys — decision 0016's
boundary is unchanged.

## Acceptance

- [ ] `flux fleet promote` accumulates applied candidates per member and promotes each member whose accumulation threshold is met, in decision 0005's dependency order, with no member id or filesystem path hardcoded.
- [ ] The gate runs in a throwaway worktree branched from the member's canonical ref; a red gate leaves the canonical ref untouched and retains the tag for triage.
- [ ] A candidate that conflicts with the accumulated tree is excluded and reported by name; the remaining candidates still promote.
- [ ] Promotion merges only the member's local canonical ref. No push, release or deployment happens, and a dry run reports the exact merges it would make.
- [ ] The threshold is configuration (a `[drive]`-style table), not an environment variable read by a shell script, and its default is documented where the verb is documented.
- [ ] Exercised across at least two members in one run, proving the ordering rather than asserting it.
