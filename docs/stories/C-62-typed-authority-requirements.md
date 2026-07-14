---
id: C-62
title: Use one typed authority contract for planning and dispatch
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: release blocker — Effect, AccessKind, permission subjects, and semantic tags authorize different realities
---

# Use one typed authority contract for planning and dispatch

## Goal

Replace the split effect/access/semantic metadata with one typed, resource-aware authority contract
that drives plan risk, policy evaluation, approval disclosure, and runtime dispatch identically.

## Acceptance

- [x] A design pins the typed requirement vocabulary for pure operations, filesystem read/write with
      path subjects, datasource read/write with source/entity subjects, network egress, process
      execution, host-state read/write, provider invocation, and semantic actions such as
      `flow.write_db`, `flow.delete`, `flow.money`, and `flow.send_external`.
- [x] `Executor` evaluates those exact requirements; unknown serialized/plugin actions fail closed,
      and no generic `Effect::Read => workspace.read` or `LocalSystem => process.exec` inference
      remains.
- [x] Plan risk/approval previews and dispatch consume the same invocation-level requirements,
      including concrete subjects, rather than separately lowering catalog metadata.
- [x] Failing-first policy tests prove `write_db`, money, delete, and external-send actions can each be
      denied or require approval at dispatch, not merely annotated by the analyzer.
- [x] Failing-first C-58 tests prove sink-backed `web.fetch`/`web.crawl` are denied when
      `flow.write_db` is ungranted and allowed only with the matching datasource subject.
- [x] Plugin integration reads declare and enforce their actual network/connection requirements;
      pure/evidence/datasource/endpoint operations no longer masquerade as workspace reads.
- [x] Registration-time validation rejects inconsistent requirements, and a catalog-wide test pins
      the exact policy requests for representative operations in every resource family.
- [x] Wire compatibility for old plugin manifests has a conservative migration rule, and language,
      operation, plugin-authoring, CHANGELOG, and user-facing approval docs are updated.

## Progress

- 2026-07-14 — Introduced resource-aware `AuthorityRequirement` values shared by planning and
  dispatch, with fail-closed adapters and exact plugin capability scopes. The catalog matrix and
  sink-backed `web.fetch`/`web.crawl` dispatch tests cover semantic and datasource denials.
  Invocation subjects for process, network, browser, and provider resources are preserved as
  trimmed, deduplicated named requirements (wildcard only when no concrete subject exists);
  runtime 90/90 and web 54/54 focused tests pin the exact vectors.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Depends on C-60. Follow-up to [D-138](D-138-semantic-effects-through-catalogs.md) and
  [C-58](C-58-honest-web-record-persistence-effects.md): those stories surface declaration and
  analysis metadata, but dispatch currently does not evaluate semantic actions.
- `endpoint.import` is a useful host-state-write regression: it currently declares `LocalSystem`,
  which the runtime translates into `process.exec`.
