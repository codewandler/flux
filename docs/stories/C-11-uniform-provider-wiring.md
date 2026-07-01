---
id: C-11
title: Uniform provider construction across subcommands (lazy for deterministic flows; aws chain everywhere)
pillar: Core
status: done
note: FIXED — build_provider owns the aws chain (sync-callable ensure_aws_chain), so review/-m aws, /model, sub-agent factory all work; flow run + preset --run use a LazyProvider that constructs on the first model call; live-verified: credential-less replay (28ms) + flux review -m aws (full Bedrock report)
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
- [x] `flux flow run <deterministic plan>` succeeds with ALL provider env scrubbed — verified
      live (`env -u ANTHROPIC_API_KEY -u OPENAI_API_KEY -u OPENROUTER_API_KEY flux flow run
      saved-plan.json --yes` → 2 steps, 28ms). `run_draft_ast*` (shared by `flow run` AND
      `preset --run`) builds via `build_agent_lazy` → `LazyProvider`, which constructs on the
      first model call and surfaces the same auth error then. (Correction to this story's
      premise: `preset --run` had the same eager construction — it goes through the same
      `run_draft_ast` path; both are lazy now.)
- [x] The aws chain lives in the ONE factory: `build_provider`'s aws arm calls the sync-callable
      `ensure_aws_chain()` (block_in_place inside the runtime, one-shot runtime outside; no-op
      when `AWS_ACCESS_KEY_ID` is set) — `build_agent`'s special case is deleted. Static-env
      factory test: `provider_factory_constructs_aws_from_static_env` (build_provider +
      provider_for, no network).
- [x] Matrix-by-construction: every provider-building entry point routes through
      `build_provider`/`provider_for` — agentic run + serve (`build_agent_with`), flow run/preset
      (`LazyProvider` → `build_provider`), review (`build_review_sub_agents` → `provider_for`),
      REPL `/model` (`build_provider`), sub-agent factory (`provider_for`) — pinned by the two
      unit tests + the deleted per-caller special case.
- [x] Live: `flux review --files src/stats.py -m aws` → full Bedrock-powered report (5 findings,
      3 reviewers, 0 gaps); `flux run -m aws` eager path re-verified after the refactor.

## Progress
- **DONE (2026-07-02).** `ensure_aws_chain()` (sync seam over `materialize_chain_into_env`) called
  from `build_provider`'s aws arm; `build_agent` special case removed. `LazyProvider`
  (OnceCell-constructed on first `stream`, rewrites `req.model` to the resolved id) +
  `build_agent_lazy` used by `run_draft_ast_with_composites`. 2 unit tests; full gate green;
  live-verified all three surfaces.

## Notes
- Found during the 2026-07-01 harness e2e review.
- Minor related cleanup worth folding in: `flux usage` attribution keys are inconsistent across
  paths (`openai/gpt-5.5` vs bare `gpt-5.5`; bare Bedrock ids vs `aws/…`), which splits totals for
  the same backend — normalize to the canonical `provider/model` spec at record time (C-06 seam).
