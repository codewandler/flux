---
id: D-124
title: browser egress policy — per-request CDP interception under the scoped web egress model
pillar: Core
status: backlog
priority:
epic: web-capabilities
design: docs/designs/web-capabilities.md
note: "required for epic-done: Fetch.enable interception runs EVERY request (nav, subresource, redirect hop, JS-initiated) through guard_url_scoped with the session's `web`-scope PrivateNetAllow; violations fail the request + surface in the digest; PrivateNetAdmit caller web:browser; no off switch (no-fallbacks rule); needs D-121"
---

# browser egress policy — per-request CDP interception under the scoped web egress model

## Goal
Make the browser subject to the same egress policy as every other flux surface, at the only layer
that actually governs a browser: per-request interception. A navigation-only check cannot hold —
redirect-to-private and JS `fetch()` are the classic SSRF escapes — so D-121's coarse gate is
replaced, not layered on.

## Acceptance
- [ ] `Fetch.enable` interception on every session: each request URL (navigation, subresource,
      redirect hop, JS-initiated) runs through `flux_system::net::guard_url_scoped` with the
      session's `web`-scope `PrivateNetAllow` (D-98); violations `Fetch.failRequest` and surface
      in the digest/delta as a policy refusal (the model sees *why* the page is broken).
- [ ] The SSRF escapes are pinned by failing-first tests (scripted-CDP fake + live fixture):
      (a) public page redirecting to a private-range URL → blocked at the redirect hop;
      (b) page JS `fetch()` to a private host → blocked; (c) same targets under a
      `[private_net] web` grant → allowed with `PrivateNetAdmit` audit events
      (`caller: "web:browser"`, honest `grant_source`).
- [ ] D-121's interim navigation-only guard is deleted (clean cutover); interception has **no off
      switch** (no-fallbacks rule) — it is the policy.
- [ ] `--allow-private-net` (D-96) widens the browser session's `web` scope for the run exactly as
      it widens plugin scopes, and its docs say so.

## Progress
- 2026-07-09 — Filed with the epic; needs [D-121](D-121-browser-cdp-foundation.md), lands before
  the epic closes. Re-scoped native same day (the scope consulted is the family-wide `web` grant,
  not a plugin grant; substance unchanged).

## Notes
- Interception overhead is accepted by design; if it proves prohibitive the answer is scoping
  patterns (e.g. skip data:/blob:), never a bypass flag.
