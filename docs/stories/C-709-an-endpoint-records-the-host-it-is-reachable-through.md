---
id: C-709
title: "An endpoint records the host it is reachable through"
pillar: "Core"
status: backlog
priority: 3
epic: the-substrate-seam
areas: [flux-secret, flux-capabilities]
design: docs/designs/the-substrate-seam.md
note: "EndpointRef carries url, product, credential_ref and labels — and nothing about where it is reachable from, so a ClusterIP endpoint looks identical to a public one"
---

# An endpoint records the host it is reachable through

## Goal

`EndpointRef` records `id`, `url`, `product`, `protocol`, `source`, `credential_ref` and `labels`.
What it does not record is **locality** — from where the endpoint is reachable at all.
`postgres://db.default.svc.cluster.local:5432` is meaningless on a laptop and exactly right inside
the cluster, and nothing in the record distinguishes it from `postgres://db.example.com:5432`.

That omission costs three things, each of which is otherwise already solved:

- the URL guard resolves the name wherever the guard runs, which is the coordinator, so a
  cluster-internal name either fails to resolve or resolves to something else (C-689 moves
  resolution onto the substrate; this story is what tells it *which* substrate);
- reaching the endpoint's private address needs a private-network grant, which today is
  caller-wide rather than scoped to the binding that reaches it (C-694);
- a discovered endpoint loses the fact that it was found *through* a particular cluster, so
  importing one produces a record no more useful than a hand-typed URL.

Give the record the host it belongs to — declared in configuration, or stamped at discovery by
whatever found it — so a destination that only exists somewhere says so.

## Acceptance

- [ ] `EndpointRef` gains an optional host binding reference; `[[endpoint.static]]` may declare it,
      `flux endpoint list/show/resolve` render it, and a reference to an undeclared binding is a
      load-time error rather than a dial-time surprise.
- [ ] `flux endpoint resolve` reports the host an endpoint would be reached through alongside the
      credential *location* it already reports — the operator diagnostic answers "from where" as
      well as "as whom".
- [ ] Dialing a host-bound endpoint resolves its name and applies its private-network scope through
      that binding rather than the caller's ambient position; an endpoint bound to a host the
      session did not select is refused with both named, never silently dialled from here.
- [ ] An endpoint with no host binding behaves exactly as today — this adds locality where it is
      known, and does not require it where it is not.
- [ ] Discovery stamps the host it discovered through (composes with C-715), and `import` preserves
      it, so the discover → import → use loop keeps the fact rather than dropping it.
