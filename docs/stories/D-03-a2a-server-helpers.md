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
- [ ] A reusable server helper (a `flux_sdk::a2a` server module, or a `flux-a2a` server feature) mounts
      agent-card + `message/send` + `message/stream` (SSE) + `tasks/get` for a given engine/spec.
- [ ] `flux-server` consumes the helper instead of its inline copy — same behaviour, proving reuse.
      Failing-first test: the existing flux-server A2A integration test passes against the extracted
      helper.
- [ ] The helper speaks the **current** spec only (no `tasks/send`); a round-trip test drives it with
      `flux-a2a::A2aClient`.
- [ ] Full gate green.

## Progress
- Backlog.

## Notes
- Watch the layering lint: a server helper that needs `axum` must sit at a layer that already allows it
  (likely `flux-server`-adjacent or a feature-gated module), not pulled down into `flux-a2a` (L1) if that
  would cross a boundary — confirm against `flux-codegate` before placing it.
- Serves managed-agents story **E-02** (A2A re-home).
- Cross-repo follow-up (not part of this story): managed-agents `channel-a2a` migrates off `tasks/send` by
  adopting this helper.
