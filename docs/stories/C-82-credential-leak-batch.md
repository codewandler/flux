---
id: C-82
title: Close credential-leak vectors — OAuthToken Debug and inline-URL credentials
pillar: Core
status: done
priority: 9
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "Secret-leak (Medium) — token Debug + inline user:pass@ creds bypass the cross-plugin gate and reach the model"
---

# Close credential-leak vectors — OAuthToken Debug and inline-URL credentials

## Goal
Three related credential-exposure gaps: (1) `OAuthToken`/`Refreshed` derive `Debug`, so one
`tracing::debug!(?token)`/`?err` dumps live bearer + refresh + id_token into logs/traces/events;
(2) inline-URL credentials (`user:pass@host`) are injected as `Authorization: Basic` unconditionally,
bypassing the deny-by-default cross-plugin gate that guards `credential_ref`; (3) the same inline creds
are rendered verbatim to the model by `endpoint.list`/`info`.

## Acceptance
- [ ] Failing-first test: `format!("{:?}", token)` for `OAuthToken`/`Refreshed` does NOT contain the
      secret material; hand-write redacting `Debug` (as `IntrospectionConfig` already does).
- [ ] Inline-URL credentials route through the same `authorize_cross_plugin` + audit path as `credential_ref`.
- [ ] `endpoint.list`/`info` redact userinfo before rendering (reuse `split_inline_credential`'s bare URL).

## Progress
- (not started) — filed from the 2026-07-15 full code review.

## Notes
- `crates/flux-credentials/src/lib.rs:82,354`; `crates/flux-capabilities/src/endpoint/broker.rs:750,765`;
  `crates/flux-capabilities/src/endpoint/ops.rs:210,100`.
- Design: [harness-hardening](../designs/harness-hardening.md).
