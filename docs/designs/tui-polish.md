# Design: TUI polish — 5 UX + 5 UI improvements

**Status:** implemented 2026-07-28 — wave 1 (C-102…C-110) and wave 2 (C-111…C-116) · **Pillar:** Core · **Stories:** wave 1: [C-102](../stories/C-102-graceful-narrow-width-bars.md), [C-103](../stories/C-103-approval-modal-safety-and-redesign.md), [C-104](../stories/C-104-tui-theme-system.md), [C-105](../stories/C-105-mouse-capture-copy-toggle.md), [C-106](../stories/C-106-scroll-position-indicator.md), [C-107](../stories/C-107-reverse-history-search.md), [C-108](../stories/C-108-transcript-search.md), [C-109](../stories/C-109-live-running-tool-cards.md), [C-110](../stories/C-110-help-overlay.md) · wave 2: [C-111](../stories/C-111-transcript-entry-focus-yank.md), [C-112](../stories/C-112-composer-path-completion.md), [C-113](../stories/C-113-approval-deny-with-reason.md), [C-114](../stories/C-114-streaming-markdown-prefix.md), [C-115](../stories/C-115-diff-hunk-view.md), [C-116](../stories/C-116-header-mode-badges.md)

## Why

The TUI became a daily driver with A-65 and gained its boot splash + spinners with C-101. The
remaining rough edges are well-defined and small: mouse capture blocks terminal-native copy, the
approval modal denies on any stray key and Debug-formats its subjects, `/help` is a transcript
notice, there is no search of any kind (transcript or history), exactly one ANSI dark theme exists,
scroll position is invisible, and narrow terminals drop the whole right half of the header/footer.
This epic lands five UX and five UI improvements, each independently shippable, all within
ratatui 0.29 (pinned by markdown-ratatui).

## Approach

All work stays modules inside `crates/flux-tui` (no new crates). Existing seams reused:
`rendering.rs` `render()`, the event loop + key dispatch in `lib.rs`, `bar_line`, the `COMMANDS`
table, `Theme`, the `(revision, width)`-keyed transcript layout cache, and the TestBackend pinning
tests. Key-dispatch logic is extracted into pure helpers (`approval_key`, `rsearch`, match-row fn)
so it is testable without the async loop.

**UX**
- *Mouse-capture copy toggle (C-105):* Ctrl-T flips capture live; footer indicator while off so
  native select/copy works. Ctrl-T verified unbound in tui-textarea 0.7.
- *Approval safety (C-103):* structured `ApprovalView { tool, subjects, scroll }` replaces the pub
  `modal: Option<String>`; only explicit keys act (y / a / n / Esc), stray keys are ignored instead
  of denying; subjects render as text, not `{:?}`.
- *Reverse history search (C-107):* Ctrl-R readline-style incremental search over durable prompt
  history; footer takeover line. Shadows tui-textarea redo — accepted (Ctrl-U undo stays; precedent:
  Ctrl-E already shadows end-of-line).
- *Transcript search (C-108):* Ctrl-F incremental search over wrapped transcript rows, n/N step,
  REVERSED highlight patched only onto the cloned viewport slice — the layout cache is untouched.
  v1 limitation: matches spanning a wrap boundary aren't found. Shares one footer-takeover
  precedence (search > history-search > normal) with C-107.
- *Help overlay (C-110):* F1 / `/help` open a centered panel; key list + slash commands iterated
  from `COMMANDS` so it can't drift. Lands last so its content is complete.

**UI**
- *Graceful narrow bars (C-102):* `bar_line` takes ordered droppable right-side segments; header
  drops cost → cache → tokens progressively. Lands first — later footer segments build on it.
- *Approval redesign (C-103, paired):* accent-bordered sheet, styled subject list windowed with a
  `+N more` scroll marker, colored key hints.
- *Theme system (C-104):* `DARK_RGB`, `LIGHT`/`LIGHT_RGB`, `MONO` (NO_COLOR) + `Theme::by_name`;
  `/theme` command; persistence via `flux_config::Config.theme` + a `persist_user_theme` following
  the `persist_allow_rules` read-merge-atomic-rename pattern; root background fill (`base_bg`) so
  LIGHT works on dark terminals.
- *Scroll indicator (C-106):* ratatui `Scrollbar` on the transcript while detached from follow
  mode, plus a percent segment in the footer.
- *Live running tool cards (C-109):* live elapsed + animated glyph on running tool cards by
  patching only visible running header lines per 62 ms tick via a shared `tool_header_line()`
  helper — the layout cache must NOT be invalidated per tick (that staticness was deliberate).

## Second wave — follow-ups (C-111…C-116)

A review pass after the first wave was accepted surfaced six complementary improvements that fit
the same seams and don't conflict with the accepted decisions above. Filed under this epic as
P3 (behind the first wave):

- *Transcript entry focus + per-card expansion + OSC-52 yank (C-111):* `expand_tools` stays a
  single global bool in wave 1; a focus cursor makes expansion per-card and gives yank a clean
  whole-entry copy path that complements C-105's native-selection toggle (and works over SSH).
- *`@` file-path completion in the composer (C-112):* the slash menu remains the only completion;
  reuse its popup slot for fuzzy workspace paths.
- *Deny-with-reason (C-113):* extends C-103's `approval_key` (Allow/AllowAlways/Deny) with a
  reason-carrying denial the model can adapt to.
- *Markdown while streaming (C-114):* render the completed block prefix styled, keep the
  unterminated tail plain — sequenced after C-104 because of the hardcoded-span-color risk below.
- *Diff hunk view (C-115):* upgrade the flat `DetailKind` diff to hunk headers + line numbers +
  intraline highlight, and embed it as the content preview C-103's sheet deliberately deferred.
- *Header mode badges (C-116):* `shell` / `auto-ok` / `effort:<level>` / `gather` as droppable
  right-side segments on the C-102 `bar_line` mechanism; `auto-ok` drops last.

## Alternatives considered

- Mouse capture off by default (native copy always works) — rejected: wheel scroll is the primary
  transcript interaction; a live toggle keeps both.
- Bumping the transcript revision per tick for animated tool cards — rejected: O(transcript)
  re-wrap at 16 fps; viewport patching keeps the cache intact.
- Shrinking the transcript width by one column for the scrollbar — rejected: would re-key the
  layout cache on every follow attach/detach.

## Risks & open questions

- flux_markdown hardcodes span colors; some may clash with the LIGHT theme. Accepted for v1;
  follow-up story to be filed if it bites.
- C-109 is the riskiest item: the patched header line's pad math must exactly mirror
  `tool_lines` — hence the shared helper, and a test pinning that the cache revision is unchanged
  across animation frames.
- Two pub-surface breaks (`TuiRunOptions.theme` field, `ChatState.modal` → `approval`) →
  next MINOR (0.28.0); batched.

## Acceptance / done

Union of the member stories' acceptance. Manual smoke: `flux tui` — toggle mouse + copy text,
mistype in an approval (nothing happens), F1, Ctrl-R, Ctrl-F, `/theme light`, shrink to ~50 cols,
watch a running tool card animate.
