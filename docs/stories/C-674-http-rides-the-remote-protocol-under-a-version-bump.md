---
id: C-674
title: "HTTP rides the remote protocol under a version bump"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-system, flux-web]
design: first-class-hosts
note: "Decision 0018 rule 5's deferred wire change; C-652's review fixed the shape constraints the frame must honor before secrets ride it"
---

# HTTP rides the remote protocol under a version bump

## Goal

C-652 put `GuardedHttp` on the port and left `RemoteSystem` answering a typed `Unserved` naming
the missing wire support. This story is that wire: the remote protocol gains one versioned HTTP
request/response frame, and a remote host serves the family through the same guarded seam its
serving process already trusts. C-652's review fixed the constraints the frame must honor before
any secret rides it: `HttpRequest` derives `Debug` while carrying resolved plaintext header
values; nothing structurally links `headers` to `secrets.carried`; and the response byte bound is
today the substrate's promise, which a wire decoder must not extend trust to.

## Acceptance

- [ ] The protocol gains a versioned HTTP frame; version negotiation refuses a mixed pair, and
      `RemoteSystem`'s `GuardedHttp` delegates instead of refusing once the negotiated version
      carries it.
- [ ] No plaintext secret is `Debug`-printable or serialized un-redacted: the request type's
      header carriage becomes redaction-safe by construction before it crosses a frame boundary.
- [ ] The serving side enforces the egress guard, redirect-scope rules and `max_response_bytes`
      itself; the requesting side re-caps rather than trusting the wire, and labels route through
      bounded construction on decode.
- [ ] Private-destination admission on the serving substrate emits its own audit event, surfaced
      in the caller's audit trail with substrate provenance.
