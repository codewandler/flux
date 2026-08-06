---
id: C-645
title: "Ops explorer: docs in the detail pane and an /ops overlay in the chat TUI"
pillar: "Core"
status: ready
epic: ops-explorer
areas: [flux-tui, flux-markdown]
design: docs/designs/operations-explorer-epic.md
note: "depends on C-643 landing; do not dispatch in the same wave as C-643"
priority: 37
---

# Ops explorer: docs in the detail pane and an /ops overlay in the chat TUI

## Goal

The explorer starts becoming a documentation reader: the right pane can show the actual
documentation for the selected op — its `language/ops.md` category section and the other pages
that mention it — rendered in-TUI through flux-markdown, plus an explicit trusted action to open
a page in the browser (preferring the local `flux docs` server when it runs). The explorer also
becomes reachable from inside `flux tui` as an `/ops` overlay sharing the same state and render
code.

## Acceptance

- [ ] A docs tab/toggle in the detail pane renders the op's ops-reference section and lets the
      user cycle through the "mentioned on" pages from the C-643 index, all through the existing
      flux-markdown pipeline (pre-wrapped output rules respected); content passes through the
      TUI trust sanitizer.
- [ ] An explicit keypress offers "open in browser" as a trusted action: a minimal opener used
      nowhere else, never triggered by rendered content (OSC 8 stays stripped), preferring the
      local docs server URL when reachable and falling back to the public URL; test proves the
      action is only reachable from the explicit key.
- [ ] `/ops` opens the same explorer as an overlay inside `flux tui` (slash command + key
      routing following the existing overlay cascade), sharing `ExplorerState` and draw code
      with the standalone path; headless test drives open → search → close without disturbing
      chat state.
- [ ] Workspace gate green; WHATS-NEW.md updated.

## Progress

- 2026-08-06 filed.

## Notes

- Depends on C-643. Converge with the `docs-reader` epic on corpus + rendering plumbing — no
  second index format (see docs/designs/tui-docs-reader-read-the-docs-where-you-work-run-the-examples-epic.md
  and docs/designs/agent-native-flux-docs.md).
- The chat TUI's slash command must also be mirrored in flux-cli's builtin command list, or a
  command file can shadow it.
