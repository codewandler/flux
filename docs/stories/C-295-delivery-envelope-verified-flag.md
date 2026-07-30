---
id: C-295
title: "The delivery envelope — an Event carries no id, no source and no `verified` flag"
pillar: Core
status: backlog
epic: verified-webhook-channel
note: "flux_app::Event is {label, payload} and nothing else, so 'put the delivery id in the payload' writes envelope data into the message body — where seed_payload binds every top-level field as a flow symbol and a vendor key silently shadows it. The security half: a flow cannot tell a signature-verified delivery from an unverified one"
---

# The delivery envelope — an Event carries no id, no source and no `verified` flag

## Goal

Give a delivery's **metadata** somewhere structured to live, so a flow can dedupe a redelivery and —
the part with a security edge — can tell a signature-verified event from an unverified one.

## Context — verified against this tree

1. **`flux_app::Event` is `{ label, payload }` and nothing else** —
   `crates/flux-app/src/bus.rs:115-118`. No id, no received-at, no source, no "this was
   signature-verified" flag. So the obvious spelling of "the delivery id reaches the flow" is envelope
   data written into the message body.
2. **`seed_payload` binds the whole payload to `$input` *and every top-level field to its own
   symbol*** — `crates/flux-app/src/app.rs:1988-1996`. A vendor payload carrying a field named like an
   injected key silently shadows it, and a flow reading `{delivery_id}` cannot tell whose value it got.
3. Consequently a program written against a **signed** GitHub webhook behaves **identically** if an
   operator later points an unverified transport at the same trigger label. The flow cannot tell, and
   neither can a reviewer reading the flow.

## Acceptance

- [ ] **A decision, recorded with its trade-off:** does `Event` grow an envelope (id, received-at,
      source, verified), or does a binding declare a reserved prefix that `seed_payload` must not
      collide with? An envelope is the honest model and touches every channel; a prefix is cheap and
      leaves the shadowing possible for anyone who ignores it. Write the answer down here before
      implementing either.
- [ ] **A flow can distinguish a verified delivery from an unverified one**, and it cannot be forged
      by the payload. Failing-first test `unverified_delivery_is_distinguishable_from_a_verified_one`:
      the same body delivered through a `verify`-declared channel and through a
      `verify "none"` channel produces observably different envelopes.
- [ ] **The flag is host-stamped, never payload-derived.** A vendor body containing
      `{"verified": true}` must not be able to make an unverified delivery look verified — this is the
      shadowing in (2) pointed at the one field where it is a security defect rather than a confusion.
      Test `payload_cannot_forge_the_verified_flag`.
- [ ] An optional `delivery_id` declaration on the channel (`source`, `name`) whose resolved value
      reaches the flow intact. Failing-first test `delivery_id_reaches_the_flow`.
- [ ] Documented guidance that the envelope key names are **stable**, since flows will key dedupe
      state on them.
- [ ] **No dedupe state is kept in the channel itself** — the channel reports, the flow decides. A
      cache in the channel would be per-process and silently wrong across a restart and across
      replicas, which is the failure mode that looks like it works in testing.
- [ ] C-291's tri-state survives to here: `verify` absent, `verify "none"` and a `verify` record are
      three different things at the declaration, and at least the verified/unverified distinction is
      three-valued or explicitly documented as collapsed to two with a reason.

## Progress

- (not started)

## Notes

- Depends on **C-291** for the verification result that the flag reports. The `delivery_id` half is
  independent of C-292/C-293/C-294.
- Design: `../flux-connectors/docs/designs/verified-webhook-seam.md` §3, capability 6. The consumer
  repository filed the same gap from its side as flux-connectors' C-85
  (`../flux-connectors/docs/stories/C-85-delivery-envelope.md`) — that story owns the decision's
  connector-side consequences and should be updated with whatever is decided here.
- **Why this is filed with the verification seam rather than after it:** flux-connectors' C-82 makes
  "a deliberately-unverifiable surface must be distinguishable from a verified one without inspecting
  the absence of a field" a connector-side invariant
  (`../flux-connectors/crates/connector-spec/src/inbound.rs`, `ChannelBinding::verification` — the
  tri-state doc comment). If flux normalises the distinction away at delivery, the connector-side
  invariant is true and useless.
- Vendors redeliver on a non-2xx and some redeliver spuriously; delivery is at-least-once. Without the
  delivery id, a retried webhook is indistinguishable from a second real event.
