---
id: C-155
title: Advertise tool-card expansion with a marker in the header
pillar: Core
status: done
epic: tui-polish-round-2
design:
note: "per-card expansion ALREADY exists (tool.expanded, lib.rs:1831; toggle at lib.rs:1741) — the gap is purely discoverability: the header is `→ verb arg … badge` with no marker (lib.rs:647-672), while Thinking advertises `Ctrl-E details` (lib.rs:1408)"
---

# Advertise tool-card expansion with a marker in the header

## Goal
Tool cards can be expanded per card (`tool.expanded`, toggled at `lib.rs:1741`, honored at
`lib.rs:1831`) and globally via Ctrl-E, but nothing on the card says so: `tool_header_line` renders
`→ verb  arg … badge` (`lib.rs:647-672`) and the summary line is a bare truncated string
(`lib.rs:1823-1826`). By contrast the `Thinking` entry explicitly renders
`thinking · N lines · Ctrl-E details` (`lib.rs:1408`). Add a collapse/expand marker so the detail
is discoverable without reading the help overlay.

## Acceptance
- [ ] A collapsed card with detail available shows a marker (e.g. `▸`, `▾` when expanded), rendered
      through `tool_header_line` so the pad/width math stays shared with the C-109 per-tick badge
      patch — failing-first `transcript_lines` test on a collapsed and an expanded card.
- [ ] The marker is never the header row's last span: the running-badge pairing matches the last
      span against `RUNNING_BADGE` (`lib.rs:1554-1567`), and a test asserts running cards still pair.
- [ ] A card with no expandable detail shows no marker (no false affordance).

## Progress
- 2026-07-29: Implemented. `tool_header_line` grew a `marker: Option<char>` parameter, rendered as
  its own span between the verb and the arg (never last — the badge always closes the row, so the
  C-109 running-badge pairing is unaffected by construction). Added a free-function predicate
  `tool_has_detail(tool: &ToolEntry) -> bool` (checks `format_diff`/`format_detail` for
  non-emptiness; `None` while running) that both call sites use to decide whether to pass a marker
  at all — the per-tick running-badge patch in `transcript_viewport` always passes `None` since a
  running card never has a result yet. `tool_lines` computes `effective_expanded` once and reuses
  it both for the marker (`▸`/`▾`) and the existing expanded-detail branch (was previously computed
  twice inline).
  Failing-first verified: ran the two positive tests before implementing (`collapsed_tool_card_shows_a_collapse_marker`,
  `expanded_tool_card_shows_an_expand_marker`) — both failed for the right reason (header text had
  no `▸`/`▾`); the negative/pairing tests passed trivially before implementation too, as expected
  since no marker existed yet to break anything.
  Tests added (`crates/flux-tui/src/lib.rs`, `mod tests`):
  `collapsed_tool_card_shows_a_collapse_marker`, `expanded_tool_card_shows_an_expand_marker`,
  `tool_card_without_detail_shows_no_marker`, `running_card_still_pairs_with_running_badge`.
  Gate (crate-scoped): `cargo test -p flux-tui` 159 passed / 0 failed; `cargo clippy -p flux-tui
  --all-targets -- -D warnings` clean; `cargo fmt -p flux-tui -- --check` clean.
  Note: worked in a shared tree with five other concurrent agents editing `lib.rs`/`state.rs` in the
  same window (C-149, C-154, C-157 among them) — all edits applied via the Edit tool's
  read-then-match-then-write, no reformatting or reverting of anyone else's code. `tool_header_line`
  did not yet grow the C-149 gutter-glyph column as of this story's completion; the marker span is
  inserted independently of the verb/arg spans so it should compose without a rewrite if that lands
  after.

## Notes
- Correction recorded during review: the original suggestion claimed expansion did not exist. It
  does (C-111); this story is discoverability only and adds no new key binding.
