---
id: C-706
title: "A served agent maps a contextId to its task"
pillar: "Core"
status: ready
epic: remote-agents
areas: [flux-server]
design: docs/designs/tui-attach.md
note: "C-686's review: task id IS the session id server-side and find_correlated maps context to session, but nothing exposes it — so a fresh process can only replay history after sending something"
priority: 4
---

# A served agent maps a contextId to its task

## Goal

Attaching a fresh process to a conversation that already exists cannot replay its history, because
history comes from `tasks/get` and nothing exposes the route from a `contextId` to the task id it
belongs to. The mapping exists server-side — the task id *is* the session id, and `find_correlated`
already maps context to session — it simply has no read-only surface. So C-686's reattach shows an
empty pane with an explicit label until the operator sends a first message, which is honest but
avoidable. Relatedly, `GET /sessions/{id}` returns only `{id, model, created_at_ms}` and carries no
history, so it is not the answer either.

## Acceptance

- [ ] A read-only route resolves a `contextId` to its task id, authenticated exactly as the rest of
      the surface is, and refuses rather than guessing when the context is unknown.
- [ ] `GET /sessions/{id}` either carries history or points at what does, so a client has one
      documented way to recover a conversation it did not start.
- [ ] An attaching client with a `contextId` and no prior turn replays history immediately, and
      C-686's "history unavailable" notice stops firing in that case.
- [ ] The route exposes no session an unauthenticated or differently-scoped caller could not
      already read.
