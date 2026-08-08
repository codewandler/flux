---
id: C-519
title: "Extract one shared cross-harness usage timeline"
pillar: Core
status: done
epic: usage-observatory
note: "Make flux usage and the future observatory consume one read-only timeline instead of copying CLI parsers into the TUI"
---

# Extract one shared cross-harness usage timeline

## Goal

Provide one reusable, read-only timeline model for Flux, Codex, Claude Code, and opencode usage, then
make `flux usage` consume it without changing its current accounting output. This establishes the data
boundary the observatory can use without depending on `flux-cli` or inventing another parser set.

## Acceptance

- [x] A failing-first contract test named `shared_timeline_covers_every_discovered_harness` exercises
      all four variants exposed by the existing `HarnessKind` discovery contract and proves the shared
      extraction returns usage-bearing records for each.
- [x] The shared record carries source harness, session, raw model, event or bucket time plus its
      precision, all available `Usage` tiers, call count, and source cost facts; absence stays explicit
      rather than becoming a zero or fabricated value.
- [x] Flux-native records may link C-575 causal resource receipts (request/result/BoardRef plus
      physical resource coverage). Foreign harnesses that expose only token history remain valid
      partial records and are never assigned invented CPU/network ownership.
- [x] Existing Codex, Claude Code, and opencode acquisition delegates to the current adapters. No
      parser is copied into the shared model or `flux-tui`, and the shared crate remains below both
      `flux-cli` and `flux-tui` in the repository layer map.
- [x] `flux usage` is refactored to consume the shared model. A failing-first parity test named
      `shared_timeline_preserves_flux_usage_output` pins its current filtering, totals, per-model rows,
      call counts, read-only discovery limits, and empty/error behavior before the private fold is
      removed.
- [x] The API supports bounded time-range reads without loading prompt text, assistant text, tool
      arguments, or transcript bodies; a sentinel fixture test named
      `usage_timeline_reads_metadata_only` proves those fields are never requested merely to build the
      timeline.

## Progress

- Acquisition moved down, not copied: `crates/flux-capabilities/src/harness/usage.rs` now owns the
  Flux/Codex/Claude/opencode token-shaped extraction, built on the same `harness::scan` primitives and
  `message.rs` timestamp helpers the message adapters use. `flux-capabilities` is L5 and `flux-cli` /
  `flux-tui` are L6 in `flux-codegate`'s layer map, so the shared crate sits below both.
- `flux usage` keeps only its rendering fold: `collect_external` hands each foreign harness to the
  shared extraction and `flux_dataset_from_store_with_progress` retains just the flux-only efficiency
  projection. `ProgressRenderer` became a `ScanObserver`, so progress is a callback rather than
  something acquisition decides.
- `UsageFact` gained `receipt: Option<ResourceLink>`, built only from a recorded C-575
  `ResourceReceipt` (root id, session, board ref, and the physical dimensions it actually observed).
  `with_receipt` refuses on a non-Flux fact, so a foreign harness cannot acquire CPU/network ownership
  it never measured.
- `UsageWindow` bounds a read while facts are built; `flux usage` passes `UNBOUNDED` and applies its
  own `TimeFilter` afterwards, which is why the accounting output is unchanged.
- Verified before touching anything: the three named acceptance tests
  (`shared_timeline_covers_every_discovered_harness`, `usage_timeline_reads_metadata_only`,
  `shared_timeline_preserves_flux_usage_output`) were already green, so what was left was the one
  acceptance clause no test observed — the receipt link itself.
- `tests/usage_timeline.rs::a_flux_record_links_only_the_receipt_recorded_for_its_own_session` now
  records a real C-575 span through `EventStore::record_resource_span`, reads it back, and proves
  `ResourceLink::from_receipt` keeps only the physical dimensions the receipt *observed* — no model
  tier, and not the network byte count nobody reported. Written failing-first, it caught a defect:
  `with_receipt` guarded the harness but not the session, so a receipt measured under session A
  attached to session B's native fact. It now requires the binding's session to agree; a receipt
  whose binding proved no session stays linkable, because an unproven session is absence, not a
  contradiction.
- Nothing *produces* receipts yet (C-575's own Progress says so, and C-727 owns the instrumentation),
  so acquisition attaches no link on its own. The seam is proven consumer-side against the real
  ledger rather than against a hand-built value.
- Re-dispatched and found finished but *uncommitted*: the whole implementation sat in the story
  worktree as unstaged edits to `harness/mod.rs`, `harness/scan.rs`, `usage_observatory.rs` and
  `flux-cli/src/usage.rs`, plus two untracked files (`harness/usage.rs`, `tests/usage_timeline.rs`).
  It is committed now. Verified before committing: `cargo test -p codewandler-flux-capabilities`
  (132 unit tests plus every integration binary) and `cargo test -p flux-cli --bin flux usage::tests`
  are green, including the three named acceptance tests and
  `a_flux_record_links_only_the_receipt_recorded_for_its_own_session`.


## Notes

- First child of [C-518](C-518-usage-observatory-epic.md); C-520 consumes this record and adds truthful
  provider/model attribution and pricing semantics.
- C-574's result bills are a richer native input, not a replacement for cross-harness discovery.
- Existing source seams are summarized in the epic: `crates/flux-cli/src/usage.rs`,
  `crates/flux-capabilities/src/harness/mod.rs`, and `crates/flux-events/src/kind.rs`.
