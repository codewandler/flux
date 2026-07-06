---
id: D-60
title: Provider/lang ergonomics batch — null/static providers, bedrock haiku override, OPENAI_KEY alias, cap-scope wrap
pillar: Agent
status: done
epic: consumer-gaps
note: "from the 2026-07-06 downstream-consumer review: four small thin-wrapper/copy sites in the consumer exist only because flux lacks the one-liner — batch them"
---

# Provider/lang ergonomics batch

## Goal
Four small additive ergonomics so consumers stop copying flux code:

1. **Null + static providers exported** — "a provider that never generates" (deterministic preset
   flows run one in production downstream) and "a provider that returns fixed text" (key-free paths,
   tests). flux itself re-hand-rolls a MockProvider in nearly every test file.
2. **Bedrock haiku alias override** — `resolve_model` hardcodes the haiku alias to a `global.`
   cross-region profile (`crates/flux-providers/src/bedrock.rs:732-745`); IAM setups without global
   inference access must copy the whole alias table just to keep haiku regional. Make the haiku
   profile region-aware/overridable so the alias table stays flux's.
3. **`OPENAI_KEY` fallback** — `openai_from_env` reads only `OPENAI_API_KEY`
   (`crates/flux-providers/src/openai.rs:667-674`); accept the common `OPENAI_KEY` alias.
4. **DraftAst cap-scope wrap helper** — applying a tool allowlist to an already-parsed flow requires
   hand-wrapping the body in `Node::CapScope`; the DSL has `ScopeBuilder::with_tools`
   (`crates/flux-lang/src/dsl.rs:737`) but nothing for an existing `DraftAst`.

## Acceptance
- [x] `NullProvider` (empty stream, never generates) and a fixed-reply static provider exported from
      flux-provider (own module; names/API at implementer's discretion, doc'd for the key-free +
      deterministic use cases); at least one in-repo test switches to them as dogfood.
- [x] Bedrock: haiku alias no longer unconditionally `global.` — overridable (env or builder,
      matching how bedrock config is already customized) with the current behavior as default;
      test pins both paths.
- [x] `openai_from_env` accepts `OPENAI_KEY` when `OPENAI_API_KEY` is absent (precedence pinned by
      test).
- [x] flux-lang: helper wrapping a `DraftAst` body in a `CapScope{tools}` (placement/name matching
      the crate's AST-helper conventions); test proves scoped execution honors the allowlist.
- [x] Full gate green; consumer-compat `cargo check` clean (all additive).

## Progress
- 2026-07-06 filed from the consumer review.
- 2026-07-07 implemented all four items:
  - `flux-provider::static_providers` (new module): `NullProvider` (zero-chunk immediate-complete
    stream) + `StaticProvider::new(text)` (fixed `TextDelta`+`Block`+`Done{EndTurn}` reply), both
    re-exported from the crate root; dogfooded into `flux-orchestrate`'s
    `spawner_runs_a_role_and_returns_text` test in place of its hand-rolled fixed-text `MockProvider`.
  - `bedrock::resolve_model`'s haiku arm now goes through a new `haiku_profile_prefix()` reading
    `FLUX_BEDROCK_HAIKU_PROFILE` (default `"global"`, matching prior behavior); sonnet/opus
    untouched. Two tests pin default + override.
  - `openai_from_env`'s key resolution split into `openai_key_from_env()` (`OPENAI_API_KEY` then
    `OPENAI_KEY`, first wins) so precedence is directly unit-tested; three precedence tests +
    one end-to-end test, guarded by a `bedrock`-style `ENV_LOCK`/`env_guard` mutex.
  - `DraftAst::scoped(tools)` in `flux-lang/src/ast.rs` (consuming builder method, right after the
    struct) wraps `body` in `Node::CapScope`. Proven via a new flux-lang-only `execute_flow` test
    with a minimal cap-scope-enforcing `OpHost` mock (mirrors `flux_runtime::Executor`'s narrow-on-
    push/deny-at-dispatch semantics): an op inside the allowlist runs, one outside is denied
    (`FlowError::Denied`).
  - Gate: `cargo build --workspace`, `cargo test --workspace` (all green), `cargo clippy --workspace
    --all-targets -- -D warnings` (clean), `cargo fmt --check` (root + `plugins/`, clean).
    Consumer-compat: `cargo check --workspace` in the downstream consumer repo stays clean (path-dep
    picks up the change; additive only, no consumer edits).

## Notes
- Adoption story in the consumer's repo follows: delete its provider wrappers/copies and its own
  scope helper.
