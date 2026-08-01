---
id: D-130
title: Authenticate the Vault credential store with Kubernetes service accounts
pillar: Core
status: done
design: docs/designs/plugin-oauth.md
epic: plugin-oauth
note: "downstream C-27: keep static-token/file compatibility, add eager Kubernetes login + lease renewal/re-auth for host-injected Vault stores"
---

# Authenticate the Vault credential store with Kubernetes service accounts

## Goal
Let a deployed host inject a Vault KV-v2 credential store without mounting a long-lived
`VAULT_TOKEN`: authenticate with the pod's projected service-account JWT and keep the resulting
Vault token healthy across renewal and JWT rotation.

## Acceptance
- [x] `VaultCredentialStore` has an additive Kubernetes-auth constructor/config while the existing
      static-token constructor and `from_env` behavior remain compatible.
- [x] Construction logs in eagerly; KV calls renew near expiry, re-read the projected JWT when
      re-authenticating, and retry exactly once after Vault returns 401/403.
- [x] Failing-first loopback tests prove login, KV-v2 round-trip, renewal, rotated-JWT re-login, and
      static-token regression coverage without exposing either token in errors.
- [x] Full workspace gate and `flux-codegate` remain green; release in the next 0.14.x patch for
      downstream ai-agent-platform C-27.

## Progress
- 2026-07-10 filed from ai-agent-platform C-27 after the deployment selected Kubernetes Vault auth.
- 2026-07-10 **DONE.** Released in flux v0.14.8; full workspace/build/test/clippy/fmt/codegate green,
  crates.io publication + binary release workflows succeeded, and the downstream platform resolved
  the published 0.14.8 closure without path overrides.

## Notes
- Downstream consumer: the private `ai-agent-platform` repo, story C-27.
- Kubernetes login endpoint: `POST /v1/auth/<mount>/login`; renewal uses
  `POST /v1/auth/token/renew-self`.
