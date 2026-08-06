---
id: C-654
title: "Host metrics over the remote protocol and the host surface"
pillar: "Core"
status: backlog
epic: host-metrics-seam
areas: [flux-server, flux-cli]
design: host-metrics-seam
note: "Decision 0018 rule 6: host.metrics bounded wire operation under a protocol version bump; flux host metrics CLI and usage-observatory projection"
---

# Host metrics over the remote protocol and the host surface

## Goal

Metrics travel the remote protocol and surface to users and agents. `host.metrics` joins the
bounded wire vocabulary of `crates/flux-server/src/system.rs` under a protocol version bump;
`flux host metrics <name>` renders readings for any registered binding; a `host.*` operation
exposes the same readings to agents; and the usage observatory may project them alongside dispatch
usage.

## Acceptance

- [ ] `bounded_operations()` gains `host.metrics` under a `PROTOCOL_VERSION` bump; an older server
      answers a typed unsupported, and the handshake test proves both directions.
- [ ] `flux host metrics <name>` renders typed readings with explicit unavailable entries; JSON
      output is the automation API.
- [ ] The `host.metrics` operation registers behind the ambient host gate with deliberate
      placement, and the placement census is updated.
- [ ] Readings served by a remote binding carry `remotely_reported` provenance.
