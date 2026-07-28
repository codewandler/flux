---
id: C-122
title: "Plugin hosts should follow a worktree transition"
pillar: Core
status: done
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
- DONE 2026-07-28. The design note (in the epic design doc, "C-122 — plugin hosts follow the
  transition") chose **neither re-spawn nor notify**: in the reference-only IO architecture the
  subprocess is not where root-sensitive IO happens — `process.run`/`process.spawn` execute
  host-side through the guarded System, `fs.read` is absolute-path-scoped by design, and the rest
  have no root. The fix is host-side dynamic resolution:
  - `flux_plugin::SystemSource` seam (+ `FixedSystem` for non-transitioning surfaces);
    `SystemHostCaps::from_source` resolves the guarded system once per `handle()` call — the same
    snapshot-per-op discipline as `ToolContext::system`. `SystemHostCaps::new` keeps its signature
    (wraps a `FixedSystem`), so `flux plugin call` and tests are unchanged.
  - The session surfaces create the `WorkspaceContext` EARLY (before plugin loading) and bind the
    same handle into both the plugin caps (`WorkspaceSystemSource` adapter in flux-cli — flux-plugin
    stays free of a runtime dependency) and the executor context
    (`ExecutionEnvironment::with_workspace` + `ToolContext::over_workspace`), so plugin ops and
    built-in tools observe the SAME transitions. Wired on both the agent-session path
    (execution.rs) and the app-run path (app_cmd.rs).
  - Failing-first tests: `process_run_follows_the_active_system_across_a_transition` at the
    plugin-host seam (fails under captured-Arc semantics), and
    `with_workspace_shares_one_handle_between_surface_and_executor` in flux-runtime (both
    directions of the shared handle).
- Deliberately NOT changed, per the design note: `PluginHost`'s subprocess cwd and crash-restart
  root stay pinned at the session root (advisory-only for a compliant plugin; a restart that lands
  in a different root mid-session would be harder to reason about), and no `flux.plugin.v1`
  protocol addition (it would deliver information a compliant plugin must not depend on).
- Scope note honoured: plugin hosts are session-global; the caps follow the session's primary
  context, not a sub-agent's independent workspace (C-100 semantics).

## Notes
- Documented v1 limitation in the epic design doc (Risks) and CHANGELOG.
- Sibling context: children/sub-agents already inherit correctly (C-100).
