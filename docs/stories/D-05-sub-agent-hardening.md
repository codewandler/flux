---
id: D-05
title: Harden the sub-agent primitive for multi-tenant production
pillar: Agent
status: done
priority:
theme: downstream-managed-services
design: docs/designs/sub-agent-hardening.md
---

# Harden the sub-agent primitive for multi-tenant production

## Goal
Take the sub-agent primitive (`flux-orchestrate`: `LocalSpawner` + `task` tool + `Role`) from
"works in the CLI and the self-improvement loop" to "safe for a multi-tenant service to consume through
`flux-sdk`." Close the five gaps between today's single-tenant, fire-and-forget primitive and the bar
downstream multi-tenant consumers need: a consumable SDK seam, lifecycle limits, a pluggable approver + tested
account isolation, child activity in the tenant audit log, and the ergonomics a programmatic consumer
needs.

## Why (downstream managed services)
Sub-agent support is the platform primitive that builder-style control-plane sub-agents are
the first consumer of. Its acceptance — invoke a named sub-agent from a flow, child tool calls through the
**same `Executor` envelope and account scope**, child **cannot exceed the parent's account/authorization**
(failing-first isolation test), **built on flux's primitive, not re-implemented** — cannot be met by the
primitive as it stands. Both roadmaps mark it "ready to consume"; it is ready at the primitive level only.

## flux gap
Verified in [`crates/flux-orchestrate/src/lib.rs`](../../crates/flux-orchestrate/src/lib.rs) and
[`flux-cli/src/main.rs`](../../crates/flux-cli/src/main.rs):
- **No SDK seam.** Sub-agents are assembled only in the CLI (`load_roles` → `LocalSpawner::new` →
  `register(TaskTool)` → `with_spawner`, ~140 lines). `flux-sdk` exposes nothing — a consumer must
  re-implement the wiring the R-03 criterion forbids.
- **No lifecycle control.** The `task` tool **discards the parent's cancellation** (orphan token, `:379`);
  there is **no wall-clock timeout**; `max_iterations: 30` is hardcoded with no setter (`:90`). A hung or
  runaway child can't be stopped — a DoS/cost surface for a tenant-facing service.
- **Approver is hardcoded** to `SubAgentApprover` (`:135`), which auto-approves anything non-destructive —
  wrong for the builder, whose CRUD mutations must be approval-gated by the envelope.
- **Account scope is not first-class** and **untested** — `auth` is fixed at construction (`:97`), no
  isolation test exists.
- **Child activity is lost.** Each spawn uses a **throwaway `EventStore::in_memory()`** (`:150`); the
  child's inner tool calls never reach the tenant log, undercutting R-04 / M4 transparency.

## Acceptance
- [x] **WS1 — SDK seam.** `flux-sdk` exposes `FlowClient::with_sub_agents(...)` backed by one reusable
      `flux-orchestrate` assembly (`SubAgents::into_spawner`); the CLI is refactored onto the same helper
      (unchanged behaviour). Hermetic `crates/flux-sdk/examples/sub_agent.rs` (mock, no API key) builds a
      `FlowClient` with an **in-memory** role and drives `task` end-to-end — no manual `Executor`/
      `ToolContext` plumbing.
- [x] **WS2 — lifecycle limits.** Parent cancellation threads into the child (`ToolContext.cancel`,
      additive, engine-installed per turn; `task` hands the child a child-token); configurable
      `SpawnLimits { max_iterations, max_tokens, wall_clock }`, the **wall-clock deadline firing the child's
      cancel token** (cooperative valid-history termination, not a future-drop). Tests:
      `wall_clock_deadline_aborts_a_hung_sub_agent`, `parent_cancellation_propagates_to_the_sub_agent`
      (a hung child via a pending provider; the orchestrate-level analogue of the `FLUX_MOCK_HANG` path).
- [x] **WS3 — pluggable approver + isolation.** `LocalSpawner::with_approver` (default
      `SubAgentApprover`). Tests: `injected_approver_governs_the_sub_agent` (a deny-all approver blocks a
      child call the default would allow) and `sub_agent_is_confined_to_the_parent_workspace` (guarded-IO
      confinement, surface (a)). Surface (b) — policy/caller-scope on custom tools — rides the existing
      inherited-policy path (`sub_agent_refuses_destructive_command` runs under `with_authorization`); a
      dedicated account-tagged-subject test is a thin follow-up (see Progress).
