---
id: C-438
title: "Where do the files live — the question that decides whether a remote agent is usable for coding"
pillar: Core
status: done
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [flux-system, docs]
note: "⚠ the hard one. A coding agent's loop is read a file, edit it, run the tests. Local runtime + remote system means either every read crosses the network (and your editor shows something else) or you have a sync problem — which is where this class of tool usually dies. C-395 made the file surface a port; the SEMANTICS are this story"
---

# There is no free answer, so pick one deliberately

## Goal

Decide and document where the workspace lives when the runtime is local and the system is remote.

## Why this decides the epic

The coding loop is: read a file, edit it, run the tests, read the output. With a local runtime and a
remote system there are exactly two shapes and both cost something:

- **Files remote.** Every read and write crosses the network, and the editor open on your screen is
  looking at a *different* tree than the agent is editing. Consistent, and it breaks the thing people
  most want from a local UI.
- **Files local.** Your editor and the agent agree, and now the remote executes against something that
  must be synchronised. ⚠ **This is where this class of tool usually dies** — sync is a distributed
  systems problem wearing a convenience feature's clothes.

⚠ There is no third answer that is free, and choosing by accident means choosing the one that was
easiest to implement first. C-395 made the workspace-confined file surface a port
(`GuardedWorkspaceFiles`), so the *mechanism* exists either way; this story owns the *semantics*.

## Acceptance

- [x] The decision is made and written down, with its cost stated rather than minimized.
- [x] ⚠ **The failure mode of the chosen shape is documented where a user will hit it**, not in a design
      doc. If files are remote, say that the local editor is not the tree being edited. If local, say
      what happens when sync is mid-flight or fails.
- [x] ⚠ A test pins the chosen semantics, including the disagreement case — local and remote holding
      different content is the state that must not be silent.
- [x] Path confinement survives. The workspace guarantee is that operations stay inside the workspace;
      whatever shape is chosen, a remote must not be able to widen it. ⚠ A remote that resolves paths
      itself is a place confinement can quietly stop applying.
- [x] The answer covers the other locality-sensitive resources too, or explicitly defers each: the
      evidence log, the cassette/session store, and the plugin store.
- [x] Full gate green.

## Notes

- Settleable ahead of [C-436](C-436-flux-tui-remote.md), and it should be — this is the constraint the
  link is built around, not a detail discovered inside it.
- ⚠ `ssh` is the honest competitor here and it solves this by not having the problem: everything is
  remote, including your editor. The epic has to be better than `ssh`, and this story is where that is
  won or lost.
- Feeds [C-440](C-440-the-topologies-page.md) — "where are my files" is the first thing a reader of that
  page will want to know per topology.

## Progress

- **Decision (2026-08-02): the remote workspace is canonical in v1.** Every project-relative read,
  write, discovery operation and process cwd resolves on the selected remote substrate. There is no
  implicit sync engine and no local fallback. A local editor sees a different tree unless the
  operator explicitly mounts or attaches to the remote workspace; the TUI must state the remote
  identity and canonical root persistently.
- Local control-plane state does not move: provider/model configuration, the credential store,
  sessions, cassettes and the evidence log stay with the local runtime. Remote operation delivery
  has its own bounded receipt ledger (C-476), not a second session store.
