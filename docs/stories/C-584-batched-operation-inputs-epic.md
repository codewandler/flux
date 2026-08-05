---
id: C-584
title: "Batched operation inputs — fewer calls for repeatable observations (epic)"
pillar: Core
status: backlog
epic: batched-operation-inputs
design: docs/designs/batched-operation-inputs.md
areas: [flux-tools, flux-runtime, flux-cli]
note: "EPIC — let measured read-only operations accept bounded arrays when one call preserves semantics; never batch writes by schema accident"
---

# Batched operation inputs — fewer calls for repeatable observations

## Goal

Reduce redundant agent tool calls and transcript envelopes by letting operations accept several
independent inputs when one bounded invocation preserves authorization, effects and result meaning.

## Acceptance

- [ ] C-585 lets one `git_diff` call inspect several paths while preserving the singular contract,
      exact permission subjects, fixed safe argv and staged/unstaged behavior.
- [ ] C-586 measures repeated same-operation call shapes from Flux SQLite event history without
      exporting content, distinguishes missing array support from unused existing batch support and
      files only evidenced follow-up stories.
- [ ] A shared review checklist covers bounds, compatibility, subject derivation, deterministic
      correlation, failure semantics, output limits and observational equivalence before any
      operation gains an array input.
- [ ] Mutation operations are excluded unless a separate story defines atomicity, approval and
      partial failure; the epic introduces no generic dispatcher bypass or universal array wrapper.
- [ ] Agent guidance/live schemas make shipped batch shapes discoverable, and evaluation compares
      tool-call count, transcript bytes and wall time without claiming savings from unsupported data.

## Progress

- 2026-08-05 — epic and first two stories filed from the `git_diff` request and a content-free
  structural event-store census.

## Notes

- Related but not duplicate: C-528 executes several independent calls concurrently. This epic asks
  when they should have been one operation call in the first place.
