---
id: C-674
title: "HTTP rides the remote protocol under a version bump"
pillar: "Core"
status: done
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

- [x] The protocol gains a versioned HTTP frame; version negotiation refuses a mixed pair, and
      `RemoteSystem`'s `GuardedHttp` delegates instead of refusing once the negotiated version
      carries it.
- [x] No plaintext secret is `Debug`-printable or serialized un-redacted: the request type's
      header carriage becomes redaction-safe by construction before it crosses a frame boundary.
- [x] The serving side enforces the egress guard, redirect-scope rules and `max_response_bytes`
      itself; the requesting side re-caps rather than trusting the wire, and labels route through
      bounded construction on decode.
- [x] Private-destination admission on the serving substrate emits its own audit event, surfaced
      in the caller's audit trail with substrate provenance.

## Design

Three decisions worth stating, because each had a defensible alternative.

**Secret carriage: a wrapper, not a discipline.** `HttpRequest.headers` becomes
`Vec<(String, port::HeaderValue)>`. The value is private, `expose()` is the only reader, `Debug`
prints "how long and whose", never "what" — including for values a caller believed were literal,
because a wrapper that trusted that label would leak precisely when the label was wrong. There is no
`Serialize`: `flux-system` carries no serialization format, so a transport has to encode the value
deliberately and cannot fall into a derive. `GuardedSecretTarget`'s `Debug` redacts the URL query
for the same reason (`in=query` credentials live there). The structural link C-652's review found
missing is `HeaderValue::secret(name, value)` plus `HttpRequest::carried_secrets()`, which unions the
caller's declared list with what the headers themselves say — so a header-placed credential is
authorized at every hop whether or not a separate list remembered it. On the wire the same shape
repeats in `WireSecretText`, whose hand-written `Debug` also makes a later `#[derive(Debug)]` on the
frame a compile error rather than a silent leak.

**A dedicated route, not the `execute` envelope.** `execute` carries arguments as a
`serde_json::Value` — freely printable and freely serializable, which is right for a path or a metric
token and wrong for a resolved credential. So `http.request` gets `POST /system/v1/http` and its own
frame. The cost is stated in the code: that route does not use the delivery ledger, so the frame
carries no at-most-once guarantee. A broken link leaves the request in the `Unreachable` position the
port already describes and the caller decides.

**No second per-family handshake field.** C-654's notes asked whether a generalized
declared-capability set should replace one field per family. It already exists: `operations`. "Does
this peer serve HTTP" is a question about an operation, which is the axis `operations` is on;
`metric_kinds` is not a counter-example, because it declares a vocabulary *within* a family that no
operation list can express. What `operations` lacked was `metric_kinds`'s discipline, so it gains it:
`SystemHandshake::declared_operations()` resolves the peer's list against this build's closed
vocabulary, deduplicated and therefore bounded by it, and the set can only degrade closed. A peer
that declares no HTTP frame is answered with a typed `Unserved` from the handshake, without a
request.
