---
id: D-98
title: flux-web crate + http.request — native arbitrary HTTP under one scoped web egress policy
pillar: Core
status: ready
priority: 17
epic: web-capabilities
design: docs/designs/web-capabilities.md
note: "tier 1, native (user call: web capabilities are table-stakes, NO plugins): new L5 crate crates/flux-web (codewandler-flux-web, path-only/unpublished) + http.request op + the family-wide `[private_net] web` scope (public-only default, guard on every call, PrivateNetAdmit caller web:<op>); secrets via secret-refs + redactor seeding"
---

# flux-web crate + http.request — native arbitrary HTTP under one scoped web egress policy

## Goal
Give the model raw protocol access — any method/headers/body, status and bytes back — as a
**native** op, zero-install, governed by one family-wide scoped egress policy instead of today's
per-tool special case. Founds the `crates/flux-web` crate every other tier builds on. Tier 1 of
[web-capabilities](../designs/web-capabilities.md): APIs → `http.request`.

## Acceptance
- [ ] New L5 library crate `crates/flux-web` (package `codewandler-flux-web`): root `Cargo.toml`
      `members` + a **path-only** `[workspace.dependencies]` alias (not in the publish closure —
      the `flux-eval` precedent; only flux-cli consumes it) + `"flux-web"` added to the L5 arm of
      the `flux-codegate` layer match (`cargo test -p flux-codegate` green).
- [ ] Registration via the flux-eval precedent: `flux_web::register_web(&mut registry)` wired at
      the same four `flux-cli/src/main.rs` sites as `flux_eval::register_eval_ops`.
- [ ] `http.request` op (`flux-web::http`): `method`, `url`, `headers`, `body`, `timeout` →
      `status`, response headers (capped), body (capped, char-boundary safe — the `web_fetch`
      `MAX_BYTES` precedent). Non-2xx is a result, not an op failure (test: 404 returns
      `status: 404`, op succeeds). Ungated (`group: None`).
- [ ] The family-wide egress scope: `[private_net] web` (`PrivateNetGrant` shape) resolved once
      and applied via `flux_system::net::guard_url_scoped` on every call; `--allow-private-net`
      widens it ephemerally. Failing-first tests: private/loopback target refused without grant;
      public target allowed; private target + `web` grant → allowed **with** a `PrivateNetAdmit`
      event (`caller: "web:http.request"`, honest `grant_source`).
- [ ] Secrets: header values resolved from `secret` references ride `resolve_secrets` and are
      redactor-seeded (C-13) — test: a Bearer token in a header never appears readable in the tool
      result rendering or persisted observations (C-22 lesson).
- [ ] Honest metadata: `Effect::Network`, `NetworkFetch` intent, non-flat risk — plan approval
      sees it (D-91 lesson).

## Progress
- 2026-07-09 — **Re-scoped native** (user call): the web capabilities are table-stakes and none
  should sit behind a plugin install; the earlier plugin shape (plugins/web over host `http.do`,
  manifest `http_hosts: ["*"]` wildcard) is dropped — the family-wide `[private_net] web` scope
  replaces what the manifest declaration would have bought. Story now also founds the crate.
- 2026-07-09 — Rescoped into the [web-capabilities](../designs/web-capabilities.md) epic; the
  URL→markdown half moved to [D-120](D-120-web-fetch-readable-markdown.md).
- Originally captured 2026-07-08 from the D-96 discussion.

## Notes
- Motivation trail: `--allow-private-net` (D-96) fully opens `web_fetch` to private ranges because
  the native tool had no scoped policy to intersect against; the `web` scope closes that for the
  whole family (D-120 deletes the per-tool special case).
- Dotted native op names are established (`proc.run`, `ai.extract`, `endpoint.discover`).
