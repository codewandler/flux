# Guarded network primitives — design questions

## Status

Backlog design placeholder. The catalogue may describe these families as planned, but no public
operation name, signature, or authority is accepted yet.

## Boundary

All resolution and socket IO must remain inside `flux-system`'s guarded boundary. A primitive must
name concrete intent before execution, apply the private-network and scoped-grant posture to every
address actually used, bound bytes/time/concurrency, redact sensitive payloads, and expose a
specific protocol contract rather than arbitrary socket syscalls.

DNS, TCP, UDP, and ICMP deliberately remain separate stories because their lifecycle and authority
differ: DNS has resolver/rebinding semantics, TCP has connection state, UDP has datagram fan-out,
and ICMP may need platform privilege and has no portable guarantee.

## Availability rule

A capability stays `planned` until its child story delivers a registered `ToolSpec`, guarded host
implementation, policy/intent mapping, tests, and public documentation. Catalogue metadata never
pre-allocates the future operation's invocation name.

