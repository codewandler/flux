---
id: D-05
title: Harden the sub-agent primitive for multi-tenant production
pillar: Agent
status: backlog
priority:
theme: downstream-managed-agents
design: docs/designs/sub-agent-hardening.md
---

# Harden the sub-agent primitive for multi-tenant production

## Goal
Take the sub-agent primitive (`flux-orchestrate`: `LocalSpawner` + `task` tool + `Role`) from
"works in the CLI and the self-improvement loop" to "safe for a multi-tenant service to consume through
`flux-sdk`." Close the five gaps between today's single-tenant, fire-and-forget primitive and the bar
managed-agents **R-03 → A-05** sets: a consumable SDK seam, lifecycle limits, a pluggable approver + tested
account isolation, child activity in the tenant audit log, and the ergonomics a programmatic consumer
needs.

## Why (managed-agents)
managed-agents **R-03** (sub-agent support) is the platform primitive A-05's `managed-agents-builder` sub-agent is
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
- [ ] **WS1 — SDK seam.** `flux-sdk` exposes a `with_sub_agents(...)` builder backed by one reusable
      `flux-orchestrate` assembly; the CLI is refactored onto the same helper (unchanged behaviour).
      Failing-first: a hermetic `crates/flux-sdk/examples/sub_agent.rs` (`mock`, no API key) builds a
      `FlowClient` with an in-memory role and drives `task` end-to-end — no manual `Executor`/`ToolContext`
      plumbing.
- [ ] **WS2 — lifecycle limits.** Parent cancellation threads into the child (`ToolContext.cancel`,
      additive); configurable `SpawnLimits { max_iterations, max_tokens, wall_clock }`, with the
      **wall-clock deadline firing the child's cancel token** (cooperative valid-history termination, not a
      future-drop). Failing-first: a `FLUX_MOCK_HANG` child is cancelled when the parent turn is; a
      `wall_clock` deadline aborts a slow child with a typed error and a **valid session history** (no split
      tool_use/tool_result pair). Sequence WS2 before WS4 (shared store needs clean termination).
- [ ] **WS3 — pluggable approver + isolation.** `LocalSpawner::with_approver` (default
      `SubAgentApprover`). Failing-first, **both enforcement surfaces**: an injected approver that denies a
      tagged tool is honoured inside a child; (a) an account-A-scoped spawner's child **cannot read
      account-B's workspace** (guarded-IO confinement) and (b) a child holding an account-scoped custom tool
      is **denied** a cross-account target (policy/caller-scope — the surface the builder relies on).
- [ ] **WS4 — child audit.** `audit: Some(store)` makes child sessions + inner `tool_call` events land in
      the parent/tenant `EventStore`, parent-linked via a dedicated `create_child_session(model, parent)`
      (no churn to existing `create_session` callers). Failing-first: events present with `Some`, store
      untouched with `None` (regression guard). The account/agent tag + the unified session entry ride
      [D-02](D-02-tenant-event-substrate.md) (coordinate the entry shape).
- [ ] **WS5 — ergonomics.** In-memory `RoleRegistry::from_iter`; depth-aware guard replacing the blunt
      `remove("task")` (default `max_depth: 1` keeps children as leaves). Optional structured `task` output
      ships with a test or is explicitly deferred in Progress.
- [ ] Full gate green (`cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`).

## Progress
- Backlog. Design doc written: [`docs/designs/sub-agent-hardening.md`](../designs/sub-agent-hardening.md)
  (grounded in a code audit of `flux-orchestrate` + a cross-repo audit of managed-agents R-03/A-05).
- Recommended slice order: WS1 → WS2 → WS3 → WS4 → WS5 (each independently committable with its own test).
- Awaiting design sign-off before implementation.

## Notes
- **Reuse, don't reimplement:** the envelope (`Executor::dispatch`), guarded IO (`flux-system`), `Role`/
  `RoleRegistry` (`insert` already supports in-memory), and the one-loop engine are all reused unchanged.
  The work is additive seams + limits + an injectable approver + an audit-store thread, plus tests.
- **Isolation is composition, not new sandboxing** — see the design's "Multi-tenant isolation model": one
  `LocalSpawner` per request/account scope, bound to that account's policy + caller + workspace.
- **Couples with [D-02](D-02-tenant-event-substrate.md)** (WS4 ships the threading seam; D-02 adds the
  account tag + projections) and **complements [D-01](D-01-flow-input-seeding.md)** (the behaviour runner a
  sub-agent-invoking flow runs on). Serves managed-agents **R-03** + **A-05**.
- Non-goal: remote/distributed sub-agents (a future `Spawner` impl), a new sandbox boundary, or D-02's
  tagging itself.
