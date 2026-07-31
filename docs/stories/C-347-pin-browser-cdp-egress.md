---
id: C-347
title: Pin or explicitly bound browser (CDP) egress, and make its audit record name the address dialled
pillar: Core
status: backlog
epic: egress-pinning-and-confinement-residuals
design: docs/designs/egress-pinning-and-confinement-residuals.md
note: "model-facing web.browser guards the URL and lets Chrome resolve again; a THIRD resolution writes the audit record, so the audit can disagree with what was contacted. C-77 recorded this residual and no follow-up story was ever filed"
---

# Pin or explicitly bound browser (CDP) egress

## Goal

Stop a model-browsable attacker host from rebinding into RFC1918 or link-local space between the
guard's answer and Chrome's connection, and stop the audit trail from describing a resolution
nobody used.

## Acceptance

- [ ] `goto` (`crates/flux-web/src/browser.rs:203`) and the fetch interception path (`:696`) either
      bind the navigation to the vetted addresses (e.g. host-resolver-rules pinned per navigation,
      or refusing a host whose re-resolution differs) or the op is documented as an operator-trust
      surface with the residual stated in the op description itself.
- [ ] `host_resolves_private` (`browser.rs:698-700`) no longer performs an independent third
      resolution for the audit record; the record names the address the connection used.
- [ ] Failing-first regression with a rebinding resolver asserting the private-range listener is
      never reached, or — if the documented-residual route is taken — a test asserting the op
      description and `docs/security/` disclose it.
- [ ] `docs/stories/C-77-egress-dns-rebinding-pin.md`'s `[~]` residual points at this story.

## Progress

- 2026-08-01 — filed from validation of NET-01.

## Notes

- Decide the route before implementing: pinning a full browser engine's resolution is materially
  harder than pinning a reqwest client, and an honest documented bound may be the better answer.
