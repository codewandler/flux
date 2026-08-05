---
id: C-519
title: "Extract one shared cross-harness usage timeline"
pillar: Core
status: backlog
epic: usage-observatory
note: "Make flux usage and the future observatory consume one read-only timeline instead of copying CLI parsers into the TUI"
---

# Extract one shared cross-harness usage timeline

## Goal

Provide one reusable, read-only timeline model for Flux, Codex, Claude Code, and opencode usage, then
make `flux usage` consume it without changing its current accounting output. This establishes the data
boundary the observatory can use without depending on `flux-cli` or inventing another parser set.

## Acceptance

- [ ] A failing-first contract test named `shared_timeline_covers_every_discovered_harness` exercises
      all four variants exposed by the existing `HarnessKind` discovery contract and proves the shared
      extraction returns usage-bearing records for each.
- [ ] The shared record carries source harness, session, raw model, event or bucket time plus its
      precision, all available `Usage` tiers, call count, and source cost facts; absence stays explicit
      rather than becoming a zero or fabricated value.
- [ ] Existing Codex, Claude Code, and opencode acquisition delegates to the current adapters. No
      parser is copied into the shared model or `flux-tui`, and the shared crate remains below both
      `flux-cli` and `flux-tui` in the repository layer map.
- [ ] `flux usage` is refactored to consume the shared model. A failing-first parity test named
      `shared_timeline_preserves_flux_usage_output` pins its current filtering, totals, per-model rows,
      call counts, read-only discovery limits, and empty/error behavior before the private fold is
      removed.
- [ ] The API supports bounded time-range reads without loading prompt text, assistant text, tool
      arguments, or transcript bodies; a sentinel fixture test named
      `usage_timeline_reads_metadata_only` proves those fields are never requested merely to build the
      timeline.

## Progress

- (not started)

## Notes

- First child of [C-518](C-518-usage-observatory-epic.md); C-520 consumes this record and adds truthful
  provider/model attribution and pricing semantics.
- Existing source seams are summarized in the epic: `crates/flux-cli/src/usage.rs`,
  `crates/flux-capabilities/src/harness/mod.rs`, and `crates/flux-events/src/kind.rs`.
