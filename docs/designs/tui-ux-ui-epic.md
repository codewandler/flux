# Epic: TUI UX/UI polish (reconciled against v0.33.0)

Status: **reconciled 2026-07-29 — parked, ready to file as an epic**
Original draft: 2026-07-28, before the TUI polish round-2 epic (C-149…C-158) shipped in v0.33.0.

This doc grew out of a UX/UI review of the TUI and proposed 15 items (U-1…U-5, V-1…V-10). It was
written the day *before* round 2 landed, so it was never reconciled against what shipped. This pass
walks every item against the current source and records a verdict with the evidence. **Ten items
survive; two are superseded outright; three have residuals too thin to be worth a story.**

Scope correction: the draft said "`crates/flux-tui` only". That is wrong for **V-8**, which lands in
`crates/flux-markdown`. Everything else holds.

## Verdict table

| Item | Verdict | Evidence |
|---|---|---|
| U-1 Ctrl-G to next/prev failed tool card | **valid** | no `Ctrl-G` binding and no failure navigation anywhere in `lib.rs` |
| U-2 `/collapse` + `/expand` | **thin residual** | the global toggle already exists — `Ctrl-E` → `toggle_details()` (`lib.rs:3681`) |
| U-3 Ctrl-V paste | **superseded** | bracketed paste is enabled (`terminal_io.rs:29`) and handled (`lib.rs:3138`) |
| U-4 `/sessions` alias + hint | **half shipped** | the alias exists (`lib.rs:222`, dispatched `:3859`/`:3878`); the hint does not |
| U-5 footer queued count | **valid** | footer right side carries mouse-off, scroll %, steps/elapsed/llm — no queue count |
| V-1 composer accent bar | **valid** | composer is background tint only (`rendering.rs:293-299`) |
| V-2 always-on scrollbar | **valid (narrowed)** | renders only under `!state.follow` (`rendering.rs:190`) |
| V-3 turn separators + elapsed | **valid** | no inter-turn separator rendering exists |
| V-4 phase-tinted footer spinner | **thin residual** | C-181 already renders the retry label in `warn_style()` (`lib.rs:2358-2361`) |
| V-5 wider badge glyph language | **valid (glyph clash)** | only `◌ running` / ✓ / ✗ exist; `◆` is taken by the A-15 brief marker |
| V-6 light-theme tool card surface | **valid (smaller)** | `panel_bg` exists on all 12 palettes, used for overlays but never for tool cards |
| V-7 mid-size degradation tier | **valid** | one hard floor at `width < 24 \|\| height < 6` (`rendering.rs:129`), no tier below it |
| V-8 code-block gutter | **valid (wrong crate)** | `Block::CodeBlock` emits bare lines; `BlockQuote` already has the `│ ` prefix machinery |
| V-9 selection survives `mono` | **valid (cheaper now)** | queue + session rows style by `sel_bg` only; `MONO.sel_bg = Reset` |
| V-10 splash quiet flag | **thin residual** | `FLUX_NO_SPLASH` already exists (`splash.rs:554-557`) |

## Superseded — do not file

**U-3 (Ctrl-V paste).** The premise — "the composer cannot ingest the clipboard" — was already false
when written. `EnableBracketedPaste` is issued at terminal setup (`terminal_io.rs:29`) and
`Event::Paste(text) => state.input.insert_str(text)` (`lib.rs:3138`) inserts it verbatim, newlines
preserved. Ctrl-V / Cmd-V in any bracketed-paste-capable terminal already lands here. Adding a
key-level clipboard read would duplicate working behaviour and regress on terminals that send both.

**U-4's alias half.** `("sessions", "list recent sessions")` is in the command table (`lib.rs:222`)
with its own dispatch arms (`:3859`, `:3878`) and read-only classification (`:4094`). Only the
discoverability hint is missing — see the revised U-4 below.

## Thin residuals — folded, not filed

**U-2.** `Ctrl-E` (`toggle_details()`, `lib.rs:3681`) already collapses/expands every card at once, so
the draft's motivation ("no way to collapse *all* cards") does not hold. What remains is a naming
question — a `/collapse` + `/expand` spelling that appears in `/help` and works while a turn runs.
Real but marginal; fold it into U-1 if U-1 is built (both are transcript-navigation discoverability).

**V-4.** C-181 already tints the one state that mattered: a pending retry renders `↻ retry 2/6 · …`
in `warn_style()` on both the truecolor and braille paths (`lib.rs:2358-2361`). The unshipped
residual is tinting the braille glyph itself and an `err` flash at a failed turn end — cosmetic on top
of a distinction that already reads.

**V-10.** `FLUX_NO_SPLASH` (plus `NO_COLOR` and the undersized-terminal skip) already covers the
"noise on the tenth run" case (`splash.rs:554-557`). Only the `/quiet` session toggle is unbuilt, and
a session-scoped toggle for a once-per-startup animation has no user visible after startup.

## Surviving items (ten) — file these if the epic is picked up

Ordered by value per unit of work, not by draft order.

