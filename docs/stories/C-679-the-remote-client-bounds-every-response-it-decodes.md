---
id: C-679
title: "The remote client bounds every response it decodes"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-system]
design: first-class-hosts
note: "the server caps requests (MAX_REQUEST_BYTES); the client caps nothing — response.json() on the wire answer and the handshake are unbounded"
---

# The remote client bounds every response it decodes

## Goal

The remote protocol's server side bounds what it will accept (`MAX_REQUEST_BYTES`), but the
client side trusts the far end completely: `response.json::<WireAnswer>()` and the handshake's
`.json::<SystemHandshake>()` decode unbounded bodies into memory for every delegated operation.
C-654's review graded this pre-existing rather than newly load-bearing — the same reachability
already existed through `host.probe` — and routed the cap here. A hostile or broken far end
should cost the client a bounded read and a typed error, never an unbounded allocation. The wire
decoders should read through a capped body path the way the egress client already caps response
bytes, with the limit versioned into the protocol contract rather than invented per call site.

## Acceptance

- [ ] Every client-side decode of a remote-system response (operation answers and the handshake)
      reads through an explicit byte cap; exceeding it yields a typed failure naming the cap, not
      an allocation.
- [ ] The cap is a named protocol constant documented beside `MAX_REQUEST_BYTES`, and a test
      drives an over-limit body from a hostile in-process server through the refusal face.
- [ ] Existing bounded semantics stay intact: the metrics decoder's list/label re-bounding and
      the process-output caps are unchanged and still tested.
- [ ] No behavior change for compliant peers; the negotiated protocol version is unchanged unless
      the wire itself must carry the limit.
