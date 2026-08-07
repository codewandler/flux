---
id: C-694
title: "A host binding declares its own private-network scope"
pillar: "Core"
status: backlog
priority: 3
epic: first-class-hosts
areas: [flux-cli, flux-system]
design: docs/designs/the-substrate-seam.md
note: "reaching a ClusterIP needs PrivateNetAllow, but the allowance is caller-shaped today — granting it to reach cluster services also opens the operator's own LAN"
---

# A host binding declares its own private-network scope

## Goal

Every interesting service inside a cluster answers on a private address: a ClusterIP is RFC1918, and
the SSRF guard refuses private, loopback, link-local, ULA and CGNAT destinations by default. The
mechanism to permit them exists — `PrivateNetAllow::Hosts(patterns)` scopes the exemption to named
hosts rather than opening everything — but it is granted per *caller*, not per *binding*. So an
operator who wants their agent to reach `backend.default.svc.cluster.local` **through a Kubernetes
host** must grant private-network access to the session, which also permits reaching their own
router, their NAS and every other machine on their LAN from the native substrate.

The allowance belongs on the binding: a destination pattern that is meaningful only inside one
cluster should be exercisable only through the binding that reaches that cluster.

## Acceptance

- [ ] A `[[host]]` entry declares an optional private-network scope as host patterns; the effective
      allowance for an effect is the binding's scope when that binding is selected, never the union
      of binding and caller scopes.
- [ ] With no binding-declared scope the behaviour is unchanged: the caller-level allowance applies
      to the native substrate exactly as today, and the default stays full refusal.
- [ ] A pattern granted to one binding cannot be exercised through another binding or through the
      native path; a test proves the cross-binding refusal.
- [ ] The audit record names which binding's scope admitted a private destination, so a reviewer can
      tell a cluster-internal call from a LAN call after the fact.
