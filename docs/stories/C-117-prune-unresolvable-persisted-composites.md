---
id: C-117
title: Prune unresolvable persisted composites at engine assembly instead of failing spawn/startup
pillar: Core
status: ready
priority: 1
design: docs/designs/resilient-composite-loading.md
note: "live repro: one global composite using gitlab/ai.reason ops bricks EVERY sub-agent spawn of EVERY role; same seam can brick top-level startup when a plugin is uninstalled"
---

# Prune unresolvable persisted composites at engine assembly instead of failing spawn/startup

## Goal
A persisted composite op (`~/.flux/flows`, `.flux/flows`, legacy `ops` dirs) that references
operations absent from an engine's registry must be excluded from that engine's catalog with a
visible audit record — never abort engine assembly. This unbricks sub-agent delegation (child
registries are role ∩ cap-scope narrowed and rarely contain plugin/cognition ops) and removes the
matching top-level startup landmine, without weakening any strict validation on live registration.

## Acceptance
- [ ] `DynamicComposites::prune_unresolvable(&self, tools) -> Vec<PrunedComposite>` removes
      unresolvable global/project composites via a fixed point (a pruned callee prunes its callers
      on the next round); per-composite diagnostics are shared with `analyze_composites` through an
      extracted `analyze_one`, and `analyze_composites` itself is byte-for-byte strict as today.
- [ ] `FlowEngine::assemble_with_loop` calls `prune_unresolvable` where it called
      `validate_base(…)?` (`crates/flux-flow/src/engine.rs:304`); `validate_base` is deleted
      (no remaining callers) and the removal is flagged breaking in the CHANGELOG.
- [ ] Failing-first engine test: a temp workspace with `.flux/flows/broken.flux` calling a
      nonexistent op assembles successfully (currently errors `composite validation failed`), and
      the turn emits a `composites.pruned` observation with name/scope/reason.
- [ ] Failing-first orchestrate test (the live repro): a persisted composite requiring ops outside
      a `tools: [read]` role's narrowed registry no longer fails `LocalSpawner::spawn` — the child
      completes its mock turn (modeled on `spawner_runs_a_role_and_returns_text`).
- [ ] Unit tests: unknown-op pruning with surviving valid sibling; transitive A→B fixed-point
      pruning; `active_for_session("")` excludes pruned names.
- [ ] Strictness pinned: `op.register`'s `validate_registration` still rejects a new composite
      naming an unknown op.
- [ ] Docs: `composites.rs` module header states the resolvability policy; CHANGELOG entry under
      this ID; WHATS-NEW `[Unreleased]` Fixed entry in plain language.
- [ ] Full gate green: `cargo build/test/clippy -D warnings/fmt` + `cargo test -p flux-codegate`.

## Progress
- 2026-07-28 — Bug found live: an in-harness sub-agent smoke test failed at spawn for every role
  with `composite validation failed: unknown operation: gitlab.mr.show …; ai.reason …;
  gitlab.mr.update …`. Traced to `~/.flux/flows/mr_update.flux` (global scope) being validated
  all-or-nothing against the child's narrowed registry at `engine.rs:304`. Design doc written
  (see frontmatter); implementation not started.

## Notes
- Root cause chain: `DynamicComposites::load` (`crates/flux-flow/src/composites.rs:76-98`) →
  `validate_base` hard-fail (`composites.rs:100-106`) at `crates/flux-flow/src/engine.rs:304`;
  child registries narrowed at `crates/flux-orchestrate/src/lib.rs:310-322`.
- Design stance and alternatives: [docs/designs/resilient-composite-loading.md](../designs/resilient-composite-loading.md).
  Precedent: `load_flows_dir` already skips malformed files so startup never breaks
  (`composites.rs:249-264`); pruning extends the same policy to resolvability. Pruning only ever
  narrows the catalog — no envelope or authorization change.
- Deliberately unchanged: `validate_registration` (live `op.register` stays strict),
  `validate_agent_loop`, `flow_run` lazy errors, `flow_list` file listing, session-scoped
  composite loading.
- Operator workaround until this ships: move the offending `.flux` file out of `~/.flux/flows/`.
