---
id: C-394
title: "flux-system as a shared execution substrate (epic)"
pillar: Core
status: ready
priority: 6
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "EPIC — the port seam already exists and names 'a remote executor' as a target; what is missing is the file surface, datagram targets, and a written contract for consuming the substrate WITHOUT flux-runtime"
---

# flux-system as a shared execution substrate (epic)

## Goal

Make `flux-system` consumable by a second, out-of-repo consumer without weakening any guarantee it
holds today, and without merging it into `flux-runtime` — the two are peers at L2 and answer opposite
questions (*may this happen* vs *this is where it happens*).

## Acceptance

- [ ] C-395 — the workspace-confined file surface is a port, and a port-based consumer is confined
      exactly as a concrete-`System` consumer is.
- [ ] C-396 — `DialTarget` covers UDP and ICMP, and an insufficient-capability raw target is refused
      at construction.
- [ ] C-398 — a written contract states which guarantees travel with `flux-system` and which are
      `flux-runtime`'s and do not.
- [ ] No new IO path: `flux-codegate`'s `no_unreviewed_guarded_port_backend_outside_system` and the
      direct-IO gate stay green with no new allowance.
- [ ] The layer map is unchanged; `flux-system` and `flux-runtime` both stay L2.

## Progress
- (not started)

## Notes
- Design: [execution-substrate.md](../designs/execution-substrate.md); context:
  [ecosystem.md](../designs/ecosystem.md).
- C-397 and C-399 (container + remote backends) are members of this epic but sit in `backlog`
  because **ownership is undecided** — the port is unsealed, so an out-of-repo consumer can
  implement either without flux changing. Do not promote them without deciding that first.
- The prior art is C-269 (`done`), which introduced the port seam for the portable-wasm epic. This
  epic finishes it for a different consumer.
