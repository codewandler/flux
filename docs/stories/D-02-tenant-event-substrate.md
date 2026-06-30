---
id: D-02
title: Tenant/context-taggable event substrate for downstream run persistence
pillar: Core
status: done
theme: downstream-managed-services
note: optional stream-level account/agent/correlation context envelope on `flux-events` runs + account-scoped reads (`list_for_account`/`account_streams`) (commit `c97c8a4`)
---

# Tenant/context-taggable event substrate for downstream run persistence

## Goal
Make `flux-events` carry an account/agent **context** on appended events and expose an account-scoped
**projection read API**, so a downstream multi-tenant service can persist and replay runs as projections
over flux's append-only log — not a parallel store bolted on beside it.

## Why (downstream managed services)
Downstream run-persistence and transparency surfaces are designed to be *projections
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
- [x] Appended events can carry a context envelope (account, agent id+version, conversation/correlation
      id) — additive, optional, no behaviour change for the single-tenant CLI. Failing-first test:
      append with a context, read it back on the `StoredEvent`.
- [x] An account-scoped projection read API (list runs / fetch a transcript for an account) returns
      **only** that account's streams. Failing-first test: two accounts' runs stay isolated.
- [x] A short doc shows a downstream service consuming flux-events as the run-persistence substrate.
- [x] Existing projections and the single-tenant path are unchanged; full gate green.

## Progress
- **Code-complete + gate-green (pending commit).** Chosen shape: the context is **stream-level**, not
  per-event — a run's account/agent/correlation is fixed for its whole lifetime, so it lives on the
  `streams` registry (set once at creation) and the `events` table is untouched. This makes account-scoping
  a cheap indexed filter and keeps the hot append path unchanged. (Per-event columns were rejected:
  redundant on every row and not indexable on a registry.)
- **Shipped:** new `flux_events::EventContext { account, agent_id, agent_version, correlation_id }` (all
  optional, `is_empty()`); `EventStore::create_session_with_context` (the 1-arg `create_session` delegates
  with an empty envelope → **zero churn at the 9 call sites**); `context` surfaced on `StoredEvent` /
  `SessionInfo` / `SessionSummary`, stamped once per read from the registry; account-scoped reads
  `list_for_account(account, limit)` + `account_streams(account)`; additive, idempotent column migration
  (`PRAGMA table_info` guard) + `idx_streams_account`. Transcript replay reuses the existing
  `conversation`/`turns` projections (unchanged). Design: `docs/designs/tenant-event-substrate.md`.
- **Verified** in an isolated `HEAD` worktree (the main tree was temporarily un-buildable due to an
  unrelated concurrent `flux-lang` edit): 23 `flux-events` tests + doctest, `clippy --workspace
  --all-targets` clean (no consumer breaks from the new field), `fmt --all`, and the `flux-codegate`
  layering lint (flux-events stays L2, no new deps).
- **Out of scope (follow-ups):** wiring real context values at call sites — the A2A `context_id` (today
  read by `extract_context_id` and only echoed) is the natural `correlation_id` source; persona/
  context-from-file at `flux app run` time is **D-11**. Downstream services consume this for run persistence
  and transparency.

## Notes
- Touch points: `EventStore::append` / `append_batch`, `create_session`, `list`, `SessionSummary`, and
  the projections in `crates/flux-events/src/projection.rs`.
- Serves downstream run-persistence and transparency surfaces.
- Decide early **because** it is cheap now and a migration later; the value is the timing, not the size.
