---
id: D-98
title: web plugin + http.request — arbitrary HTTP under the plugin envelope
pillar: Core
status: ready
priority: 17
epic: web-capabilities
design: docs/designs/web-capabilities.md
note: "tier 1 of the web-capabilities epic: plugins/web (host http.do only) + a generic http.request op; new mechanism = manifest open-PUBLIC egress (`http_hosts: [\"*\"]`, SSRF guard always on, private only via scoped grant); auth by secret-purpose injection, raw header values redactor-seeded"
---

# web plugin + http.request — arbitrary HTTP under the plugin envelope

## Goal
Give the model raw protocol access — any method/headers/body, status and bytes back — as a normal
plugin op under the standard envelope (declared egress, scoped private-net grant, secret injection,
redaction), instead of not-at-all today. Tier 1 of
[web-capabilities](../designs/web-capabilities.md): APIs → `http.request`.

## Acceptance
- [ ] `plugins/web` crate (binary `flux-plugin-web`) over the host `http.do` capability — no
      `reqwest`/`std::net` in the plugin (D-27 references-only invariant holds).
- [ ] Manifest open-public egress: `http_hosts: ["*"]` = *any public host*. Failing-first test in
      `flux-plugin` host-caps enforcement: wildcard manifest + private/loopback target → denied
      without a grant (guard still runs); public target → allowed; private target + scoped
      `[private_net.plugins]` grant → allowed with a `PrivateNetAdmit` audit event.
- [ ] `http.request` op: `method`, `url`, `headers`, `body`, `timeout` → `status`, response headers
      (capped), body (capped, char-boundary safe — the `web_fetch` `MAX_BYTES` precedent). Non-2xx
      is a result, not an op failure (test: 404 returns `status: 404`, op succeeds).
- [ ] Auth by secret-purpose injection (the D-12 Basic/header/query mechanism) is the documented
      path; raw header values are seeded into the redactor (C-13) — test: a Bearer token passed as
      a raw header never appears readable in the tool result rendering or persisted observations.
- [ ] Honest metadata: `Effect::Network`, `NetworkFetch` intent, non-flat risk — plan approval sees
      it (D-91 lesson).
- [ ] A keyless `smoke-plugins.sh` leg drives one `http.request` against a public endpoint (SKIP
      when offline).

## Progress
- 2026-07-09 — Rescoped into the [web-capabilities](../designs/web-capabilities.md) epic: this
  story keeps tier 1 (the plugin + `http.request` + the open-public-egress mechanism); the
  URL→markdown half moved to [D-120](D-120-web-fetch-readable-markdown.md), which also owns retiring
  `web_fetch`'s bespoke native private-net path (the original third bullet here).
- Originally captured 2026-07-08 from the D-96 discussion.

## Notes
- Motivation trail: `--allow-private-net` (D-96) fully opens `web_fetch` to private ranges because
  the native tool has no manifest to intersect against; the plugin path is gated like every other
  integration. D-95 (direct-call grant parity) is adjacent and unblocked separately.
- The wildcard is a *declaration*, not a bypass: `flux_system::net::guard_url_scoped` runs on every
  call; the wildcard only ever widens to public ranges.
