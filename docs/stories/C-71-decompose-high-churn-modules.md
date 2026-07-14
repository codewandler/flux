---
id: C-71
title: Decompose high-churn modules without adding crates
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: split responsibility-heavy CLI, A2A, SDK, TUI, plugin-host, and integration modules after seam consolidation
---

# Decompose high-churn modules without adding crates

## Goal

Reduce review cost and accidental cross-feature coupling in the largest high-churn files by
extracting responsibility-focused internal modules while preserving the current crate/layer map.

## Acceptance

- [x] Land as small per-crate refactors after C-67/C-68/C-69 to avoid moving the same code twice;
      every step is behavior/API neutral and independently gate-green.
- [x] `flux-cli` separates argument types, execution assembly, session/REPL, flow/app, A2A, plugin,
      auth, and rendering concerns behind a thin `main` dispatcher.
- [x] `flux-server` A2A blocking/background/SSE paths share one task-run transition/finalization
      kernel; `flux-sdk` plain/composite execution methods share one internal execution kernel.
- [x] `flux-tui` separates durable UI state/history projection, rendering, terminal IO, and controller
      events without widening its public façade.
- [x] `flux-plugin` separates protocol/guest, host capabilities/loading, hooks, and pack installation;
      GitLab (then Slack/Jira if still warranted) separates manifest/schema/client/operation families
      inside its existing binary crate.
- [x] Snapshot/parity tests pin command trees, operation catalogs/manifests, A2A transitions, SDK
      results, and plugin contract fixtures before and after extraction.
- [x] No new product binary or architectural crate is added; `flux-agent`, `flux-flow`, and
      `flux-orchestrate` remain separate and `flux-flow` keeps its compatibility façade.

## Progress

- 2026-07-14 — Extracted responsibility-focused CLI, TUI, plugin host/protocol, server A2A, SDK
  execution, and GitLab/Slack/Jira modules without changing the crate or binary map. Command,
  transition, execution-kernel, manifest/catalog, and plugin contract parity tests pin behavior.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Audit hotspots included `flux-cli/src/main.rs` (~14.5k lines), `flux-lang/src/runtime.rs` (~9.1k),
  `flux-plugin/src/lib.rs` (~6.1k), and `plugins/gitlab/src/main.rs` (~8.5k). Size alone is not the
  criterion; extraction follows responsibility and repeated lifecycle branches.
