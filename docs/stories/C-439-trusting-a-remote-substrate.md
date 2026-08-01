---
id: C-439
title: "Trusting a remote substrate — an unauthenticated endpoint, and a remote that lies about what it did"
pillar: Core
status: ready
priority: 7
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [flux-system, flux-auth, flux-evidence]
note: "⚠ two failures, and the second is the interesting one. A remote executes your effects and REPORTS what happened — the evidence chain is flux's core guarantee, and a remote link is exactly where it can quietly stop meaning anything"
---

# The remote reports; flux records what it was told

## Goal

Make the trust flux places in a remote substrate explicit, authenticated, and bounded — and decide what
the evidence chain means when the executing party is not flux.

## The two failures

**1. An unauthenticated or hijacked endpoint.** A remote substrate runs whatever flux dispatches. An
endpoint anyone can reach, or one flux does not authenticate, is arbitrary execution with a friendly
name. ⚠ flux already learned the shape of this locally: `flux-server` *"will not let it reach a
non-loopback bind"* without authentication, and C-409 found the adapters that bound their own listeners
inheriting none of that. A remote link is the same class from the client side.

**2. ⚠ A remote that lies.** The more interesting one. flux records what happened from what the executor
*reports*. A remote that claims success it did not achieve, or omits an effect it did produce, corrupts
the evidence chain — and auditability is one of flux's headline guarantees, not a convenience. Locally
the executor and the recorder are the same process, so the question never arises. Remotely they are not.

## Acceptance

- [ ] The remote endpoint is authenticated, and an unauthenticated one is **refused rather than
      warned about**. ⚠ Follow `flux-server`'s existing posture rather than inventing a second one — it
      already refuses a non-loopback bind without auth, and the client side should mirror that shape.
- [ ] The transport is guarded: the endpoint routes through `guard_url_scoped` in its `http`/`https`
      form, as D-205 does for `wss://`. A remote address is still an egress destination.
- [ ] ⚠ **The evidence chain states its provenance.** Where a record comes from a remote's report rather
      than from flux's own execution, the record says so. A log that cannot distinguish "flux did this"
      from "a remote told flux it did this" has lost the property that makes it evidence.
- [ ] Unreachable · refused · reported-failure are three distinguishable outcomes — C-399's acceptance,
      and the operator response differs for each.
- [ ] What flux does **not** verify is stated plainly. ⚠ flux cannot independently confirm a remote
      executed what it claims; pretending otherwise would be worse than the gap. Say it, in the docs
      that [C-440](C-440-the-topologies-page.md) produces.
- [ ] Full gate green.

## Notes

- Settleable ahead of [C-436](C-436-flux-tui-remote.md).
- ⚠ Adjacent, and worth checking here: does a remote substrate see secrets? If a dispatched op carries a
  credential, the remote holds it during execution — which interacts directly with
  [C-437](C-437-which-guarantees-travel.md)'s secret-handling row and may be the sharpest thing on that
  table.
- The `ssh` comparison is useful again: `ssh` solves authentication with a well-understood key model and
  a host-key check people already reason about. Whatever is built here should be at least as legible.

## Progress

- Filed 2026-08-01 with the remote-agents epic.
