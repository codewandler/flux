---
id: C-348
title: Make pinned egress the only reachable egress API
pillar: Core
status: backlog
epic: egress-pinning-and-confinement-residuals
design: docs/designs/egress-pinning-and-confinement-residuals.md
note: "unpinned helpers stay public and default (oauth_token_grant, resolve_stored_bearer, A2aClient::new); nothing marks the pinned variant as required, so the next adapter picks the wrong one by default"
---

# Make pinned egress the only reachable egress API

## Goal

Remove the default-wrong choice: an author adding an outer adapter should not be able to reach an
unpinned constructor without stepping over a gate that says so.

## Acceptance

- [ ] `flux_credentials::oauth_token_grant` (`crates/flux-credentials/src/lib.rs:563`) and
      `resolve_stored_bearer` (`:611`) either take an injected pinned client or are marked such that
      a new caller cannot pick them silently; the operator login flows in
      `crates/flux-cli/src/auth_cmd.rs:397,471` are updated or explicitly exempted.
- [ ] `flux a2a <URL>` (`crates/flux-cli/src/a2a_cmd.rs:218`) — the one production `A2aClient::new` —
      is guarded, or carries a recorded exemption stating that the URL is operator-typed.
- [ ] The plugin `http.do` pre-check (`crates/flux-plugin/src/host.rs:1228`) uses the same resolver
      as the authoritative guard at `:711`, so one op does not run two different resolvers.
- [ ] `web.crawl`'s unpinned pre-guards (`crates/flux-web/src/crawl.rs:272,292`) are removed or
      annotated — they read like the boundary and are not.
- [ ] Fleet pinning is proven through the registered op: `FleetDispatchTool::execute` (and status,
      cancel) driven with an injected rebinding resolver, including 307/308 POST behaviour. Today
      `worker_client` (`crates/flux-orchestrate/src/fleet.rs:260`) hardcodes `SystemHostResolver`
      with no injection seam on the tool.
- [ ] An exemption inventory test enumerates every outer adapter that resolves a hostname and fails
      when a new one appears unclassified.

## Progress

- 2026-08-01 — filed from validation of NET-01/NET-02.

## Notes

- XMPP room egress (`crates/flux-channels/src/rooms/xmpp/session.rs:160-181`) concedes in-line that
  it closes SSRF-by-configuration, not rebinding. Classify as an owned exemption, not a defect.
