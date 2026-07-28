---
id: C-112
title: "@-triggered file-path completion in the composer"
pillar: Core
status: done
priority: P3
design: tui-polish
epic: tui-polish
areas: [flux-tui]
note:
---

# @-triggered file-path completion in the composer

## Goal
The slash-command menu is the only completion in the TUI; typing a workspace path into the
composer is unassisted. Trigger a fuzzy file-path popup on `@` so users can reference
`crates/…/foo.rs` without leaving the TUI to look it up.

## Acceptance
- [x] Typing `@` at a token start opens a completion popup in the slash-menu layout slot;
      subsequent chars fuzzy-filter workspace-relative paths; ↑/↓ select; Tab/Enter insert the
      selected path replacing the `@token`; Esc dismisses — TestBackend test pins popup rows and
      the insertion. `@` mid-word (e.g. an email) does not trigger.
- [x] The file inventory is built OFF the render path: lazily on first `@`, ignore-aware
      (skip `.git`, `target`, `node_modules`, respecting the workspace root = cwd), entry-count
      capped, and cached; staleness across turns is an accepted, documented v1 limitation.
- [x] Fuzzy matching is a pure helper with unit tests pinning the ranking (path-segment prefix >
      substring > subsequence).
- [x] The slash menu is unaffected (its own popup and precedence unchanged).

## Progress
- Done 2026-07-28: `@token`-at-token-start trigger (email-safe, slash menu wins), lazy bounded walk (`workspace_file_inventory`, 20k cap, skips hidden/target/node_modules, session-cached), pure `fuzzy_rank` (segment-prefix > substring > subsequence), popup in the slash-menu slot, Tab/Enter replaces the token, Esc dismisses per-token. Tests: fuzzy_rank_orders_prefix_substring_subsequence, at_token_triggers_only_at_token_start, path_completion_popup_renders_and_inserts, workspace_inventory_is_ignore_aware_and_capped.

## Notes
- Seams: `COMMANDS` `crates/flux-tui/src/lib.rs:164`, `slash_matches` `lib.rs:224`, popup render
  slot `rendering.rs:30`.
- Keep the walk bounded and synchronous-but-cheap, or push it to a blocking task completing into
  state — never block the 62 ms tick.
