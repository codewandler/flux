---
id: C-550
title: "A federated workspace board schedules authoritative cross-repository stories"
pillar: Core
status: done
epic: first-class-board
design: docs/designs/native-board-fleet-cli.md
areas: [flux-capabilities, flux-cli, flux-config]
note: "workspace planning is an index of BoardRefs, never a second copy of repo-local Track state"
---

# A federated workspace board schedules authoritative cross-repository stories

## Goal

Let one Flux workspace query, validate and schedule several repository planning boards while routing
all authority and mutations back to the owning repository.

## Acceptance

- [x] Failing-first test federates two repositories that both contain `C-1`; listing returns distinct
      namespaced references and no copied story files or shadow status database is created.
- [x] Read/query/graph/next compute a deterministic union, resolve cross-repository dependencies and
      reject missing references and cycles with concrete paths. Readiness uses authoritative member
      revisions.
- [x] Create/update/transition require a target member and route through its backend, repository
      confinement and concrete permission subject. A workspace-wide mutation subject is refused.
- [x] The workspace board can own its own revisioned vision, roadmap, decisions and designs while
      federating member items. Tests prove these documents do not shadow or copy member documents,
      and cross-repository links resolve through stable `BoardRef`s.
- [x] `.flux/fleet.toml` declares repository ids, roots, canonical refs, member boards, gates, fences,
      concurrency and tranche/wave groupings with closed validation and source-labelled diagnostics.
- [x] `flux fleet refresh`, `validate`, `schedule` and `status` reproduce the roadmap fixtures for
      canonical ref refresh, exactly one active tranche, dependency order, ten-item wave cap and
      repo-local live story status; all support C-547 JSON output.
- [x] Dirty primary checkouts and stale/diverged worktrees are reported without modification. No
      implicit fetch, cleanup or mutation occurs in a read command.
- [x] Same-repository and cross-repository dependency cycles, duplicate repository ids, overlapping
      roots, absent boards and ambiguous selectors have failing-first tests.
- [x] Targeted config/capability/CLI tests pass; the final board wave owns the full gate.

## Notes

- Depends on C-549 for Track members and A-134 for concrete `BoardRef` routing.
- 2026-08-05 corrective audit: the delivered workspace path remained coupled to Fleet config and
  did not carry the real program schedule. C-588 owns the independent Board configuration and actual
  roadmap adoption without rewriting this historical completion record.
