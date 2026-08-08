---
id: C-716
title: "A live datasource connects from where its endpoint is reachable"
pillar: "Core"
status: backlog
epic: the-substrate-seam
areas: [flux-capabilities]
design: docs/designs/the-substrate-seam.md
note: "LiveDatasource receives ToolContext and must route IO through the guards its LiveAccess declares, but nothing ties the connection to the substrate its endpoint is reachable from"
---

# A live datasource connects from where its endpoint is reachable

## Goal

`LiveDatasource` is the governed read over a system of record: it declares its concrete external
resources as `LiveAccess`, receives a `ToolContext`, and is required to perform real IO through
flux's guarded surfaces rather than a client of its own. Since C-675 that context carries the
session's selected substrate, so the machinery for "connect from there" exists — but nothing
connects the two ends. A datasource whose endpoint is a ClusterIP has no way to say the connection
must be made from inside the cluster, and a backend that builds its own client would not follow a
selection even if it were made.

This is the third leg of the same triangle: a host says *where*, an endpoint says *what service and
as whom* (C-709 adds where it is reachable from), and a datasource is the governed read over it.
Composing them means a datasource read happens on a machine that can actually reach its endpoint,
with the private-network scope and name resolution of that machine rather than the coordinator's.

## Acceptance

- [ ] A datasource bound to a host-local endpoint (C-709) makes its connection through that host's
      substrate, with resolution and the private-network scope of that binding; a datasource whose
      endpoint names a host the session cannot select is refused naming both.
- [ ] `LiveAccess` declares the locality it requires, so the refusal happens at admission rather
      than as a connection error deep inside a backend.
- [ ] Every shipped `LiveDatasource` backend routes its connection through the guarded surface its
      access declares — a backend constructing its own client is a census-style failure, not a code
      review question.
- [ ] A datasource whose endpoint has no host binding behaves exactly as today.
- [ ] The docs state the composition once, plainly: the host is where the connection is made from,
      the endpoint is what is connected to, and the grant is who may.
