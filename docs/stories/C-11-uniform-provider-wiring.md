---
id: C-11
title: Uniform provider construction across subcommands (lazy for deterministic flows; aws chain everywhere)
pillar: Core
status: ready
priority: 6
note: `flux flow run` refuses to replay a fully deterministic plan without a provider credential (preset --run doesn't); `flux review -m aws` fails "AWS_ACCESS_KEY_ID is not set" because only build_agent materializes the credential chain
---

# Uniform provider construction across subcommands

## Goal
One provider-construction path with two properties, verified missing on 2026-07-01:

1. **Lazy for deterministic execution.** `flux flow run saved-plan.json --yes` with no
   `ANTHROPIC_API_KEY` fails at startup (`error: anthropic provider: auth error: ANTHROPIC_API_KEY
   is not set`) even though the plan contains zero model ops — undermining the README's
   "re-running a plan costs zero extra model calls" repeatability claim on CI/replay boxes that
   rightly have no credentials. With `-m mock` the same plan runs in 30ms, and `flux preset … --run`
   already runs offline without a provider — the eager construction is a `flow run` (and friends)
   inconsistency, not a design constraint. Provider construction should happen on first model-op
   dispatch (or when the analyzed flow references a model op), not unconditionally.
2. **The aws credential chain works on every `-m aws` path.** `flux review --files x -m aws` fails
   with `AWS_ACCESS_KEY_ID is not set` while `flux run -m aws` succeeds — only the async
   `build_agent` path calls `flux_providers::bedrock::materialize_chain_into_env()`
   (`crates/flux-cli/src/main.rs` ~1196); `flux review`'s own wiring (`build_review_sub_agents` →
   `provider_for`) never materializes the chain. Any current or future subcommand that builds a
   provider without `build_agent` silently loses `aws` (and repeats the class of bug for the next
   chain-based provider).

## Acceptance
- [ ] Failing-first: `flux flow run <deterministic plan>` succeeds with ALL provider env scrubbed
      (no ANTHROPIC/OPENAI/AWS keys) — provider construction is deferred until a model op actually
      dispatches; a flow WITH a model op still fails fast with the same clear auth error at
      analysis/first-dispatch.
- [ ] Failing-first: the aws chain materialization lives in the shared provider factory
      (`provider_for`/`build_provider` seam), covered by a test faking the chain via static env —
      `flux review -m aws`, `flux flow run -m aws`, and `flux preset --run -m aws` all construct.
- [ ] A subcommand-matrix test (or table-driven unit test over the factory) pins that every
      provider-building CLI entry point goes through the one factory.
- [ ] Live: `flux review --files <f> -m aws` runs (once L-14 unblocks review itself).

## Progress
- (not started)

## Notes
- Found during the 2026-07-01 harness e2e review.
- Minor related cleanup worth folding in: `flux usage` attribution keys are inconsistent across
  paths (`openai/gpt-5.5` vs bare `gpt-5.5`; bare Bedrock ids vs `aws/…`), which splits totals for
  the same backend — normalize to the canonical `provider/model` spec at record time (C-06 seam).
