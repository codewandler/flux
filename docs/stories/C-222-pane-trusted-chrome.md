---
id: C-222
title: The trusted-chrome invariant — an agent pane can never be mistaken for the approval sheet
pillar: Core
status: in-progress
priority: 13
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-tui]
note: "C-163 already wrote the rule for plugins — 'a plugin that can pop a dialog is a plugin that can phish the user inside a trusted surface … constrain the rendering rather than relying on good behavior'; it holds harder for the model, which is the thing the approval sheet exists to gate"
---

# The trusted-chrome invariant

## Goal
Make it structurally impossible for an agent-authored pane to be mistaken for harness chrome — the
approval sheet above all. The failure mode is not a rendering bug; it is a pane rendering *correctly*
and being read as a trusted prompt. This story owns the mark, the styling boundary, the draw order,
and the adversarial test that proves them.

## Acceptance
- [x] Every agent-authored pane renders with a **surface-owned** mark and border style sourced from
      the `Theme`, never from the payload. The model has no field that reaches a `Style` (C-220 pins
      the type); this story pins the *rendering* — a payload cannot inject styling through content
      either.
      → `trust::agent_block` / `trust::agent_mark_span` (`crates/flux-tui/src/trust.rs:65-97`) are
      the only chrome constructors, and `trust::sanitize` closes the content route; asserted by
      `panes::tests::a_pane_paints_only_colours_the_theme_defines`.
- [x] The mark survives `Theme::MONO` (`crates/flux-tui/src/theme.rs:120`), where every colour role
      resolves to `Color::Reset`. It is therefore a **glyph plus a modifier**, not a tint — the same
      reasoning C-149 used for the transcript gutter rail (`lib.rs:770-781`) and C-154 for the
      approval risk tiers.
      → `panes::tests::the_agent_mark_survives_mono_in_every_slot` (screen level, all four slots)
      and `trust::tests::the_mark_is_a_glyph_and_a_modifier_not_a_tint`.
- [x] **Failing-first test (`TestBackend`):** a pane whose `title` and `data` are verbatim
      approval-sheet text (`" approval · destructive "`, the subject lines, the `y/a/N` affordance)
      still renders inside the marked agent region, and — with an approval pending — the real sheet
      draws **over** it on its own `Clear`ed rect. The screen assertion distinguishes the two.
      → `panes::tests::an_agent_pane_cannot_imitate_the_approval_sheet`. The payload is not
      hand-copied: it is the sheet's own rendered rows, read back off the buffer, so it cannot rot.
- [x] Draw order is explicit and tested: panes render before the approval sheet, always. A pane
      cannot occlude the sheet at any width, in any slot, including `overlay`.
      → `panes::tests::a_pane_can_never_occlude_the_approval_sheet_at_any_width_or_slot`: the
      sheet's rows are byte-identical, styles included, with and without a greedy pane open.
- [x] Pane payload is rendered as **text, never interpreted** — no ANSI passthrough, no escape
      sequences, control characters stripped. This is the C-113/C-114 approval-modal lesson applied
      one surface over, and C-163 names it as a requirement for the plugin case too.
      → `trust::sanitize` (ECMA-48 sequences consumed whole, controls and invisible/bidi format
      characters dropped); `trust::tests::escape_sequences_and_control_bytes_are_removed_whole`.
- [x] Multi-byte and wide-character payloads truncate on **char** boundaries, never `String::truncate`
      at a byte offset (AGENTS.md, the guarded-process invariant's wording; the same rule holds
      wherever untrusted bytes get bounded).
      → `trust::tests::multi_byte_and_wide_payloads_survive_intact_and_truncate_on_char_boundaries`
      (a lock, not a fix: `crate::truncate` was already char-wise, and `sanitize` is too).

## Progress
- Landed `crates/flux-tui/src/trust.rs`, the trusted-chrome boundary, and rewired `panes.rs` and
  `rendering.rs` onto it.
