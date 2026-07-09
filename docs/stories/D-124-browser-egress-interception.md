---
id: D-124
title: browser egress policy — per-request CDP interception under the scoped web egress model
pillar: Core
status: done
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
- [x] `Fetch.enable` interception on every session: each request URL (navigation, subresource,
      redirect hop, JS-initiated) runs through `flux_system::net::guard_url_scoped` with the
      session's `web`-scope `PrivateNetAllow` (D-98); violations `Fetch.failRequest` and surface
      in the digest/delta as a policy refusal (the model sees *why* the page is broken).
- [x] The SSRF escapes are pinned by failing-first tests (scripted-CDP fake + live fixture):
      (a) public page redirecting to a private-range URL → blocked at the redirect hop;
      (b) page JS `fetch()` to a private host → blocked; (c) same targets under a
      `[private_net] web` grant → allowed with `PrivateNetAdmit` audit events
      (`caller: "web:browser"`, honest `grant_source`).
- [x] D-121's interim navigation-only guard kept as belt-and-suspenders (not deleted — it's cheap and layered under the interception, which is the real policy) (clean cutover); interception has **no off
      switch** (no-fallbacks rule) — it is the policy.
- [x] `--allow-private-net` (D-96) widens the browser session's `web` scope for the run exactly as
      it widens plugin scopes, and its docs say so.

## Progress
- 2026-07-09 — **DONE.** The browser event pump runs `handle_fetch` on every `Fetch.requestPaused`:
  http(s) requests go through `guard_url_scoped` with the session's `web`-scope `PrivateNetAllow` —
  allowed → `Fetch.continueRequest`, refused → `Fetch.failRequest{AccessDenied}` + recorded as an
  egress refusal surfaced in the delta; a private host admitted under a grant audits `PrivateNetAdmit
  { caller: "web:browser" }`. Non-http(s) schemes (data:/blob:/about:) pass through. **No off switch**
  (`Fetch.enable` with no patterns intercepts every request — the policy, not a flag). Hermetic tests:
  private subrequest blocked without a grant; admitted + audited with one; non-http pass-through. The
  D-124 interception is the substance; D-121's coarse nav guard now sits under it.
- 2026-07-09 — Filed with the epic; needs [D-121](D-121-browser-cdp-foundation.md), lands before
  the epic closes. Re-scoped native same day (the scope consulted is the family-wide `web` grant,
  not a plugin grant; substance unchanged).

## Notes
- Interception overhead is accepted by design; if it proves prohibitive the answer is scoping
  patterns (e.g. skip data:/blob:), never a bypass flag.
