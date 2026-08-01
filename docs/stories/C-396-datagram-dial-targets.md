---
id: C-396
title: "UDP and ICMP dial targets"
pillar: Core
status: ready
priority: 8
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "raw ICMP needs CAP_NET_RAW — an unheld capability must refuse at construction, because a check that happens on the wire has already leaked the attempt"
---

# UDP and ICMP dial targets

## Goal

`net::DialTarget` covers TCP. Reachability checks and protocol probes need datagram and raw sockets,
under the same egress guard — resolved IPs, private/loopback/link-local/ULA/CGNAT blocked unless a
scoped grant says otherwise.

## Acceptance

- [ ] `DialTarget` gains UDP and ICMP variants, guarded by the same resolution and range checks as
      TCP. No second guard is introduced.
- [ ] **Failing-first test** — a UDP target resolving to a private address is refused without a
      scoped private-net grant, and admitted with one.
- [ ] **Failing-first test** — a raw ICMP target is refused **at construction** when the process
      lacks the capability to open a raw socket, with an error naming the capability. Refusing at
      first send is not acceptable: the destination has already been contacted.
- [ ] Pinning holds: the address a guard approved is the address dialled (the `dial_scoped_pinned`
      property, extended to the new variants).

## Progress
- (not started)

## Notes
- `crates/flux-system/src/net.rs` — `DialTarget`, `DialStream`, `dial_scoped`, `dial_scoped_pinned`,
  `destination_is_private`.
- Platform reality to state in the design, not discover: Linux raw ICMP needs `CAP_NET_RAW` or
  `ping_group_range`; macOS differs. A capability the process may not hold is a deployment fact, so
  the refusal must name it rather than say "permission denied".
