---
id: C-394
title: "flux-system as a shared execution substrate (epic)"
pillar: Core
status: in-progress
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "EPIC — file/env/process/network ports and the authenticated remote system ship; inbound-listener migration and the owned container-process backend remain"
---

# flux-system as a shared execution substrate (epic)

## Goal

Make `flux-system` consumable by a second, out-of-repo consumer without weakening any guarantee it
holds today, and without merging it into `flux-runtime` — the two are peers at L2 and answer opposite
questions (*may this happen* vs *this is where it happens*).

## Acceptance

- [x] C-395 — the workspace-confined file surface is a port, and a port-based consumer is confined
      exactly as a concrete-`System` consumer is.
- [x] C-396 — `DialTarget` covers UDP and ICMP, and an insufficient-capability raw target is refused
      at construction.
- [x] C-398 — a written contract states which guarantees travel with `flux-system` and which are
      `flux-runtime`'s and do not.
- [x] C-399/C-473…C-476 — the port has a selectable authenticated remote implementation with opaque
      managed resources and honest delivery outcomes.
- [ ] C-435 — migrate the remaining server/channel listeners onto the guarded inbound port.
- [ ] C-397 — land the Flux-owned container backend for guarded process effects.
- [ ] No new IO path: `flux-codegate`'s `no_unreviewed_guarded_port_backend_outside_system` and the
      direct-IO gate stay green with no new allowance.
- [x] The layer map is unchanged; `flux-system` and `flux-runtime` both stay L2.

## Progress

- 2026-08-02: C-395/C-396/C-398/C-399 and the production remote-system series are shipped.
  `GuardedNetwork` covers bounded stream/listener/datagram resources. The epic remains open for the
  older-listener migration in C-435 and the owned local container-process backend in C-397.

## Notes
- Design: [execution-substrate.md](../designs/execution-substrate.md); context:
  [ecosystem.md](../designs/ecosystem.md).
- Ownership is settled: Flux owns both C-397 and C-399 so local-first use does not depend on an
  external service. C-399 ships; C-397 is ready.
- The prior art is C-269 (`done`), which introduced the port seam for the portable-wasm epic. This
  epic finishes it for a different consumer.
