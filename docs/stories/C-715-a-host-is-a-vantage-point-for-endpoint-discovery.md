---
id: C-715
title: "A host is a vantage point for endpoint discovery"
pillar: "Core"
status: backlog
epic: the-substrate-seam
areas: [flux-capabilities, flux-cli]
design: docs/designs/the-substrate-seam.md
note: "EndpointBroker fans discovery across provider plugins, but nothing scopes a query to a substrate — so 'what does my dev cluster see' is not a question the system can be asked"
---

# A host is a vantage point for endpoint discovery

## Goal

Endpoint discovery is already brokered and pluggable: `EndpointBroker` fans an `endpoint.discover`
query across provider plugins, and the kubernetes provider already returns in-cluster Services,
Ingresses and crossplane/RDS-derived database endpoints as weak references whose credential is a
`kubernetes/<ns>/<secret>/<key>` location. What is missing is the axis a host makes available:
**from where**. A query today answers "what can be discovered", implicitly from wherever the
provider happened to run, and the answer is silently different depending on that.

A host binding is exactly a vantage point — a machine with a view of a network. Scoping discovery
to one makes "what does my dev cluster see" a question the system can be asked, and makes the
answer attributable rather than ambient. It also gives the discovered records their locality
(C-709), which is what makes them usable afterwards rather than merely informative.

## Acceptance

- [ ] Discovery accepts a host binding as its vantage: the CLI surface and the operation both take
      one, the query runs with that substrate in force, and the binding's grant is checked before
      anything is dialled.
- [ ] Every returned record is stamped with the host it was discovered through (C-709's field), so
      an imported endpoint carries where it came from rather than becoming a bare URL.
- [ ] Results from different vantages are distinguishable rather than merged: discovering the same
      product from two clusters yields two records, and neither silently overwrites the other on
      import.
- [ ] Discovery without a host behaves exactly as today, and is documented as meaning "from here".
- [ ] A provider that cannot run under the selected substrate refuses with the reason rather than
      falling back to the local vantage and returning an answer about the wrong network.
