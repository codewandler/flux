---
id: C-745
title: "A story already implemented on a live branch is not re-dispatched"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-orchestrate]
---

# A story already implemented on a live branch is not re-dispatched

## Goal

A branch audit found `C-562` implemented **independently four times** — waves 257, 281, 286 and 308,
carrying 827, 1121, 1129 and 1004 unique lines of `board_fleet_cmd.rs` — and `C-569` twice more on
waves 299 and 302. The fleet re-dispatched stories that were already implemented on a live branch and
discarded the losers: roughly 5,000 lines of model output, produced and thrown away.

C-723 stopped the driver withholding on an unverified `already-built` signal. This is the opposite
error: dispatching work that demonstrably exists, because nothing looks at the branches.

## Acceptance

- [ ] Before dispatching an item, the driver checks whether a live branch already holds an
      implementation of it, and reports what it found either way.
- [ ] The check is evidence-based, not a name match. C-723's lesson applies exactly: a branch whose
      name contains the story id proves nothing, and `wave-745/story/C-575` held a genuine
      implementation while `wave-472` branches held superseded ones.
- [ ] Finding one does not silently withhold. It dispatches with the prior attempt named, or
      withholds with the branch cited — the operator must be able to see which and why.
- [ ] A superseded attempt is distinguishable from a live one, so the check does not resurrect work
      that main has already moved past.
- [ ] Regression test: two waves dispatched for one story produce a report naming the first
      attempt's branch rather than two independent implementations.
- [ ] `C-510` is the live fixture and must be reported, not dispatched, by a dry-run tick. It is
      `ready` with 38 acceptance criteria, and its implementation already exists **twice** — see the
      audit below.

## The standing case: C-510

A second branch audit on 2026-08-08 found the cost is not historical. `C-510` — *install and
supervise a verified local Exchange release* — is `ready` on the board right now, and roughly **5,100
lines implementing it** sit on two branches:

| Branch | Unique commits | What it adds |
|---|---|---|
| `wave/flux-exchange-lifecycle-2` | 7 | `flux-cli/src/exchange_local_cmd.rs`, `flux-system/src/exchange_release_transport.rs` |
| `wave/flux-exchange-lifecycle-1` | 5 | `flux-cli/src/exchange_local/`, `flux-system/src/verified_cache.rs` |

They are **disjoint rival implementations, not a series**: their merge base is `087e93ba`, a docs-only
merge, and `git cherry lifecycle-2 lifecycle-1` reports all five of branch-1's commits as `+`. So the
fleet already paid for this story twice and landed neither.

Main holds the contract and nothing else — `ExchangeLocalAction::{Start,Status,Stop}` in `args.rs`,
`Command::ExchangeLocal*` in `dispatch.rs`, and `docs/designs/managed-exchange-lifecycle.md` — while
`flux exchange local status` answers `refused [unsupported]`. That refusal is honest and deliberate;
the point is that the code which would replace it has been written twice and is on this disk.

Dispatching `C-510` today buys a third implementation. That is what this story has to stop, and it is
why the check must run before dispatch rather than as a report afterwards.
