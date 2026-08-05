---
id: C-544
title: "Create a new agent loop by prompting, available in the loop selector"
pillar: Core
status: ready
priority: 32
epic:
design:
areas: [flux-tui, flux-lang, flux-agent]
note: "\"create this <...> loop for me\" -> flux writes the *.flux file; the new loop then appears in the C-543 selector"
---

# Create a new agent loop by prompting, available in the loop selector

## Goal

An operator can create a new agent loop by describing what it should do — "create this <…> new
loop for me" — and flux generates the corresponding `*.flux` file, so new loops are authored
conversationally and become immediately selectable in the TUI loop selector without hand-writing
flux source.

## Acceptance

- [ ] A prompt-driven creation path exists ("create this <…> loop for me"): flux generates a
      `*.flux` loop file from the description. *(First pass — confirm the entry point: TUI command,
      chat directive, or CLI subcommand.)*
- [ ] The generated file is valid flux: it parses and passes the same load-time validation as a
      hand-written loop before it is offered for use; an invalid generation is refused with the
      error surfaced, never silently saved. Failing-first tests for both the accept and refuse
      paths.
- [ ] The new loop appears in the [C-543](C-543-tui-loop-selector-with-visualizing-overlay.md)
      selector immediately after creation and can be selected and run.
- [ ] The generated loop carries a description (used by the C-543 overlay) derived from the
      operator's prompt.
- [ ] The gate is green in both workspaces.

## Progress

- (not started)

## Notes

- Filed 2026-08-05 via /track:story from the roadmap coordinator session; first-pass draft from the
  title — refine the entry point and the generation contract (model-assisted vs template) before
  dispatch.
- Depends on [C-543](C-543-tui-loop-selector-with-visualizing-overlay.md) (the selector and its
  live-refresh acceptance). Dispatch serialized after it or in the same wave starting from its
  integrated prerequisite.
