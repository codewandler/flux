---
id: C-13
title: Seed the redactor from resolve_secrets (and one provider-key source of truth)
pillar: Core
status: done
priority: 2
note: resolve_secrets now seeds every resolved value into the ONE redactor the App's journey + agent-target executors redact with; provider env-key seeding is sourced from flux_credentials::provider_env_keys() (now incl. the AWS secret material)
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
- [x] **Failing-first:** `resolved_secret_is_registered_with_the_redactor` (flux-app) — resolve a
      program with a `{"$secret":…}` marker against a `Redactor`, assert `redactor.redact(value)`
      masks it (fails today: the function has no redactor at all).
- [x] `resolve_secrets(program: &mut Program, redactor: &Redactor)` seeds every resolved value at
      the moment of resolution (clean cutover signature change; flux-app gains the flux-secret dep,
      L6→L0).
- [x] ONE shared `Redactor` flows into every executor the App builds (`build_executor` +
      `build_agent_engine` via `.with_redactor`; Redactor clones share the value store) — app-level
      test: an op echoing a resolved setting returns `[REDACTED…]` through the executor.
- [x] `flux_credentials::provider_env_keys()` is the single source for provider env-key seeding —
      replaces the CLI's hardcoded 4-key list on both the `build_agent` and `app run` paths
      (`FLUX_SECRET` stays CLI-side; `auth_status` keeps its provider-paired *display* list, which
      also covers non-env OAuth sources).
- [x] Full gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P2 of the round).
- Done 2026-07-02. `resolve_secrets(program, &redactor)` add_secrets every resolved value;
  `App::with_sub_agents` takes the host's redactor and `Engine` installs it on BOTH executor-build
  paths (`build_executor` for journeys, `build_agent_engine` for agent targets) via
  `ToolContext::with_redactor`. New CLI helper `seed_provider_env_secrets` seeds from
  `flux_credentials::provider_env_keys()` (anthropic/openai/openrouter + `AWS_SECRET_ACCESS_KEY`/
  `AWS_SESSION_TOKEN` — the Bedrock chain materializes those into env, so an `env` dump in tool
  output is now scrubbed too) + `FLUX_SECRET`; the `app run` path previously seeded NOTHING and now
  shares the same helper + redactor. 2 new tests (unit seeding + app-level scrub through
  `build_executor`'s envelope). Drive-by: fixed a pre-existing `HOME`-mutation race between two
  flux-credentials tests (shared `HOME_LOCK`).

## Notes
- Files: crates/flux-app/src/secrets.rs + app.rs, crates/flux-cli/src/main.rs (:1443, :4206 path),
  crates/flux-credentials/src/lib.rs.
- Precedent: `RedactorSecretSink::register_secret` already does this for host-materialized plugin
  credentials (main.rs:1046-1054).
