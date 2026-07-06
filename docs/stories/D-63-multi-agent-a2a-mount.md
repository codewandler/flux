---
id: D-63
title: Multi-agent A2A mount in flux-server (resolver-keyed)
pillar: Agent
status: backlog
note: "design-first (2026-07-06 downstream-consumer review): flux-server's a2a surface is single-agent, so the consumer serves N agents itself and thereby forgoes flux's session-TTL/SSE machinery"
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
- [ ] Design doc answering the open questions; C-18/C-29 invariants re-stated under the composite key.
- [ ] Implementation story split out after design review.

## Progress
- 2026-07-06 filed (design-first) from the downstream-consumer review.
