---
id: C-690
title: "Clock offset joins the host metric vocabulary"
pillar: "Core"
status: backlog
epic: the-substrate-seam
areas: [flux-system]
design: docs/designs/the-substrate-seam.md
note: "time is not an authorization boundary — it is substrate condition; sampled_at is stamped by the reader's clock and no field expresses the skew a consumer compares across"
---

# Clock offset joins the host metric vocabulary

## Goal

Time deliberately does not belong on the guarded port: it has no blast radius, cannot be
meaningfully refused, and a fail-closed `Unserved` clock would be useless. But substrate time is
already a *measurement* — `uptime` is a metric kind and every snapshot carries `sampled_at`
stamped by whichever clock read it. A remote snapshot therefore carries the far machine's time
while every consumer compares it against the coordinator's, and nothing expresses the difference.
Nodes drift; an audit or monitoring view that mixes the two clocks is quietly wrong about
ordering. Add the skew as what it is: a reading about the substrate, in the closed vocabulary that
already exists.

## Acceptance

- [ ] A clock-offset metric kind joins the closed `MetricKind` vocabulary, reporting the
      substrate's wall clock relative to the reading process's, with the measurement's own
      round-trip uncertainty carried beside it rather than hidden.
- [ ] The native backend answers zero offset by construction (same clock) rather than by
      measurement; a remote substrate answers a real difference; an unmeasurable one answers
      explicitly unavailable, never zero.
- [ ] `sampled_at`'s documentation states whose clock stamped it, and the remote hop records the
      offset alongside so a consumer can reconcile without guessing.
- [ ] A test drives a fixture substrate with a deliberately skewed clock and proves the reported
      offset and the unavailable face.
