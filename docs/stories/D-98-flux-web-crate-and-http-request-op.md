---
id: D-98
title: flux-web crate + http.request — native arbitrary HTTP under one scoped web egress policy
pillar: Core
status: done
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
- [x] New L5 library crate `crates/flux-web` (package `codewandler-flux-web`): root `Cargo.toml`
      `members` + a **path-only** `[workspace.dependencies]` alias (not in the publish closure —
      the `flux-eval` precedent; only flux-cli consumes it) + `"flux-web"` added to the L5 arm of
      the `flux-codegate` layer match (`cargo test -p flux-codegate` green).
- [x] Registration via the flux-eval precedent: `flux_web::register_web(&mut registry, &opts)` wired
      at the two `flux-cli/src/main.rs` registration sites (live loop + `skill_ops_registry`) beside
      `flux_eval::register_eval_ops`. Takes a `WebOptions` (private-net scope + audit sink +
      grant-source) because — unlike eval ops — web ops do guarded egress; the group-push sites are
      D-121's (`browser_group()`).
- [x] `http.request` op (`flux-web::http`): `method`, `url`, `headers`, `body`, `timeout` →
      `status`, response headers (capped), body (capped, char-boundary safe — the `web_fetch`
      `MAX_BYTES` precedent). Non-2xx is a result, not an op failure (test:
      `not_found_is_a_result_not_a_failure` — 404 returns Ok, status in body). Ungated (`group: None`).
- [x] The family-wide egress scope: `[private_net] web` (`PrivateNetGrant` shape) resolved once via
      `Config::web_private_hosts()` and applied through `flux_system::net::guard_url_scoped` on every
      call; `--allow-private-net` widens it ephemerally (`effective_web_private_hosts`). Failing-first
      tests: `private_target_refused_without_grant`; `guard_allows_public_refuses_private`;
      `private_admit_emits_audit_event` (private target + `web` grant → allowed **with** a
      `PrivateNetAdmit`, `caller: "web:http.request"`, honest `grant_source`).
- [x] Secrets: header values that are `{"$secret": "ENV"}` markers are resolved from the environment
      and seeded into `ctx.redactor` — test `secret_header_is_resolved_and_seeded_into_the_redactor`
      proves the value is scrubbed; `missing_secret_header_env_is_a_clean_error` names the var.
- [x] Honest metadata: `Effect::Network`, `NetworkFetch` intent, `Risk::Medium` + `NonIdempotent`
      (arbitrary HTTP can mutate) — plan approval sees it (D-91 lesson).

## Progress
- 2026-07-09 — **DONE.** Crate scaffolded (`crates/flux-web`: `lib.rs` with `register_web`/`WebOptions`,
  `http.rs`). Root `Cargo.toml` member + path-only alias; `flux-web` added to the flux-codegate L5 arm
  (codegate green). `[private_net] web` field + `Config::web_private_hosts()` in flux-config;
  `effective_web_private_hosts` / `web_grant_source` helpers + `register_web` wiring at both flux-cli
  sites (audit sink = the same `EventStoreEgressAudit` the plugin path uses). 6 crate tests green;
  ops-reference / flux-flow skill / website config docs updated; CHANGELOG + WHATS-NEW entries. The
  `web_fetch` per-tool key is retained here and retired by D-120's cutover.
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
