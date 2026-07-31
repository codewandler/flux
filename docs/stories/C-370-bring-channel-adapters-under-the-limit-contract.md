---
id: C-370
title: Bring the webhook and connector channel adapters under the server limit contract
pillar: Core
status: backlog
epic: serving-surface-and-turn-outcome-residuals
design: docs/designs/serving-surface-and-turn-outcome-residuals.md
note: "both mount a bare Router::new() — no body limit, no timeout, no rate guard, no concurrency permit, no provider budget — and tokio::spawn the delivery BEFORE admission; connector.rs:1086-1089 comments that it 'adds no queue of its own', which is wrong"
---

# Bring the webhook and connector channel adapters under the server limit contract

## Goal

Make everything C-189 and C-261 built apply to the ingress that a `.flux` program actually exposes
when it declares a `webhook` or `connector` channel and is served by `flux app run`.

## Acceptance

- [ ] `crates/flux-channels/src/adapters/webhook.rs:96-99,191` and
      `connector.rs:686-699,1110` carry a body limit, a request timeout, the principal-keyed rate
      guard, a concurrency permit and the provider budget — the same contract `flux-server` enforces.
- [ ] Admission is acquired **before** `tokio::spawn` (`webhook.rs:120`, `connector.rs:1091`), so a
      burst cannot park unbounded tasks behind the semaphore.
- [ ] The process-global admission bound (`crates/flux-app/src/admission.rs:135`) is keyed to a
      principal, or its global nature is documented where operators read about limits.
- [ ] The incorrect comment at `connector.rs:1086-1089` is corrected.
- [ ] Failing-first: a burst against a webhook binding is rejected with a typed limit response
      rather than spawning; an oversized body is rejected.
- [ ] The loopback-with-no-token open posture is documented or refused, matching how `flux-server`
      refuses a non-loopback bind without a token.

## Progress

- 2026-08-01 — filed from the ingress inventory built during validation. Largest live gap in the epic.

## Notes

- `connector`'s HMAC verification is unimplemented and hard-refuses at load
  (`connector.rs:509-517`), so a bearer token is the only possible authentication there today.
