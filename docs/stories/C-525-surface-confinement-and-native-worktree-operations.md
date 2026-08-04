---
id: C-525
title: "Tell the model its confinement and worktree topology, then expose native worktree operations"
pillar: Core
status: backlog
epic: context-local-git-worktrees
note: "No trial-and-error escape probes: inject proven sandbox/worktree facts before the first turn and manage linked worktrees through guarded Git ops"
---

# Tell the model its confinement and worktree topology, then expose native worktree operations

## Goal

Give every model the runtime's authoritative workspace, sandbox and Git-worktree constraints before
its first action, so it does not waste tool calls discovering that paths outside the effective root
are forbidden. Complete the same boundary with guarded, typed Git operations for inspecting,
creating, preview-merging and safely retiring worktrees, without asking the model to shell out or
construct escape-prone paths.

## Acceptance

- [ ] A typed startup context provider, wired beside `EnvContext` and `GitContext`, renders only
      runtime-proven facts before the first model turn: effective cwd/workspace root; sandbox backend
      and disabled/allow/require posture; whether reads, writes, process launch and network access are
      confined; every admitted external root; and an explicit `unknown` for any fact the host cannot
      prove. It derives these facts from the effective `ToolContext`/guarded `System`, not by
      re-reading environment variables into a second policy interpretation.
- [ ] Git context distinguishes repository top, current worktree root, primary versus linked
      worktree, Git common directory, worktree Git directory, branch/detached state, HEAD and bounded
      status. A linked worktree whose `.git` file points outside the checkout is described without
      requiring the model to probe that path, and secret-bearing config, remotes and credentials are
      never rendered. Failing-first context tests cover a primary checkout, linked checkout, bare/
      non-repository directory, dirty tree, detached HEAD and inaccessible metadata.
- [ ] Context is truthful after a runtime reroot or `git_worktree_enter`/`git_worktree_leave`: the
      next turn sees the new effective root and worktree identity. A deterministic turn fixture proves
      the model receives the confinement/topology block before tools and does not call shell, glob,
      stat or filesystem operations merely to discover whether `..`, an absolute path or the linked
      Git directory is allowed.
- [ ] The built-in Git family gains a read-only `git_worktree_list` operation returning a bounded,
      typed inventory with stable worktree identity, current/primary flags, branch or detached HEAD,
      commit, dirty/locked/prunable facts and managed/unmanaged ownership. It uses the guarded Git
      path and understands linked-worktree `gitdir`/`commondir`; it is not a shell-command template.
- [ ] The family gains `git_worktree_new` for one managed linked worktree rooted under Flux's pinned
      worktree base. The caller names a validated branch and start ref but never an arbitrary absolute
      destination. Option-shaped refs, `..`, symlink escapes, collisions, dirty/in-flight repository
      state and a sandbox that cannot admit the managed base refuse before mutation. Failure removes
      only allocations made by that call and never removes an existing branch or directory.
- [ ] The family gains `git_worktree_remove` by stable managed identity. It refuses the primary or
      current worktree, an uncommitted/untracked tree, locked or unknown worktrees and any path not
      proven to be Flux-managed; it has no force mode. Branch deletion is separate and safe-only, and
      partial cleanup is retryable without losing the checkout or its commit.
- [ ] A guarded `git_merge_tree` operation previews the merge of validated refs without changing an
      index, checkout, branch or worktree. It returns the merge base, clean/conflicted outcome,
      bounded conflicting paths and a result-tree identity when Git can produce one. Any object-store
      write is declared accurately in effects and permission subjects. Tests prove the caller's HEAD,
      index, working tree and registered-worktree set are byte-for-byte unchanged on clean, conflict
      and malformed-ref paths.
- [ ] All new operations run through `flux-runtime::Executor::dispatch`, use argv-only guarded Git IO
      with the context's workspace-pinned cwd and linked metadata allowances, and publish accurate
      risk, idempotence, effects and concrete permission subjects. The model never receives a generic
      command, cwd or path escape aperture. Existing `git_worktree_enter`, `git_worktree_leave`,
      `git_branch` and `git_merge` retain their contracts; the new operations share policy helpers
      rather than duplicating or weakening them.
- [ ] The public op catalog, Git group, generated Flux-Lang/website references and help explain the
      native names. Existing underscore names remain canonical (`git_worktree_list|new|remove`,
      `git_merge_tree`); a hierarchical presentation such as `git.worktree.list` may be generated by
      a frontend, but this story introduces no silent alias or breaking rename.
- [ ] Sandbox-backend tests run with outside-workspace access denied and prove both halves together:
      the first prompt already states that denial, impossible operations are unavailable or return the
      precomputed typed constraint without exploratory IO, and permitted managed-worktree operations
      complete without an escape attempt. Standard workspace build/test/clippy/fmt, `flux-codegate`,
      generated-reference checks, embedded-doc regeneration/check and website gate are green.

## Progress

- 2026-08-04 — filed from coordinator feedback after agents repeatedly spent shell calls learning
  worktree/sandbox constraints that the runtime already knew.

## Notes

- Existing startup seams: `crates/flux-runtime/src/context.rs` (`EnvContext`, `GitContext`,
  `Projector`) and `crates/flux-cli/src/execution.rs` where those providers are installed.
- Existing guarded Git/worktree seams: `crates/flux-tools/src/lib.rs`,
  `crates/flux-runtime/src/lib.rs::WorkspaceContext`, `flux-system` worktree-base allocation and
  [the context-local worktree design](../designs/context-local-git-worktrees.md).
- This is host-supplied execution context, not an instruction to make the model its own sandbox
  authority. The runtime remains authoritative and fail-closed when context and an attempted effect
  disagree.
