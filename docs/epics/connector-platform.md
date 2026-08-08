---
id: E-24
title: "The connectors seam — a vendor credential flux is structurally unable to hold"
tracker: C-420
---

# The connectors seam — a vendor credential flux is structurally unable to hold

## Why

Its history was written down in [C-420](../stories/C-420-connector-platform-epic.md), which stays the narrative record.

## Success criteria

- [ ] A vendor credential never enters the flux process on this path, asserted by a test rather than
      by design intent (C-312).
- [ ] Every plugin-response ingest surface routes through that check — not just the projected-tool
      path (C-403, C-404).
- [ ] The surfaces that print plugin-authored strings run inside the sandbox floor and the approval
      envelope (C-410).
- [ ] A plugin cannot widen its own grant unobserved (C-411).
- [ ] The pack stops carrying twelve private copies of one primitive (C-405).
- [ ] This narrative reaches `docs/roadmap.md`, so the seam is findable without reading eight stories.

## Exit criteria

- [ ] Every story carrying `epic: connector-platform` is `done` (`flux board epics --slug connector-platform`).
- [ ] Every success criterion above is ticked.
