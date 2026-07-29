---
id: C-222
title: The trusted-chrome invariant — an agent pane can never be mistaken for the approval sheet
pillar: Core
status: ready
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
- [ ] Every agent-authored pane renders with a **surface-owned** mark and border style sourced from
      the `Theme`, never from the payload. The model has no field that reaches a `Style` (C-220 pins
      the type); this story pins the *rendering* — a payload cannot inject styling through content
      either.
- [ ] The mark survives `Theme::MONO` (`crates/flux-tui/src/theme.rs:120`), where every colour role
      resolves to `Color::Reset`. It is therefore a **glyph plus a modifier**, not a tint — the same
      reasoning C-149 used for the transcript gutter rail (`lib.rs:770-781`) and C-154 for the
      approval risk tiers.
- [ ] **Failing-first test (`TestBackend`):** a pane whose `title` and `data` are verbatim
      approval-sheet text (`" approval · destructive "`, the subject lines, the `y/a/N` affordance)
      still renders inside the marked agent region, and — with an approval pending — the real sheet
      draws **over** it on its own `Clear`ed rect. The screen assertion distinguishes the two.
- [ ] Draw order is explicit and tested: panes render before the approval sheet, always. A pane
      cannot occlude the sheet at any width, in any slot, including `overlay`.
- [ ] Pane payload is rendered as **text, never interpreted** — no ANSI passthrough, no escape
      sequences, control characters stripped. This is the C-113/C-114 approval-modal lesson applied
      one surface over, and C-163 names it as a requirement for the plugin case too.
- [ ] Multi-byte and wide-character payloads truncate on **char** boundaries, never `String::truncate`
      at a byte offset (AGENTS.md, the guarded-process invariant's wording; the same rule holds
      wherever untrusted bytes get bounded).

## Progress
- (not started — depends on C-221's rendering)

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
