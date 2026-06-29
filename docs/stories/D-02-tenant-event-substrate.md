---
id: D-02
title: Tenant/context-taggable event substrate for downstream run persistence
pillar: Core
status: backlog
priority:
theme: downstream-managed-agents
---

# Tenant/context-taggable event substrate for downstream run persistence

## Goal
Make `flux-events` carry an account/agent **context** on appended events and expose an account-scoped
**projection read API**, so a downstream multi-tenant service can persist and replay runs as projections
over flux's append-only log — not a parallel store bolted on beside it.

## Why (managed-agents)
managed-agents **R-04** (run persistence) and its **M4 transparency** surface are designed to be *projections
over `flux-events`* (their `docs/designs/transparency.md`: "flux is already event-sourced … we build on
that substrate, not a new one"). That is a **"build it in, not on"** decision: the context envelope must
exist while R-01 lands, or audit/transparency becomes an expensive retrofit over an untagged log.

## flux gap
`crates/flux-events` `EventStore` is keyed by `stream` (a session id) with conversation / run-trace /
turn-metrics projections (`conversation`, `run_trace`, `turns`), but:
- events carry **no account / agent-id+version** tag — streams are anonymous sessions;
- the read surface (`list(limit)`, `latest_session`) is **global**, not account-scoped;
- there is no documented way for an external consumer to fold the log into per-account transcripts.

## Acceptance
- [ ] Appended events can carry a context envelope (account, agent id+version, conversation/correlation
      id) — additive, optional, no behaviour change for the single-tenant CLI. Failing-first test:
      append with a context, read it back on the `StoredEvent`.
- [ ] An account-scoped projection read API (list runs / fetch a transcript for an account) returns
      **only** that account's streams. Failing-first test: two accounts' runs stay isolated.
- [ ] A short doc shows a downstream service consuming flux-events as the run-persistence substrate.
- [ ] Existing projections and the single-tenant path are unchanged; full gate green.

## Progress
- Backlog. No design doc yet (the shape is small + additive; promote to a design doc if the context
  envelope turns out to touch the projection schema non-trivially).

## Notes
- Touch points: `EventStore::append` / `append_batch`, `create_session`, `list`, `SessionSummary`, and
  the projections in `crates/flux-events/src/projection.rs`.
- Serves managed-agents story **R-04** (run persistence) and the M4 transparency surface (**E-05**).
- Decide early **because** it is cheap now and a migration later; the value is the timing, not the size.
