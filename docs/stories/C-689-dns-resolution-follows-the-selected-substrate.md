---
id: C-689
title: "DNS resolution follows the selected substrate"
pillar: "Core"
status: ready
priority: 3
epic: the-substrate-seam
areas: [flux-system]
design: docs/designs/the-substrate-seam.md
note: "HostResolver exists as a test seam inside net.rs but never reaches the port, so the coordinator resolves names the substrate will dial — a cluster name resolves to the wrong answer or none"
---

# DNS resolution follows the selected substrate

## Goal

`flux-system/src/net.rs` defines a `HostResolver` trait with `SystemHostResolver` behind it, and
the URL guard takes it as a parameter — but it is a *test* seam: it appears nowhere on
`ExecutionSystem`, so resolution always happens wherever the guard runs, which is the coordinator.
Under a selected substrate that is the wrong machine. A Kubernetes service name resolves only
inside the cluster, so `--host <pod>` plus a dial of `svc.cluster.local` either fails to resolve
locally and is refused for a destination the substrate could reach, or — worse — resolves to a
different address locally, and the SSRF guard then makes its private-versus-public judgment, and
its address pin, against a view of the name the substrate would never have produced.

The shape is the one the codebase already uses for handoffs: **fact remote, policy local.**
Resolution becomes a guarded operation answered by the selected substrate; the private/loopback/
link-local/ULA/CGNAT judgment still runs on the coordinator over that answer.

## Acceptance

- [ ] Resolution joins the guarded port with a fail-closed default and a reviewed census entry;
      the native backend answers with the local resolver exactly as today.
- [ ] `guard_url_scoped` and `guard_url_scoped_pinned` resolve through the selected substrate when
      one is in force, and the guard's address judgment runs unchanged on the returned addresses —
      no policy moves to the far side.
- [ ] `dial`, `bind` and the HTTP path agree on which machine resolved the name; a test proves a
      name that resolves differently on each side is judged and pinned against the substrate's
      answer, not the coordinator's.
- [ ] A substrate that cannot resolve answers a typed refusal naming the gap; nothing falls back to
      the coordinator's resolver silently.
