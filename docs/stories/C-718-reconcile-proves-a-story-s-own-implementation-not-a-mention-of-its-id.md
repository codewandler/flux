---
id: C-718
title: "Reconcile proves a story's own implementation, not a mention of its id"
pillar: "Core"
status: ready
priority: 6
areas: [flux-cli, flux-board]
note: "implementation-landed fires on any commit mentioning the id, including docs commits that file or block the story; the drive then withholds real work as already-built"
---

# Reconcile proves a story's own implementation, not a mention of its id

## Goal

`flux board reconcile` reports `implementation-landed` when it finds a commit that *mentions* a
story id. It does not check that the commit implemented that story. Because `flux fleet drive`
withholds an item as `already-built` on this signal, a false positive silently removes real work
from the frontier — the item stays `ready` forever and no worker is ever dispatched to it.

The signal must rest on evidence that the story's own implementation is present, and a commit that
only names the id must not be sufficient.

## Evidence

Observed on `main` at ffd8acfc, 76 findings, 70 of them `implementation-landed`:

- **C-544** (`ready`, all six Acceptance boxes unticked, no prompt-driven loop creation exists in
  the tree) — sole evidence is a commit belonging to a *different* story:
  `feat(tui): select the agent's loop from a hotkey with a visualizing overlay (C-543)`.
  `flux fleet drive` withholds it every tick with
  `already-built | board reconcile reports the implementation is already present`.
- **A-138** — sole evidence is `docs(track): close A-145, block A-138, correct C-422`. A docs-only
  commit that *blocked* the story is read as having implemented it.
- **A-140** — evidence includes `docs(board): file the fleet lifecycle epic` (1 file). Filing an
  epic is read as delivering it.

## Acceptance

- [ ] A commit whose only connection to a story is naming its id — in the subject, body, or by
      touching `docs/stories/<ID>-*.md` — does not by itself produce `implementation-landed`.
      Failing-first test built from the A-138 shape: a docs-only commit that blocks a story.
- [ ] Evidence is attributed to the story it implements, not to a sibling that names it. A
      failing-first test built from the C-544/C-543 pair: C-543's feature commit must not appear as
      C-544's evidence.
- [ ] `flux board reconcile` reports, per finding, what the signal actually rests on, so a reader
      can tell a real landing from an id mention without opening every commit.
- [ ] `flux fleet drive` withholds `already-built` only on the strengthened signal; a story whose
      Acceptance is wholly unticked is never withheld on evidence alone.
- [ ] Re-running reconcile on the current tree materially reduces the 70 `implementation-landed`
      findings, and each survivor is spot-checkable to a commit that changed that story's code.
- [ ] The gate is green in both workspaces.

## Notes

Filed while restoring fleet autonomy. This is the second frontier-suppression mode found in the
same session; the first was an empty wave holding a claim (wave-634 on `exchange/X-139`). Both share
a shape: the drive honestly reports why it withheld, and the withhold reason is wrong.
