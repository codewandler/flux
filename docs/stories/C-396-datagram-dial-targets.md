---
id: C-396
title: "UDP and ICMP dial targets"
pillar: Core
status: in-progress
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

- [x] `DialTarget` gains UDP and ICMP variants, guarded by the same resolution and range checks as
      TCP. No second guard is introduced.
- [x] **Failing-first test** — a UDP target resolving to a private address is refused without a
      scoped private-net grant, and admitted with one.
- [x] **Failing-first test** — a raw ICMP target is refused **at construction** when the process
      lacks the capability to open a raw socket, with an error naming the capability. Refusing at
      first send is not acceptable: the destination has already been contacted.
- [x] Pinning holds: the address a guard approved is the address dialled (the `dial_scoped_pinned`
      property, extended to the new variants).

## Progress

Landed in `crates/flux-system/src/net.rs`.

- `DialTarget::Udp { host, port }` and `DialTarget::Icmp { host }`; `DialStream::Udp` / `::Icmp`.
- **One guard, not two.** All three IP variants call `vetted_or_refuse` → `guard_target_host_pinned`
  — the function TCP already used. The empty-answer fail-closed rule moved into it too, so TCP's
  spelling and the new ones cannot drift.
- **The pin is enforced by the kernel.** Both datagram sockets are `connect`ed to the vetted address,
  so no later call can address anything else. `datagram_dials_pin_the_address_the_guard_approved`
  asserts DNS is consulted exactly once and reads back `peer_addr()`.
- **Raw ICMP privilege** goes through the `RawIcmpOpener` seam (`SystemRawIcmp` in production).
  `socket(AF_INET, SOCK_RAW, IPPROTO_ICMP)` is attempted at dial time — it transmits nothing — and
  `PermissionDenied` is turned *by `net.rs`* into a message naming `CAP_NET_RAW`. No `SOCK_DGRAM`
  ICMP fallback; see the design's decision table.
- **The raw descriptor is close-on-exec from creation** (`raw_socket_type` folds in `SOCK_CLOEXEC`).
  Setting it with a following `fcntl` would leave the fd inheritable for the width of that window,
  and this process spawns children concurrently on other threads while `Command` closes no inherited
  descriptors — so a `fork`+`exec` in the window hands a child a raw socket that traversed no grant.
  Apple has no such flag; there the `fcntl` remains and the window narrows but does not close.
- **Not reachable from a plugin.** `conn.dial` accepts no `kind` that builds either variant. The
  plugin host's matches are exhaustive by variant so a future surface must decide, not default.

### What the tests can and cannot pin

`the_real_raw_icmp_opener_is_close_on_exec_or_refuses_by_capability` is the only test that executes
the real `SystemRawIcmp::open` and its `unsafe` block. It asserts whichever branch the host takes —
privileged: the descriptor is close-on-exec; unprivileged: `PermissionDenied` maps to a message
naming `CAP_NET_RAW`. Both branches were confirmed reachable on one machine: ordinary run → `Err`,
and `unshare -r -n` + `ip link set lo up` → `Ok` with the `FD_CLOEXEC` assertion passing.

**It cannot pin the close-on-exec *window*, and nothing portable can.** Once `open` returns, the fd
is close-on-exec either way; the difference between the atomic form and a following `fcntl` is
visible only to a `fork` that interleaved between them, and racing a spawn to catch it would be
flaky. The atomicity is therefore pinned where it *is* deterministic — at the source, by
`the_raw_socket_is_created_close_on_exec_atomically`, which fails if the one `libc::socket` call
stops taking its type from `raw_socket_type`. Verified to bite by temporarily restoring the bare
`SOCK_RAW` form.

Under a closed network (`unshare -r -n`, i.e. C-410's confined posture) both variants fail at
construction with `ENETUNREACH` in the message: nothing addressed, nothing sent.

⚠ Adding enum variants is **breaking** for `codewandler-flux-system` consumers that match
exhaustively → obliges a MINOR bump. `check-crate-versions.sh` is structurally blind to this.

### Boundary with the `network-primitives` epic (C-287 UDP op, C-288 ICMP op)

They are different layers, not duplicates, and this story deliberately stays under theirs. C-396 is
the substrate primitive — a guarded destination and a socket pinned to it, in flux-system. C-287/288
are the model-facing operations: a registered `ToolSpec`, intent/policy mapping, bounded datagram
counts and timeouts, multicast/broadcast and spoofing rules, per-reply validation — and both are
gated on C-284's design, which is unwritten. None of that is implemented here.

TCP settles it by precedent: `DialTarget::Tcp` shipped with D-12 and C-286 ("guarded TCP operation")
is still `backlog`. The primitive existing has never been the op existing. What C-287/288 inherit
free: the destination is vetted once and the socket is `connect`ed to it, so the kernel enforces the
send destination *and* the reply source; and C-288's "explicit unsupported result rather than a
shell-out or privilege bypass" is already true at the substrate (`CAP_NET_RAW` named, no `SOCK_DGRAM`
fallback) — C-288 still owns how that surfaces as an op result. Recorded at `crates/flux-system/src/net.rs:236`.

## Notes
- `crates/flux-system/src/net.rs` — `DialTarget`, `DialStream`, `dial_scoped`, `dial_scoped_pinned`,
  `destination_is_private`.
- Platform reality to state in the design, not discover: Linux raw ICMP needs `CAP_NET_RAW` or
  `ping_group_range`; macOS differs. A capability the process may not hold is a deployment fact, so
  the refusal must name it rather than say "permission denied".
