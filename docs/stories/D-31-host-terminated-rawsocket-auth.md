---
id: D-31
title: Host-terminated raw-socket auth (no credential to the plugin)
pillar: Core
status: backlog
epic: endpoint-discovery
design: docs/designs/endpoint-discovery.md
---

# Host-terminated raw-socket auth (no credential to the plugin)

## Goal
Close the last gap in the references-only invariant for **raw-socket, in-band-auth** protocols. Today
HTTP plugins never see a credential (the host injects it), but a raw-socket plugin that speaks auth
in-band — Postgres SCRAM (`sql`), Asterisk AMI — receives the resolved credential value through the
gated `credential` capability (trusted plugin only, redacted, never the model). This story makes the
**host** terminate the protocol handshake so even those plugins never receive the credential: the
plugin gets a *post-auth* connection stream.

## Why
The epic's confirmed security decision (D-27) was: gated credential to the trusted plugin **now**,
host-terminated handshake auth as a **stricter future hardening**. This is that hardening. It removes
the one place a (trusted) plugin still holds a secret value, so the invariant becomes absolute, not
"absolute except for in-band-auth raw sockets." See [endpoint-discovery.md](../designs/endpoint-discovery.md)
— *Security model* and the *Future hardening* note.

## Acceptance
- [ ] **Host-side Postgres auth** — the host performs the Postgres startup + SCRAM-SHA-256 / md5
      handshake using the materialized credential and hands `sql` a connection that is already
      authenticated (a post-auth `conn_id` + the negotiated parameters), so `sql` never calls the
      `credential` capability for Postgres. Failing-first test: a MockHost/integration test asserts the
      `sql` plugin frame never carries the password and no `credential` callback is made on the PG path.
- [ ] **Protocol seam** — the handshake terminator is a host-side, per-protocol component (start with
      Postgres; AMI/mysql are follow-on), behind `flux_system::net`/the plugin host, not in the plugin.
- [ ] **`credential` capability stays gated** for any protocol not yet host-terminated (no regression),
      and is removed from `sql`'s grant once PG is host-terminated.
- [ ] Gate green: `cargo test -p flux-plugin -p flux-system` + the `sql` plugin tests; clippy `-D
      warnings`, fmt, `flux-codegate`.

## Progress
- (not started — follow-up to D-27/D-29.)

## Notes
- Larger, protocol-specific effort: the host must speak enough of each wire protocol to authenticate.
  Sequence: Postgres first (the demo path), then mysql, then AMI.
- Prior art: fluxplane resolved HTTP host-side via injected headers; raw-socket termination is the new
  piece. Keep the credential resolution path (cross-plugin gate + audit) unchanged — only *who* speaks
  the handshake moves to the host.
- Design: [endpoint-discovery.md](../designs/endpoint-discovery.md).
