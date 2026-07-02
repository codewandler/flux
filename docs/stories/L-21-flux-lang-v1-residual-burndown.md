---
id: L-21
title: flux-lang v1 hardening — residual burn-down (resume key, denial fatality, analyzer gaps)
pillar: Language
status: ready
priority: 4
epic: flux-lang-v1-hardening
design: docs/designs/flux-lang-v1-hardening.md
note: the four residuals the epic recorded at close — named-flow resume still checkpoints hash-only through the engine, policy denial is retryable because it's an in-band string, three eval_arg positions still accept calls, and type_check_body diagnostics lack node paths
---

# flux-lang v1 hardening — residual burn-down

## Goal
Close the four residuals `docs/designs/flux-lang-v1-hardening.md` recorded at the epic's close
(2026-07-02), so the epic's guarantees hold on every path, not just the primary ones.

## Acceptance
- [ ] **Named-flow resume checkpoint key.** The engine's `resume_suspended`
      (`crates/flux-flow/src/engine.rs`) persists only body+node and resumes via the *unnamed*
      path, so a **named** flow resumed through the engine derives its checkpoint `flow_key`
      hash-only — run and resume disagree for named flows. Thread the flow name through
      suspension persistence into `flux_lang::resume_flow_named`. Failing-first test:
      `named_flow_resume_uses_the_same_checkpoint_key_as_the_run`.
- [ ] **Policy denial is fatal.** A policy denial surfaces as an in-band `OpOutcome::is_error`
      string, so `FlowError::is_fatal()` (`crates/flux-lang/src/error.rs`) cannot represent it and
      a denied op inside `loop`/`retry`/composite is *retried*. Give denial a typed, fatality-
      surviving representation (the same class as the denied-`confirm` fix from L-17).
      Failing-first test: `policy_denied_op_is_not_retried_inside_loop`.
- [ ] **Analyzer expression-position gaps.** `each` source, `jq` input, and `parse` value are
      `eval_arg` positions the analyzer still accepts `call` nodes in (the runtime rejects them).
      Failing-first test: `call_in_each_source_is_a_diagnostic` (+ jq/parse siblings).
- [ ] **`type_check_body` node paths.** Its diagnostics carry no JSON-pointer node paths while
      `analyze_flow`'s do. Align them. Failing-first test asserts a path like `body[1]` in a type
      diagnostic.
- [ ] Gate green: `cargo test --workspace`, clippy `-D warnings`, fmt, `cargo test -p flux-codegate`.

## Progress
- (not started — filed 2026-07-02 from the epic design's Residuals section.)

## Notes
- `{{sym}}` definedness inside `Fmt` templates stays **out of scope** — the epic explicitly
  deferred it for false-positive risk (recorded in L-15); do not sneak it in.
- Touches flux-lang + flux-flow; the epic's tests are the regression floor.
