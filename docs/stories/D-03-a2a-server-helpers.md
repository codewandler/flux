---
id: D-03
title: Reusable A2A server helpers on the current spec
pillar: Agent
status: backlog
priority:
theme: downstream-managed-agents
---

# Reusable A2A server helpers on the current spec

## Goal
Lift flux-server's inline A2A routes (agent-card + `message/send` + `message/stream` + `tasks/get`) into
a **reusable helper** that binds an `AgentSpec`/`FlowEngine` to those routes, so a downstream service
mounts a spec-conformant A2A endpoint on its own HTTP server instead of hand-rolling one — and so the two
codebases stop drifting on the wire protocol.

## Why (managed-agents)
managed-agents **E-02** wants to thin-wrap flux's A2A server rather than maintain its own. And there is a
**live divergence**: flux cut over (commit `06065f6`) to `message/send` / `message/stream` / `tasks/get`
and **deleted** the draft `tasks/send` / `tasks/sendSubscribe`, but managed-agents' `channel-a2a` still serves
`tasks/send` — it now speaks a dialect flux answers with `-32601 Method not found`. A shared helper fixes
both the duplication and the drift.

## flux gap
The A2A server routes live inline in `crates/flux-server/src/a2a.rs` (`a2a_handler`, `send`, `subscribe`);
nothing exposes them for reuse by a downstream binary. The client side already is reusable
(`flux-a2a::A2aClient`); the server side is not.

## Acceptance
- [x] A reusable server helper (a `flux-a2a` server module) provides the spec-conformant A2A protocol —
      agent-card builder + `message/send` dispatch + `message/stream` frame shaping — for reuse by a
      downstream surface. Delivered as `flux_a2a::server` (see the design note below on shape).
- [x] `flux-server` consumes the helper instead of its inline copy — same behaviour, proving reuse
      (the auth-gate tests still pass; the A2A wire output is unchanged).
- [x] The helper speaks the **current** spec only (no `tasks/send`); round-trip is covered by the
      `flux_a2a::server` unit tests and, end-to-end over HTTP, by managed-agents' `a2a_routing` integration
      test driving `message/send` → a completed `Task` through the shared `dispatch`.
- [x] Gate green (scoped: `flux-a2a` + `flux-server` + the `flux-codegate` layering lint).

## Progress
- **Code-complete + gate-green, pending commit.** Added `flux_a2a::server` — a reusable, **axum-free**
  protocol core (the layering note below is honored): the `A2aTurn` runner seam, `dispatch` (JSON-RPC
  `message/send` → completed `Task`; `-32601`/`-32602`/`-32600` errors), `agent_card(...)`,
  `extract_text`/`extract_context_id`, `rpc_ok`/`rpc_err`, `now_rfc3339`, and `status_update_value` (the
  `message/stream` frame `result`). `flux-server/src/a2a.rs` now consumes these — it keeps its axum
  routes, SSE streaming control-flow, `Collect`/`StreamSink` (`flux_flow::AgentSink`), and its
  session-id-as-task-id behaviour, deleting only the duplicated extraction/card/timestamp/frame logic.
  10 new unit tests in `flux_a2a::server`; `flux-codegate` confirms `flux-a2a` stays **L1** (the only new
  dep is `async-trait`).
- **Shape decision (vs the original framing).** The acceptance first imagined a helper that *mounts the
  full route set (incl. `tasks/get`) for a single engine/spec*. That shape does **not** serve the primary
  consumer: managed-agents (**E-02**) needs **multi-tenant, per-request** engines (per-agent path, per-request
  auth + embed tokens, a fresh engine built after auth), so it must mount its **own** routes over the
  shared protocol — exactly what the axum-free `flux_a2a::server` form enables. flux-server likewise keeps
  its own routes. So the reusable unit is the **protocol core**, not a route-mounter. `tasks/get` is not
  served by flux-server today (stateless/blocking `message/send`), so it stays out of scope here.
- **`tasks/send` drift:** already resolved upstream — managed-agents' `channel-a2a` migrated to `message/send`
  in its E-06; this change keeps it there.

## Notes
- Watch the layering lint: a server helper that needs `axum` must sit at a layer that already allows it
  (likely `flux-server`-adjacent or a feature-gated module), not pulled down into `flux-a2a` (L1) if that
  would cross a boundary — confirm against `flux-codegate` before placing it. **Resolved:** the helper is
  axum-free pure-`Value`/trait logic, so it stays in `flux-a2a` (L1) with no boundary crossing; each
  surface keeps its own axum glue. `flux-codegate` green.
- Serves managed-agents story **E-02** (A2A re-home).
- Cross-repo follow-up (the consuming half, in managed-agents' E-02): `channel-a2a` + the multi-tenant
  `A2aRouter` adopt `flux_a2a::server`.
