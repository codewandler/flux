---
id: C-13
title: Seed the redactor from resolve_secrets (and one provider-key source of truth)
pillar: Core
status: ready
priority: 2
note: secret "NAME" program refs resolve to plaintext without ever registering with the redactor — only 4 fixed env keys are seeded; a Slack bot token in tool output would pass unredacted unless it happens to match a credential-shape prefix
---

# Seed the redactor from resolve_secrets

## Goal
Make the README:167 claim ("Secrets are registered with a redactor and scrubbed from all tool
output and logs") true for **app-declared secrets**. Verified 2026-07-02:
`flux_app::resolve_secrets` (`crates/flux-app/src/secrets.rs:15-54`) resolves `{"$secret":"NAME"}`
markers in agent/channel/datasource settings to plain values in-memory but never calls
`add_secret`; startup seeds exactly four fixed env keys (`crates/flux-cli/src/main.rs:1442-1452`).
Any secret beyond those four is scrubbed only if it matches a credential-shape prefix by luck.

## Acceptance
- [ ] **Failing-first:** `resolved_secret_is_registered_with_the_redactor` (flux-app) — resolve a
      program with a `{"$secret":…}` marker against a `Redactor`, assert `redactor.redact(value)`
      masks it (fails today: the function has no redactor at all).
- [ ] `resolve_secrets(program: &mut Program, redactor: &Redactor)` seeds every resolved value at
      the moment of resolution (clean cutover signature change; flux-app gains the flux-secret dep,
      L6→L0).
- [ ] ONE shared `Redactor` flows into every executor the App builds (`build_executor` +
      `build_agent_engine` via `.with_redactor`; Redactor clones share the value store) — app-level
      test: an op echoing a resolved setting returns `[REDACTED…]` through the executor.
- [ ] `flux_credentials::provider_env_keys()` is the single source for provider env-key seeding —
      used by both `provider_statuses` and the CLI seeding (replaces the hardcoded 4-key list;
      `FLUX_SECRET` stays CLI-side).
- [ ] Full gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P2 of the round).

## Notes
- Files: crates/flux-app/src/secrets.rs + app.rs, crates/flux-cli/src/main.rs (:1443, :4206 path),
  crates/flux-credentials/src/lib.rs.
- Precedent: `RedactorSecretSink::register_secret` already does this for host-materialized plugin
  credentials (main.rs:1046-1054).
