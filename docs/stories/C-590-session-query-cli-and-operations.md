---
id: C-590
title: "Query and inspect one session through CLI and agent operations"
pillar: Core
status: ready
priority: 2
epic: session-truth
design: docs/designs/session-truth-and-self-inspection.md
areas: [flux-events, flux-capabilities, flux-cli]
note: "one redacted bounded projection backs --id/--json, flux session inspect and session.list/session.inspect"
---

# Query and inspect one session through CLI and agent operations

## Goal

Replace database spelunking with one stable read-only service that can select an exact session and
explain its causal execution tree to either an operator or an admitted agent.

## Acceptance

- [ ] Failing first, `flux sessions --query s_2013 --json` cannot select by session identity or emit
      JSON, and no agent operation can inspect the actions/children of its own durable session.
- [ ] `SessionQueryService` reads the existing event-store API and supports exact id plus existing
      content/file/time predicates, deterministic ordering, limit/cursor and SQLite/Postgres parity.
- [ ] A bounded detail request selects one id and optional turn/include set and returns identity,
      messages, plans/action batches, operation status/effect/sequence, child lineage, outcome and
      usage with `complete`, typed omission counts and `next_cursor`.
- [ ] `flux sessions --id s_2013 [--json]` performs exact selection and `flux session inspect
      s_2013 [--turn N] [--children] [--json]` renders the same typed projection. Missing/ambiguous
      ids fail clearly; singular `session` help points to plural discovery.
- [ ] `session.list` and `session.inspect` project the exact shared request/response schemas.
      `current` defaults to the caller's session; another id requires the declared read subject and
      never grants resume, replay, fork or mutation authority.
- [ ] Session-related intent surfaces these operations for questions about this conversation, prior
      actions, tool use, sub-agents, transcript and `s_<id>` without surfacing arbitrary foreign
      harness history.
- [ ] Durable redacted views are reused; raw reasoning/system prompts and unredacted operation bodies
      are impossible include fields. Secret-search, truncation, child-tree and large-session tests
      pass with stable JSON fixtures and public docs.

## Progress

- 2026-08-05 — contracted after inspecting `s_2013` required direct read-only SQLite queries because
  `flux sessions` exposed neither `--id` nor JSON/detail output despite the event log already holding
  every required fact.
