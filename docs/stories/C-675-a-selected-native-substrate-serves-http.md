---
id: C-675
title: "A selected native substrate serves HTTP"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-system, flux-web]
design: first-class-hosts
note: "C-651/C-652 interplay: a sandboxed selection answers Unserved for web effects — fail-closed, and a capability gap both implementors flagged"
---

# A selected native substrate serves HTTP

## Goal

Under a selection that resolves to a native-composed substrate — the sandboxed peer today, the
container backend next — `http.request` and `web.fetch` answer the port's `Unserved`, because a
bare `System` serves no HTTP and the one native implementation (`flux_web::NativeHttp`) lives a
layer above the substrate. That refusal is honest and fail-closed, and it is also a gap: selecting
confinement should not cost web effects. Give selected native substrates an HTTP backend without a
second client, without an ambient seam, and without the routing branch learning to sniff kinds.

## Acceptance

- [ ] A sandboxed selection serves `http.request`/`web.fetch` through the one reviewed egress
      client with its own audit sink; the codegate `Http` census still counts exactly the existing
      client construction points.
- [ ] The selection branch stays kind-blind and nothing can fall back to a local send while a
      selection is in force; the placement census is unchanged.
- [ ] A spawned sub-agent's context carries the parent's selected substrate, pinned by the test
      C-652's review named as the open question.
- [ ] `SandboxedSystem`'s `GuardedHttp` census entry moves from empty to its new truth with a
      review note stating which call is made and why it adds no IO path.
