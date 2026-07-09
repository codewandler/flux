---
id: L-21
title: flux-lang v1 hardening — residual burn-down (resume key, denial fatality, analyzer gaps)
pillar: Language
status: done
priority:
epic: flux-lang-v1-hardening
design: docs/designs/flux-lang-v1-hardening.md
note: all four closed — suspensions persist flow_name (guarded migration) so named-flow resume checkpoints name+hash like the run; FlowError::Denied is fatal (host-marked via OpOutcome.denied, executor's canonical op-anchored denial shape pinned by test; hook denials stay retryable by design); each/jq/parse eval_arg positions reject calls; type diagnostics carry node paths
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
- **Done (2026-07-02).** All four residuals closed with failing-first tests:
  - **Named-flow resume key:** the suspensions table persists a nullable `flow_name` (guarded
    `ALTER TABLE` migration for pre-existing stores); `save_suspension`/`take_suspension` carry
    it, `resume_flow_with_composites` gained `name: Option<&str>`, and the engine's
    `resume_suspended` threads the persisted name into `resume_flow_named` (re-saving on
    re-suspension). Test proves a resumed named flow fast-forwards past the post-await checkpoint
    with zero re-dispatches (empirically failed with `None` swapped in).
  - **Denial fatality:** new `FlowError::Denied` (fatal). The host marks denials structurally —
    `OpOutcome.denied` set only by the host; the flux-flow `ExecutorHost` classifies via the
    executor's ONE canonical op-anchored denial shape (`` `{op}` denied by {authority} ``, all
    four deny paths), pinned by a live-executor contract test — no substring matching on
    arbitrary prose. A denied op inside `loop`/`retry` dispatches exactly once; `eval_cond`
    propagates denial instead of reading `false` (a denied `until` guard no longer re-prompts
    per iteration); `try/catch` still catches denials (L-17 semantics unchanged). **Hook denials
    stay retryable by design** (arbitrary reason text, possibly transient).
  - **Analyzer eval_arg positions:** `each` source / `jq` input / `parse` value now reject `call`
    nodes (mirroring the runtime's accepted set lit/var/obj/list), with node paths and a
    no-false-positive sibling test.
  - **Type-diagnostic paths:** `type_check_body`/`check_call_types` thread the same path
    accumulator as the structural pass (`body[1].then[0]`, `args[i]`, `branches[i]`, …).
- **Cross-agent fallout (fixed by the orchestrator in this commit):** A-11's reply-parking landed
  mid-flight calling the old `save_suspension`/`resume_flow_with_composites` arities —
  flux-app's park now passes `None`/the persisted name (journeys execute their flow unnamed, so
  run and resume agree on the hash-only key).
- **Residual:** if flux-runtime ever grows a structured `denied` flag on `ToolResult`, replace
  `is_envelope_denial` with it (strictly better marker; noted in code).

## Notes
- `{{sym}}` definedness inside `Fmt` templates stays **out of scope** — the epic explicitly
  deferred it for false-positive risk (recorded in L-15); do not sneak it in.
- Touches flux-lang + flux-flow; the epic's tests are the regression floor.