1. **V-9 — selection styling that survives `mono`.** The only *correctness* item in the list: with
   `NO_COLOR=1`, `MONO.sel_bg = Color::Reset` (`theme.rs:129`), so the queue overlay
   (`rendering.rs:323-337`) and session picker (`:389-396`) show no selection at all — the user
   cannot see what Enter will act on. The slash and `@` menus already carry ` ▸ ` (`:246`, `:279`);
   this is applying the same marker + BOLD to the other two. **Cheaper than when drafted**: C-152
   collapsed all three overlays onto `render_overlay_panel`, so the fix has one home.
   *Acceptance:* with `NO_COLOR=1`, selection is perceivable in the slash menu, queue modal, and
   session picker; pinned by a render test asserting the marker on the selected row.

2. **V-7 — graceful mid-size degradation.** Below 24×6 the TUI says "terminal too small"; between
   that floor and comfortable there is no tier, so a 50-column split pane renders queue rows and
   slash descriptions it has no width for. Below 60 columns drop the queue preview and the slash
   description column; below 40 force a 1-row composer.
   *Acceptance:* at 50 columns transcript/composer/footer all render; at 30 the slash menu shows
   names only; no panic at 24×6.

3. **V-8 — code-block framing in the Markdown renderer.** Lands in **`flux-markdown`**, not
   `flux-tui`. The machinery already exists: `Block::BlockQuote` pushes a muted `│ ` `Prefix`
   (`render/layout.rs:143-160`) while `Block::CodeBlock` emits bare `code_line`s (`:134-141`). Give
   `CodeBlock` the same treatment with `▎`. Benefits every Markdown surface, not just the TUI.
   *Acceptance:* fenced blocks carry the gutter on every row; inline code unchanged; the glyph
   survives `mono`.

4. **V-2 — always-on slim scrollbar.** C-106 shipped the indicator but gated it on `!state.follow`
   (`rendering.rs:190`, and the footer `⤓ N%` at `lib.rs:2377`), so scroll position is invisible
   until you scroll — exactly the moment you want to know there is more above. Render the track
   whenever `max_scroll > 0`; thumb colour distinguishes follow from detach.
   *Acceptance:* track visible whenever `last_max_scroll > 0`; transcript width unchanged (it is an
   overlay today and stays one).

5. **V-1 — focused-composer accent bar.** The composer's only boundary is `composer_style()`'s
   background (`rendering.rs:293-299`), which `mono` flattens away entirely. A one-column `▍` in
   `accent` while idle, `muted` while running, is a focus read that survives `mono` because it is a
   glyph.
   *Acceptance:* visible in dark/light/mono; text insets by one column rather than shifting; no
   layout churn between running and idle.

6. **V-5 — wider tool-badge glyph language.** Today an op killed by Ctrl-C just stops updating with
   no terminal badge — indistinguishable from one still running after the spinner stops.
   Add a cancelled badge and a dry-run badge. **Pick a different glyph than the draft's `◆`**: it is
   already the A-15 brief marker (`lib.rs:1699`, `:1730`).
   *Acceptance:* a cancelled op seals with its own badge; dry-run results seal distinctly; ✓/✗
   unchanged.

7. **U-1 — jump to next/previous failed tool card.** Failure is structured data on `ToolEntry`
   (`result.is_error`) but finding the one `✗` in a long turn is manual scrolling. `Ctrl-G` is free
   (no `Char('g')` binding exists). Fold U-2's `/collapse`+`/expand` in here if convenient.
   *Acceptance:* Ctrl-G cycles the failures, detaches follow, flashes a notice when there are none;
   covered by a key-handling test.

8. **V-3 — turn separators with elapsed time.** A faint rule with a right-aligned `── 14:32 · 12s ──`
   between completed turns, derived from `last_elapsed` — no new state. Note it must compose with
   C-149's gutter rail (also a leading span) and the chunk-per-entry layout cache.
   *Acceptance:* the rule appears only between completed turns, never around tool cards; glyph-based
   so it survives `mono`.

9. **V-6 — light-theme tool card surface.** In `LIGHT`/`LIGHT_RGB` every surface is near-white, so
   cards blend into prose. Reuse the existing `panel_bg` (present on all 12 palettes) on card
   summary/detail rows for light themes only. Smaller than drafted — no new theme field.
   *Acceptance:* light themes show a visible card block; dark/mono unchanged; no extra rows.

10. **U-5 — footer shows the queued-prompt count.** The queue preview strip renders up to three rows
    plus `+N more queued` (`rendering.rs:205-227`), but the footer — where the eye is during a long
    turn — carries no count. Append a droppable `· +N queued` segment that sheds before the C-180
    timing segment.
    *Acceptance:* the segment appears only while running with a non-empty queue; it sheds before
    `N steps · elapsed` on narrow terminals.

**Revised U-4 (fold into the C-157 card, not the splash).** The alias already exists; only the
discoverability hint is missing, and its right home is now the C-157 empty-state card
(`rendering.rs:render_empty_state_card`), which already names model, workspace, and the three
affordances. Add a fourth line — `↳ N previous sessions — /sessions to resume` — only when the store
has any. This is a one-line addition to a card that already exists, not a splash change.

## Verification (unchanged from the draft)

- `cargo test -p flux-tui` and `-p flux-markdown` (V-8), new unit tests per item.
- `cargo clippy --all-targets -- -D warnings` clean, `cargo fmt --all --check` in **both** workspaces.
- Manual smoke: `cargo run -p flux-cli -- run -m mock` in dark, light (`/theme light`), and
  `NO_COLOR=1` to eyeball V-1/V-2/V-6/V-9.
