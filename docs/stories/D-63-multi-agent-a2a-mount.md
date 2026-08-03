---
id: D-63
title: Multi-agent A2A mount in flux-server (resolver-keyed)
pillar: Agent
status: done
design: docs/designs/multi-agent-a2a-mount.md
note: "shipped (Unreleased): router_multi + AgentResolver/StaticResolver; auth stays one layer (answers its own + D-64's open question)"
---

# Multi-agent A2A mount in flux-server

## Goal
Generalize flux-server's A2A surface from one fixed agent to **N agents keyed by path**, resolved
per-request (`Fn(agent_id, headers) -> (AgentCard, impl A2aTurn)`), so multi-tenant consumers get
flux's session lifecycle instead of rebuilding the mount.

## Why (evidence)
`flux-server/src/a2a.rs` serves exactly one `CardInfo` (`crates/flux-server/src/lib.rs:48`) at fixed
routes — but already owns the hard parts a multi-agent mount needs: A2A session TTL retention
(`create_a2a_session`, `prune_expired_a2a_sessions_at` — C-18), `message/stream` SSE (`subscribe`),
and the C-29 queued-session mint-order fix. The reviewed downstream consumer therefore built its
own mount (`/:agent_id/.well-known/agent-card.json` + `/:agent_id/a2a` via a per-request resolver +
auth) — and in doing so **forgoes** the TTL/SSE machinery entirely, running stateless turns.

## Design sketch (to be developed before implementation)
- Resolver seam: `AgentResolver` trait (async `resolve(agent_id, headers) -> Option<(CardInfo,
  Arc<dyn A2aTurn>)>`); the current single-agent surface becomes the trivial resolver.
- Session TTL/queue maps become keyed by (agent_id, session) — audit C-18/C-29 invariants under the
  new key.
- Open questions: auth injection point (resolver-owned like the consumer's, or a separate layer),
  card-URL derivation per mount (use `card_url` helper from D-57), whether streaming sessions pin
  the resolved agent for their lifetime (they should — re-resolution mid-stream is a tenancy hazard).

## Acceptance
- [x] Design doc answering the open questions. → [multi-agent-a2a-mount](../designs/multi-agent-a2a-mount.md)
      C-18/C-29 hold unchanged: each agent owns its own engine+store, so per-agent isolation is
      structural — no composite `(agent_id, session)` key is needed (the engine IS the key), and the
      existing TTL/mint-order logic runs per engine verbatim.
- [x] Implemented directly (not split): `AgentResolver`/`ResolvedAgent`/`StaticResolver` +
      `router_multi` in flux-server, reusing the existing `send`/`subscribe`/`create_a2a_session`.

## Progress
- 2026-07-06 filed (design-first) from the downstream-consumer review.
- 2026-07-07 DONE. Scoped to the A2A protocol surface the consumer actually hand-rolled (card +
  `/:agent_id/a2a`), giving them flux's TTL/SSE/continuity they were forgoing. Open questions
  answered: auth is ONE outer layer (resolver consumes `AuthContext`, never verifies — also answers
  D-64's shared question); card url built per-mount as `<base>/<agent_id>/a2a` (external_url in
  principal mode, host-derived otherwise); streaming pins the resolved engine (owned clone); unknown
  agent → constant 404. Resolver returns `Arc<FlowEngine>` (not `A2aTurn`) because flux-server's
  machinery is engine-based; the story's `impl A2aTurn` sketch didn't match the actual surface.
  6 integration tests (`tests/multi_agent_mount.rs`); full workspace gate green. Per-agent REST
  `/sessions` surface and per-agent turn gate are noted non-goals/follow-ups in the design.
- 2026-07-07 pre-release review: added `serve_multi` and the then-existing native-listener helper;
  C-435 later removed every already-bound listener helper and made `serve_multi` require an explicit
  `ExecutionSystem`. The router still owns the shared Open-non-loopback refusal. Documented the public-card existence-
  oracle caveat and the per-engine sub-agent identity-cell contract on `AgentResolver`/`ResolvedAgent`.