- [~] **WS4 — child audit.** `LocalSpawner::with_audit(Arc<EventStore>)` makes the child run (and its
      inner tool calls) persist into the shared `EventStore` — the flow store now shares it
      (`in_memory_with_events`). Tests: `audit_store_captures_child_run_events` /
      `without_audit_the_shared_store_is_untouched` (regression). **Deferred to [D-02]:** the explicit
      parent-session-id link (`create_child_session(model, parent)`) — D-02 owns session-entry tagging, so
      the link lands with the tag rather than as a churny standalone signature change now.
- [x] **WS5 — ergonomics.** In-memory `RoleRegistry::from_roles` + `FromIterator<Role>`; depth-aware guard
      replacing the blunt `remove("task")` (default `max_depth: 1` keeps children leaves; `with_max_depth`
      opt-in). Test: `max_depth_bounds_nested_delegation`. **Structured `task` output deferred** (design
      permits) — see Progress.
- [x] Full gate green (`cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`).

## Progress
- **Done — merged to `main`** (commits `5617944` impl + `4c89134` review pass; reviewed in an isolated
  worktree). Full gate green (clippy `-D warnings`, fmt, codegate layering lint, `cargo test --workspace`).
- Design doc: [`docs/designs/sub-agent-hardening.md`](../designs/sub-agent-hardening.md).
- **Landed:** WS1 (SDK seam + CLI refactor + hermetic example), WS2 (cancel threading + wall-clock-as-
  cancel + `SpawnLimits`), WS3 (injectable approver + workspace-confinement isolation test), WS4 (audit-
  store threading — child runs persist into the shared `EventStore`), WS5 (in-memory roles + depth-aware
  leaf guard). 8 new failing-first tests in `flux-orchestrate`; the existing 10 still pass.
- **Deferred (intentional, noted in Acceptance):**
  - WS4 explicit parent-session-id link (`create_child_session(model, parent)`) → folded into
    [D-02](D-02-tenant-event-substrate.md) so the link + account tag land as one session-entry change.
  - WS5 structured `task` output (`result_schema` param) — design permits deferral; no consumer needs it
    for v1 (builder-style sub-agents return text the caller persists).
  - WS3 surface (b) dedicated account-tagged-subject test (policy inheritance itself is covered).
  - WS2 aggregate token budget (stretch); `max_depth > 1` is implemented + tested but stays opt-in.
- **Publish-closure note:** `flux-sdk` now depends on `flux-orchestrate`, widening the crates.io publish
  closure — fold into [`crates/flux-sdk/PUBLISHING.md`](../../crates/flux-sdk/PUBLISHING.md) before a release.
- **Post-implementation review pass** (independent diff review) → refinements applied: rewrote a
  tautological audit test into an adversarial one-store gate test; tightened the confinement test to
  assert a *workspace-escape* error (not just `is_err`); added `SubAgents::with_max_depth` (the SDK/CLI
  seam can now opt into bounded nesting); a bounded grace backstop on the wall-clock `run.await`; and a
  default 10-min `wall_clock` in `FlowClient::with_sub_agents` (the one-shot SDK path has no other kill
  switch). Two lifecycle gaps **documented, not fixed** (see the design's "Known limitations"):
  parent-turn cancel drops an in-flight child without finalizing it (matters under `with_audit`), and the
  per-turn cancel slot assumes one active turn per engine (a latent served-agent concern, same assumption
  `loop_host.set_turn` already makes).

## Notes
- **Reuse, don't reimplement:** the envelope (`Executor::dispatch`), guarded IO (`flux-system`), `Role`/
  `RoleRegistry` (`insert` already supports in-memory), and the one-loop engine are all reused unchanged.
  The work is additive seams + limits + an injectable approver + an audit-store thread, plus tests.
- **Isolation is composition, not new sandboxing** — see the design's "Multi-tenant isolation model": one
  `LocalSpawner` per request/account scope, bound to that account's policy + caller + workspace.
- **Couples with [D-02](D-02-tenant-event-substrate.md)** (WS4 ships the threading seam; D-02 adds the
  account tag + projections) and **complements [D-01](D-01-flow-input-seeding.md)** (the behaviour runner a
  sub-agent-invoking flow runs on). Serves downstream multi-tenant sub-agent use cases.
- Non-goal: remote/distributed sub-agents (a future `Spawner` impl), a new sandbox boundary, or D-02's
  tagging itself.
