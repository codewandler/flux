---
id: L-11
title: Strict review — scoped capabilities (with_tools) enforced at dispatch (Phase 2)
pillar: Language
status: done
epic: strict-review-flows
design: docs/designs/strict-review-flows.md
note: analyzer-visible capability-scope node + runtime narrowing threaded into Executor::dispatch
---

# Strict review — scoped capabilities (with_tools) enforced at dispatch (Phase 2)

## Goal

Turn per-block tool restriction from advisory into a **runtime-enforced** guarantee: add an
analyzer-visible capability-scope node (`with_tools` lowering to a `cap_scope` block, or metadata on
`seq`/`parallel`/`each`) and thread the narrowed tool/effect set through `flux-flow` into
`Executor::dispatch`, so a call to a tool outside the active scope fails closed — even when the outer
session policy would allow it. This is the feature that makes strict review not-just-a-skill; it
serves the Language pillar by making capability narrowing a first-class, checkable language construct.

Full design: [docs/designs/strict-review-flows.md](../designs/strict-review-flows.md) — Phase 2 &
"Capability scoping".

## Acceptance

- [x] **Failing-first test:** a flow with `with_tools ["read_many"]` can call `read_many` and a
  `grep` call inside that block is **denied** with a normal policy/capability error — added red,
  then green. (`with_tools_scope_allows_the_named_tool_and_denies_the_rest`,
  `crates/flux-flow/src/runtime.rs`.)
- [x] Capabilities narrow (never widen) as execution descends: session ∩ AgentSpec ∩ flow ∩ block ∩
  sub-agent. A sub-agent invoked with `tools: []` cannot perform filesystem/shell/network IO beyond
  the provider call its role requires. (Pre-existing `subset(Some(&[]))` invariant, plus the new
  block-scope intersection: `spawn_scoped_intersects_the_active_block_scope_with_the_roles_tools`,
  `task_tool_forwards_the_contexts_active_cap_scope_to_the_spawner`,
  `nested_with_tools_cannot_widen_past_the_outer_scope` /
  `nested_scope_narrows_and_never_widens`.)
- [x] Enforcement is in the runtime dispatch path (`Executor::dispatch`), not prompt text. (New
  gate 0, checked first, before pre-tool hooks — `crates/flux-runtime/src/lib.rs`.)
- [x] Capability scope **entry/exit and every denial** appear in the evidence log.
  (`cap_scope_enter`/`cap_scope_denied`/`cap_scope_exit`; tested in both `flux-runtime` and
  `flux-flow`, including ordering.)
- [x] The analyzer sees the scope node so an undeclared tool inside it can be flagged statically where
  possible. (`check_cap_scopes` in `crates/flux-lang/src/analyze.rs`, tested with 4 cases including
  the non-widening-nested case.)
- [x] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.
- [x] CHANGELOG entry.

## Notes
- Open question from the design **resolved**: scopes narrow by **tool name** (not effects — future
  refinement); sub-agent restriction is a **surrounding block scope** intersected into the role's
  `tools`, not a typed `task(tools:)` param. See the design doc's Phase 2 + Open questions.
- Builds on [L-10](L-10-strict-review-example-flow.md) (proves the contract this must enforce).

## Progress

Implemented end to end. Enforcement locus (the crux): `flux-runtime`'s `Executor` grew an
interior-mutable capability-scope stack (`ToolContext::cap_scopes: Arc<Mutex<Vec<Vec<String>>>>`,
mirroring the existing `plan_scope`/`trust_all` pattern) with `push_cap_scope`/`active_cap_scope` +
a `CapScopeGuard` whose `Drop` pops unconditionally. `Executor::dispatch` checks the top of stack as
gate **0**, before pre-tool hooks and the policy/permission layers, on every dispatch — so a composite
op's recursive inner calls and a sub-agent's own dispatches are caught identically to a direct call.
`push_cap_scope` intersects the incoming allowlist with the current top-of-stack, so nesting only ever
narrows.

Language side: a new `Node::CapScope { tools, body, bind }` AST node (native text
`with_tools ["a","b"] { … }`), handled in `ast.rs`, `parse.rs` (`parse_with_tools`, reusing
`parse_setting_list`), `analyze.rs` (walked by `for_each_node`/`check_node`, plus a dedicated
`check_cap_scopes` static pass for the undeclared-tool-in-scope diagnostic), `runtime.rs` (push → run
body → always pop, mirroring `Scope`'s RAII), `format.rs`/`render.rs` (native pretty-printing, so the
plan-preview tree shows the scope), `optimize.rs` (no change needed — its liveness pass already walks
every node kind via `for_each_node`'s exhaustive match), `schema.rs`/`skill.rs` (auto-derived from the
`Node` doc-comment; SKILL.md/reference.md regenerated), `dsl.rs` (`Block::with_tools` builder + DSL
coverage-guard test). The `flux-lang` `OpHost` trait grew `push_cap_scope`/`pop_cap_scope` (default
no-op); `flux-flow`'s `ExecutorHost` overrides them to forward to the real `Executor`, holding the
returned guard in a small stack between the two separate `await` calls (they aren't RAII at the
`OpHost` boundary — the interpreter's `CapScope` node calls push, awaits the body, then always calls
pop). No new `flux-lang → flux-runtime` dependency: the language only knows the `OpHost` trait.

Sub-agent intersection: `Spawner` grew a default-delegating `spawn_scoped(role, task, cancel,
cap_scope)` method (so `flux-eval`'s unrelated `plan_and_dispatch` caller and the 3 test-mock
`Spawner` impls needed zero changes). `TaskTool::execute` reads `ctx.active_cap_scope()` — the same
shared stack `Executor::dispatch` checks — and calls `spawn_scoped`. `LocalSpawner::spawn_with_scope`
intersects `role.tools` with the incoming `cap_scope` before subsetting the child's registry.

Gate: `cargo build --workspace` / `cargo test --workspace` (all green, including a pre-existing
unrelated `flux-config` test-order flake reproduced independently on a clean `main` stash — not
touched) / `cargo clippy --workspace --all-targets -- -D warnings` (clean) / `cargo fmt --all --check`
(clean) / `cargo test -p flux-codegate` (layering intact, no new crate edges). Skill docs regenerated
via `UPDATE=1 cargo test -p flux-lang --test skill_in_sync` and
`UPDATE=1 cargo test -p flux-flow --test skill_docs_in_sync`.
