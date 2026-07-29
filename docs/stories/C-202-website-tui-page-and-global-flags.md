---
id: C-202
title: "The TUI is the daily driver and has one table row on the website"
pillar: Core
status: done
priority: 14
epic: website-truth-and-identity
design: docs/designs/website-truth-and-identity.md
note: "docs/usage.md documents the whole TUI keybinding surface; the public site reduces it to agent/cli.md:31 plus a slash-command table — and --color and --max-tokens have zero site hits"
---

# The TUI is the daily driver and has one table row on the website

## Goal
`README.md` calls `flux tui` "the same daily driver" and `docs/usage.md` documents an entire
interaction surface for it. The public site gives it **one table row** (`agent/cli.md:31`) and a
slash-command table. The audience the vision ranks first — the personal coding agent user — gets
the least documented surface. Separately, `agent/cli.md` has no global-flags section, so several
shipped flags are undiscoverable.

## Acceptance
- [x] New `website/docs/agent/tui.md` documents the interaction surface: `Ctrl-J` / `Alt-Enter` /
      `Shift-Enter` newline, bracketed paste, mid-turn `/queue` with `Delete` / `Alt-Up` /
      `Alt-Down`, PgUp/PgDn and mouse scrollback, `Ctrl-End`, `Ctrl-E`, `Ctrl-C` and double-`Ctrl-C`
      quit, `Ctrl-D`, the `y` / `a` approval keys, `Ctrl-R` / `Ctrl-F` search, `@` path completion,
      and `/theme` with the available palettes.
- [x] Keybindings are taken from the TUI's key-handling source, not transcribed from
      `docs/usage.md` — the repo docs are themselves stale in places (see C-204).
- [x] Wired into `website/sidebars.js` under the Agent category, directly after `agent/cli`.
- [x] `agent/cli.md` gains a **global flags** section. It currently documents no global flag except
      `--store`. `--color` (`crates/flux-cli/src/args.rs:23`) and `--max-tokens` (`:117`) have zero
      occurrences anywhere on the site; `--add-dir`, `--allow-all-paths`, `--allow-private-net`,
      `--sandbox` and `--no-sandbox` are scattered across `agent/project-context.md`,
      `reference/config.md` and `security/os-sandbox.md` and absent from the CLI reference.
- [x] Failing-first: `tui_page_documents_the_bound_keys_and_themes` ties the page to `HELP_KEYS`
      and `Theme::names()`, so a rebind or a new palette that isn't documented fails the gate.
- [x] The deep pages keep ownership of the *reasoning* for the safety flags; the CLI reference
      lists them and links across.

## Progress

### Pages written
- **`website/docs/agent/tui.md`** (new). Sections: keybinding table, terminal support for the
  newline keys, pasting, the composer (slash menu + `@` completion), queue and steering, approvals,
  slash commands, themes, leaving. Every binding was read off the key-handling source, not
  `docs/usage.md`.
- **`website/docs/agent/cli.md`** (edited). Added `## Global flags` (the seven real `global = true`
  flags), added `--max-tokens` to Turn controls, corrected the incomplete REPL slash-command table,
  linked `flux tui` → the new page, added Related-docs links. The `--store` subsection now sits
  under Global flags and carries an explicit `{#store-flag}` id (its natural slug contains `<DIR>`
  and an em dash, which is fragile under `onBrokenAnchors: 'throw'`).

### Source of truth used
- Bindings: `crates/flux-tui/src/lib.rs:3190-3819` (the `Event`/`Event::Key` dispatch) and
  `HELP_KEYS` at `:274-301` — the same table the F1 overlay renders, so the page and the overlay
  cannot disagree.
- Approval keys: `crates/flux-tui/src/controller.rs:137-146` (`approval_key`).
- Terminal modes: `crates/flux-tui/src/terminal_io.rs:17-35` (raw, alt-screen, mouse capture,
  bracketed paste).
- Slash commands: `BUILTIN_COMMANDS` (`lib.rs:216-233`), the `handle_command` match (`:3847-4140`),
  and `command_is_read_only` (`:4144-4150`). REPL commands: `crates/flux-cli/src/session.rs:716-748`.
- Themes: `crates/flux-tui/src/theme.rs:247-249` (`Theme::names`).
- Flags: `crates/flux-cli/src/args.rs` — the seven `global = true` arms at `:23, 30, 36, 41, 50,
  62, 67`.

