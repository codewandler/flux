---
id: L-31
title: "Reject with_tools cap-scope inside a concurrent parallel/race branch"
pillar: Language
status: done
priority: 11
epic: review-hardening
design: docs/designs/review-hardening.md
note: "parallel branches run concurrently against ONE shared executor, so a with_tools/CapScope in a branch mutates one shared cap-scope stack across await points — LIFO pop can drop the wrong branch's guard (op runs under a wider allowlist) or an intersection can empty the scope (spurious FlowError::Denied). Latent: no shipped .flux uses with_tools. Fix by static rejection, like check_await_position"
---

# Reject with_tools cap-scope inside a concurrent parallel/race branch

## Goal
Close a capability-scoping soundness gap by statically rejecting `with_tools`/`CapScope` nested inside a
`parallel`/`race` branch — mirroring the existing `check_await_position` / `check_checkpoint_position` /
return-in-parallel guards for constructs whose semantics don't compose with concurrent shared-state
branches. `parallel` branches run concurrently via `futures::future::join_all`
(`crates/flux-lang/src/runtime.rs:1931`) against the **same** executor, whose `ToolContext::cap_scopes`
is one shared `Arc<Mutex<Vec<Vec<String>>>>` (`flux-runtime/src/lib.rs:231`) and whose flux-flow guard
list is one shared `Mutex<Vec<CapScopeGuard>>` (`flux-flow/src/runtime.rs:53`). Two failure modes when a
branch yields (`Poll::Pending`) mid-scope on real async IO: (a) an intersection with a sibling's
top-of-stack empties the effective scope → spurious `FlowError::Denied` (fails safe, nondeterministic);
(b) a sibling finishing first pops the wrong guard (LIFO across branches) → a wider allowlist is active
and an op executes outside its own declared `with_tools` scope (authorization escape, capped at the outer
scope by narrow-only). The construct is unused today, so static rejection is the right, minimal fix.

## Acceptance
- [x] Failing-first test: an `analyze` test asserting a flow with a `Node::CapScope` inside a
      `Node::Parallel` (or `race`) branch emits a diagnostic. Today it analyzes clean.
- [x] Fix: add a `check_cap_scope_position` guard (alongside `check_await_position` /
      `check_checkpoint_position`) rejecting cap-scope in concurrent position.
- [x] Sequential `with_tools` (outside any concurrent branch) still analyzes and runs unchanged.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded 🟡 **latent** (Opus). Concurrency is real (confirmed, not
  assumed — unlike the refuted park-attribution finding), so the soundness gap is genuine; but `grep
  with_tools **/*.flux` = 0 and the strict-review flow scopes via sub-agents, so it's unreachable from any
  shipped flow. Filed as a static-rejection guard rather than a per-branch cap-scope context (unwarranted
  for an unused construct).
- 2026-07-03 implemented — added `check_cap_scope_position` in `crates/flux-lang/src/analyze.rs`
  (mirrors `check_await_position`/`check_checkpoint_position`: walks each top-level statement's
  subtree, flags it if a `Node::Parallel`/`Node::Race` found there has any branch whose subtree
  contains a `Node::CapScope`) plus its `branch_contains_cap_scope` helper; wired into `analyze_flow`
  right after `check_checkpoint_position`. Failing-first tests (confirmed `Ok` before the fix, `Err`
  after): `cap_scope_inside_parallel_branch_is_rejected`, `cap_scope_inside_race_branch_is_rejected`,
  `sequential_with_tools_outside_parallel_is_unaffected` (proves acceptance item 3, nesting
  `with_tools` in a sequential `when`). Added a "Key invariants" bullet to
  `crates/flux-lang/docs/reference.md` documenting the new `parallel`/`race` restriction (no node-kind
  or doc-comment changes, so the generated node-kind/prelude tables needed no regeneration — confirmed
  via `cargo test -p flux-lang --test skill_in_sync`, all 3 sub-tests green). Gate:
  `cargo test -p flux-lang` (213 lib + 6 integration/doctest, all green), `cargo clippy -p flux-lang
  --all-targets -- -D warnings` (clean), `cargo fmt -p flux-lang --check` (clean).

## Notes
- Evidence: `crates/flux-lang/src/runtime.rs:1921-1931` (concurrent branches, shared executor);
  `crates/flux-runtime/src/lib.rs:231,844-846,935-945,1055` (shared stack, LIFO guard drop, intersect,
  dispatch check); `crates/flux-flow/src/runtime.rs:53,136-141`; `analyze.rs:218,1141-1190,1398` (no
  positional guard today).
- Residual of [L-11](L-11-strict-review-scoped-capabilities.md). Design: [review-hardening](../designs/review-hardening.md).
