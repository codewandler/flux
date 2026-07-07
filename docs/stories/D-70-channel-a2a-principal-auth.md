---
id: D-70
title: Per-request principal auth parity for the program `a2a` channel
pillar: Agent
status: done
design: docs/designs/request-auth-seam.md
note: "shipped (Unreleased): `flux app run` a2a channel gains the D-69 principal-auth knobs; one shared construction point"
---

# Per-request principal auth parity for the program `a2a` channel

## Goal
Bring the D-69 per-request principal auth to a program's declared `a2a` channel (`flux app run`),
so a multi-tenant program served over a channel gets the same bearer→principal resolution, realm
scoping, and card scheme advertisement as `flux --serve` — not just the pre-D-69 optional token.

## Why (evidence)
D-69 wired principal mode into the standalone `--serve` path only; the `a2a` channel adapter
(`crates/flux-channels/src/adapters/a2a.rs`) still built `ServerAuth::from_token` (token-or-open).
A program is exactly the multi-tenant surface principal auth is for, so the gap left the channel
path a second-class citizen.

## Acceptance
- [x] `A2aSettings` gains the introspection knobs (`introspect_url`, `external_url`,
      `introspect_client_id`, `introspect_secret` as a host-resolved `secret "ENV"`,
      `introspect_account_claim`, `introspect_roles_claim`, `introspect_require_account`,
      `introspect_allow_http`); `introspect_url` selects principal mode, else token-or-open.
- [x] The non-loopback bind guard requires *authentication* (token OR principal), not just a token.
- [x] Construction goes through the ONE shared point `flux_server::PrincipalAuth::from_introspection`
      (new, behind the `introspect` feature) so the security-critical claim mapping is identical to
      `--serve`; the CLI's `server_auth_from_config` was refactored onto the same point.
- [x] Tests: auth-mode selection (open/token/principal), `external_url` + `introspect_secret`
      required-with checks, https-only rejection — in `crates/flux-channels`.
- [x] Documented in `docs/a2a.md` (channel config example) and workspace gate green.

## Progress
- 2026-07-07 DONE alongside D-63. The channel's client secret is a `secret "ENV"` reference
  (host-resolved before deserialization), unlike the CLI's env-var-NAME convention — both feed the
  same `IntrospectionParams`.
