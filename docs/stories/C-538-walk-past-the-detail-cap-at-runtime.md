---
id: C-538
title: "Walk past the tool-card detail cap at runtime"
pillar: Core
status: backlog
epic: tool-output-rendering
design: docs/designs/tool-output-rendering.md
areas: [flux-tui]
note: "The '… N more lines' row is inert; the only escape is restarting with -v"
---

# Walk past the tool-card detail cap at runtime

## Goal

The `… N more lines` elision row stops being a dead end: on the focused card, Enter cycles
collapsed → capped → full → collapsed, so one oversized result can be inspected in place without
restarting the session with a global verbosity flag.

## Acceptance

- [ ] Failing-first TestBackend frame test with a 40-line result: first Enter shows the capped 30
      rows plus an elision row that advertises the next step (e.g. `… 10 more lines (Enter for
      all)`), second Enter shows all 40, third collapses.
- [ ] The full state is offered only when rows were actually elided; cards under the cap keep the
      existing two-state toggle.
- [ ] `-v`/`FLUX_VERBOSE` semantics unchanged; applies to final results only — the C-158 live tail
      (3 lines, deliberately capped) is untouched.
- [ ] Ctrl-E global toggle behavior and per-card override precedence (C-111) preserved; existing
      frame tests green.
- [ ] `cargo test -p flux-tui` green.

## Progress

- 2026-08-05 — filed from the tool-output review.

## Notes

- Evidence: `MAX_DETAIL = 30` and the inert elision rows at `crates/flux-tui/src/lib.rs:203-205,
  2512-2525, 2533-2561`; per-card expansion state `ToolEntry::expanded` (`lib.rs:420-436`).
- In-card scrolling/pagers are explicitly out of scope (deferred to the A-142 detail pane); this is
  a three-state toggle, not a viewer.
- Design: [tool-output-rendering](../designs/tool-output-rendering.md) F-8.