### Verified against the story's expected surface
Every item the Acceptance lists exists in the source, with these refinements:
- `Delete` **and** `Backspace` both retract in the `/queue` manager (`lib.rs:3372`) — the story only
  named `Delete`.
- `Ctrl-D` quits only when the session is **idle and the composer is blank**
  (`:3691 … if ctrl && !running && state.input_blank()`), not unconditionally.
- The double-`Ctrl-C` quit window is 2s (`CTRL_C_QUIT_WINDOW`, `:109`), and *any* other key disarms
  it (`:3212-3216`).
- `Ctrl-F` search steps matches with `n`/`N` only after `Enter` leaves typing mode (`:3418-3458`).
- `/theme` ships **six** palettes — `dark light dracula nord high-contrast mono` — not the three
  listed elsewhere (see discrepancy below).
- Queued messages are not merely "run next": the queue is a `SteeringQueue` shared with the engine
  (`crates/flux-flow/src/steering.rs:1-26`), drained at the head of each planner consultation and
  folded into the *running* turn. The page documents both outcomes.

### Bindings the code has that the story did not list (now documented)
`F1` help overlay · `Ctrl-T` mouse-capture toggle · `Shift-↑`/`Shift-↓` transcript entry focus with
`Enter` expand, `y` OSC-52 copy (72 KiB cap) and `Esc` clear · `↑`/`↓` history recall at the
composer's edges · slash-menu `Tab`/`Enter`/`Esc` · `@`-popup `Tab`/`Enter`/`Esc` · approval `n`/`Esc`
deny, `d` deny-with-reason, `↑`/`↓` subject scroll · the `/sessions` type-to-filter picker ·
`/usage` overlay.

### Discrepancies found (not fixed here — outside this story's owned files)
1. **`--max-tokens` is not a global flag.** The story note cites `args.rs:117`, but that arm lives
   on `AgentFlags`, which is flattened only into the agent-path subcommands (`run`, `tui`, `fork`,
   `app run`). Only the seven arms marked `global = true` are global. `--max-tokens` is therefore
   documented under **Turn controls**, not Global flags. Confirmed still true that it had zero site
   occurrences before this change, as did `--color`.
2. **`reference/config.md:91` understates the theme set.** The `theme` row says
   "(`dark`, `light`, `mono`)"; `Theme::names()` returns six. That file is C-200's; `tui.md` states
   the real six and cross-links config for persistence rather than restating.
3. `docs/usage.md:82-91` omits `Ctrl-T`, `F1`, `Shift-↑/↓`, `Ctrl-R`/`Ctrl-F` and the
   double-`Ctrl-C` confirmation — consistent with C-204's premise.

### Landed centrally (the two reserved items)
- Sidebar: `agent/tui` sits directly after `agent/cli` in the Agent category.
- Guard: `tui_page_documents_the_bound_keys_and_themes` in
  `crates/flux-cli/tests/website_contract.rs`. It binds the page to **`HELP_KEYS`** — the table the
  in-app F1 overlay renders — rather than to the `match` arms, on the reasoning in Notes: `HELP_KEYS`
  is the one list that cannot drift from the bindings without the overlay lying too, so it needs no
  new accessor. It takes the first alternative of each chord ("Ctrl-J / Alt-↵ / Shift-↵" → `Ctrl-J`)
  so the page is not pinned to the overlay's typography, and normalises `↵` → `Enter` because the
  width-constrained overlay uses a glyph where prose spells the key out. The same test asserts every
  `Theme::names()` palette appears in **both** `tui.md` and `reference/config.md`.
- Fixed the discrepancy the agent routed up: `reference/config.md`'s `theme` row listed three of the
  six palettes. Now lists all six and links to `tui.md`; the new assertion prevents a recurrence.
- `--max-tokens` was **correctly excluded** from Global flags. It lives on `AgentFlags`, which is
  flattened only into the agent-path subcommands, so only the seven `global = true` arms are global.
  The story's premise was wrong here and the page is right.
- Gate: `cargo test -p flux-cli --test website_contract` — 18 green. `npm run build` clean.

## Notes
- Binding the assertion to the key-handling source may require a small accessor or a test-visible
  table if the bindings are matched inline. Prefer exposing the binding table over asserting
  against a `match` arm by string.
- `/theme` persistence is `crates/flux-config/src/lib.rs:689, 1119-1126` — the same key C-200
  documents in the config reference. Cross-link rather than restate.
