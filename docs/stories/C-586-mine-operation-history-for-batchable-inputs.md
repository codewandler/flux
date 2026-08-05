---
id: C-586
title: "Mine Flux operation history for safe array-input candidates"
pillar: Improve
status: backlog
epic: batched-operation-inputs
design: docs/designs/batched-operation-inputs.md
areas: [flux-store, flux-runtime, flux-tools]
note: "content-free SQLite census of repeated call shapes, live schemas and operation semantics; distinguish missing batching from unused batching"
---

# Mine Flux operation history for safe array-input candidates

## Goal

Use real Flux execution structure to find operations that repeatedly receive independent inputs and
could safely accept arrays, without collecting user prompts, arguments, paths or results.

## Acceptance

- [ ] A read-only analysis accepts an explicit EventStore SQLite path and documents which event
      fields it reads. It aggregates operation identity, sequence/turn correlation and schema/effect
      metadata only; prompts, arguments, subjects, results, paths, secrets and session text never
      enter report output or committed fixtures.
- [ ] The census reports total calls, turns with repeats, excess same-turn calls, immediately adjacent
      repeats and maximum burst, with a minimum sample threshold and store/corpus identity that does
      not identify a user or session.
- [ ] Every frequent candidate is joined to the live operation schema and classified as: already
      array-capable/batch companion exists, array input may preserve semantics, concurrency addresses
      latency only, composite-domain operation preferred, or unsafe because effect/approval/order/
      partial-failure semantics differ.
- [ ] The first report explicitly evaluates at least `git_diff`, `read`/`read_many`, `grep`, `glob`,
      `file_stat`, `path_exists`, `git_log`, `sqlite_query` and the most frequent read-only plugin
      operations present above threshold. Absence or redaction is reported as unknown, never zero.
- [ ] Candidate review verifies bounds, permission-subject expansion, backend request shape, stable
      result correlation and output amplification. No mutation receives an array merely because it
      was repeated.
- [ ] Each accepted candidate gets its own atomic implementation story with failing-first evidence;
      rejected candidates record the semantic reason, and rerunning the audit produces a bounded
      deterministic aggregate report rather than rewriting operation schemas.

## Progress

- 2026-08-05 — initial content-free local census: 64,261 observation events; `git_diff` appeared 143
  times, including 84 immediately repeated calls and 98 excess same-turn calls across 30 turns.
  `read` showed far more repeats despite already accepting arrays, demonstrating why the audit must
  inspect live schemas rather than equate repetition with a missing parameter.

## Notes

- The counts are dated trigger evidence, not a universal benchmark. The implementation must support
  an explicitly selected store and privacy-preserving aggregate output.