- **The invariant is enforced by construction, in three places at once:**
  1. `PaneStore` holds `trust::AgentPane`, whose inner `PaneSpec` is private to `trust` and whose
     only constructor sanitizes. `panes.rs` is a *sibling* module, so it cannot assemble one from a
     raw spec — an unsanitized payload cannot be rendered because it cannot be stored.
  2. `sanitize` drops escape sequences (whole, per the ECMA-48 grammar) and control characters, and
     replaces every glyph from the blocks that exist in order to draw (`is_reserved`) with a space,
     one for one.
  3. The mark and border come from the `Theme` via `agent_block` / `agent_overlay_header`. The mark
     is ` ◆ agent ` under `REVERSED | BOLD` — a glyph, a word and two modifiers, no tint.
- **What is guaranteed, stated precisely** (`trust.rs` has the same section, because C-163 is told to
  inherit this invariant and an overclaim here would propagate): a payload cannot style anything,
  cannot produce a glyph from the drawing blocks — so no *pixel-accurate* copy of harness chrome —
  paints only inside a region whose border ring and mark the surface draws afterwards from the
  theme, and cannot change one cell of the approval sheet. **Not** guaranteed: that nothing a
  payload writes can *resemble* a frame; ASCII `| - + _` always can, and cannot be taken away from
  text. `is_reserved` raises the cost and kills the accurate imitation; the thing actually standing
  between the user and a phish is the mark.
- The rule is **range-shaped, not an enumeration**, because reserving the six glyphs the sheet
  happens to use (`┌┐└┘─│`) is defeated by `┏┓┗┛━┃`, then `╔╗╚╝═║`, then `╭╮╰╯`. Reserved: misc
  technical (scan lines, bracket pieces), box drawing, block elements, geometric shapes, braille
  (the spinner's own block), arrows, misc symbols and arrows, CJK compatibility forms, the
  fullwidth/halfwidth symbol tail, geometric shapes extended, legacy computing, plus a short,
  explicitly best-effort list of named rules and attention marks.
- `render_overlay_panel`'s `header` became a `Line<'static>` so the `overlay` slot — which shares
  its chrome with the host's own overlays — can carry the mark's modifiers. Host callers pass the
  same styled row they passed before, so their frames are unchanged.
- Markdown gets a second pass (`trust::sanitize_lines`) because it is the one kind whose *renderer*
  turns payload text into glyphs; the tree connectors and the progress bar are surface-generated and
  are deliberately left alone.
- **Beyond the letter of the Acceptance:** the reserved-glyph rule. Stripping ANSI alone leaves a
  payload able to draw a pixel-accurate box-drawn counterfeit of the sheet out of ordinary text,
  which is the actual threat this story names.
- Base proof: at `37b0a3c6` the same test reported *53 cells of chrome the surface did not draw* —
  a full counterfeit frame plus a forged `◆` mark and the sheet's `⚠`.
- **Review round 2 (blocking finding, fixed).** The first revision's ranges stopped one Unicode block
  short, and its adversarial test could not see it: `panes.rs`'s `is_chrome_glyph` re-stated the same
  three ranges as `trust::is_reserved`, so the test's blind spot *was* the implementation's. A
  counterfeit sheet rebuilt from U+23B8–23BD scan lines, Braille, legacy-computing sextants and
  fullwidth rules rendered in full and scored **zero forged cells**. Both halves are now fixed: the
  rule is widened, and the test is driven by `CHROME_LOOKALIKES` — a corpus grown by *probing* the
  rule, not derived from its ranges — so widening the rule and widening its test are no longer the
  same edit. Verified by re-narrowing `is_reserved` to the shipped-first three ranges: the test then
  reports *29 lookalike(s) reached a cell*, naming each one and what it imitates.

## Notes
- This is the story a reviewer should read first, and the one where a review verdict matters more
  than a green test. It is deliberately separated from C-221 so the invariant gets its own
  adversarial pass rather than riding along with layout work.
- The impersonation payload for the test should be lifted from `approval_tier_style`
  (`rendering.rs:108-119`) and `plan_detail_lines` (`controller.rs:424`) so it stays accurate as the
  sheet evolves — a hand-copied string would rot silently and the test would keep passing.
- Worth stating in the code comment, not just here: the reason the model gets no style field is not
  aesthetic consistency. It is that a style field is the phishing primitive.
- When [C-163](C-163-plugin-commands-and-host-ui.md) is designed, its host-UI prompts must land on
  this same invariant rather than a parallel one. One trusted-chrome rule, one place it is enforced.
