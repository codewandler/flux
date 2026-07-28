---
id: C-122
title: "Plugin hosts should follow a worktree transition"
pillar: Core
status: backlog
epic: context-local-git-worktrees
design: docs/designs/context-local-git-worktrees.md
note: "v1 limitation: PluginHost pins cwd at subprocess spawn and SystemHostCaps captures an assembly-time System — plugin ops keep the original root after git_worktree_enter"
---

# Plugin hosts should follow a worktree transition

## Goal
After `git_worktree_enter`, built-in tools operate in the worktree but plugin operations do not:
`PluginHost::spawn` pins the subprocess cwd at spawn time and stores its own `System` for
restarts, and `SystemHostCaps` captures a separate assembly-time `Arc<System>` for host-mediated
`fs.read`/`process.run`. Design and implement how plugin hosts observe a workspace transition —
likely re-spawn-on-transition or a host-notification protocol — so plugin ops act in the same
root as everything else, without breaking long-lived plugin state.

## Acceptance
- [ ] A design note choosing re-spawn vs notify (and what happens to in-flight plugin calls and
      plugin-held state on transition), reviewed against the plugin process protocol.
- [ ] Plugin `process.run`/`fs.read` observe the active context root after enter and the restored
      root after leave, with a failing-first test at the plugin-host seam.
- [ ] `SystemHostCaps` resolves through the context's active system rather than a captured Arc.

## Progress
- (not started)

## Notes
- Documented v1 limitation in the epic design doc (Risks) and CHANGELOG.
- Sibling context: children/sub-agents already inherit correctly (C-100).
