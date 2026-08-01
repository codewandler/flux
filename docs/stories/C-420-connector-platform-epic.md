---
id: C-420
title: "The connectors seam — a vendor credential flux is structurally unable to hold (epic)"
pillar: Core
status: backlog
epic: connector-platform
areas: [flux-plugin, flux-capabilities, flux-secret]
note: "tracker filed 2026-08-01 by a board audit. ⚠ Seven of its eight stories are DONE and the epic had no narrative anywhere — no tracker, no design, no roadmap entry — so the credential boundary, one of flux's central invariants, was recorded only inside the stories that built it"
---

# The connectors seam (epic)

## Goal

Let an operator hand flux a *platform* rather than a *secret*: flux calls a connector, the connector
holds the vendor credential, and flux is structurally unable to receive one back.

## ⚠ Why this tracker exists

This epic **shipped its core** without ever being written down as an epic. Seven stories are done;
the invariant they establish — *flux holds exactly one secret on this path, the deployment session
bearer* — lived only in C-312's module header and in the reviews that attacked it. That is precisely
the shape the C-406 sweep was looking for: real, load-bearing work with no narrative a newcomer could
find.

## Members

| story | status | what it established |
|---|---|---|
| C-310 | done | Catalog refresh — a plugin's op set can change when the operator authenticates a provider, without restarting flux |
| C-311 | done | Vendor-host disclosure at approval — the compensating control for the seam's one real trade-off |
| C-312 | done | **The credential boundary** — a response carrying credential-shaped material is *refused*, not merely redacted |
| C-403 | done | The endpoint broker was a second plugin-response ingest surface the boundary's scope statement did not cover |
| C-404 | done | The `internal: true` carve-out was prose with no test pinning it |
| C-410 | done | `flux plugin call` sat outside both the sandbox floor and the approval envelope |
| C-411 | done | A widened plugin manifest was adopted at next load with no operator-visible diff |
| C-405 | ready | The pack's twelve private percent-encoders, one of which has already drifted |

## ⚠ The pattern this epic taught, which outlived it

Four of the eight — C-403, C-404, C-410, C-411 — were **not** planned scope. Each was found by a
review *of the previous one*, and each found the same defect class:

> **a guard or a comment that agrees with its own assumption.**

C-312 asserted a boundary and stated its scope; C-403 found a live call site the scope statement did
not cover. C-403's carve-out was described as dormant; C-404 found it excusing a real dispatched op.
C-410 found the surface that prints plugin-authored strings running outside the very envelope
C-404's hardening presumed. **The trade-off in C-311 is the honest one to keep in view**: when the
platform dials the vendor, `guard_url_scoped` only ever sees `localhost:8000`, so flux's per-vendor
egress allowlist stops constraining which vendor is reached. Disclosure at approval is the
compensating control — not a fix.

Read that chain before extending the seam. A new ingest surface here does not inherit the boundary
by being nearby; it inherits it by being routed through the check.

## Acceptance (for the epic)

- [x] A vendor credential never enters the flux process on this path, asserted by a test rather than
      by design intent (C-312).
- [x] Every plugin-response ingest surface routes through that check — not just the projected-tool
      path (C-403, C-404).
- [x] The surfaces that print plugin-authored strings run inside the sandbox floor and the approval
      envelope (C-410).
- [x] A plugin cannot widen its own grant unobserved (C-411).
- [ ] The pack stops carrying twelve private copies of one primitive (C-405). ⚠ This is a
      **protocol-line change** — `host-kit` is published, so per the repo's version rule it owes a
      version decision and a pack release; `scripts/check-crate-versions.sh` is the thing that
      catches it.
- [ ] This narrative reaches `docs/roadmap.md`, so the seam is findable without reading eight stories.

## Notes

- Filed as part of the C-406 curation sweep. It is a **tracker**, not new scope: all eight members
  already existed and already carried this slug.
- Related but distinct: [C-419](C-419-verified-webhook-channel-epic.md) is about proving an *inbound*
  delivery's origin; this epic is about never holding an *outbound* credential.
- The downstream connector work lives outside this repo (`../flux-connectors`, `../flux-exchange`);
  this epic is only flux's half of the seam.

## Progress

- Filed 2026-08-01. Members unchanged; the invariant statement and the defect-class chain above are
  the new content.
