# Design: Context-local Git worktrees

**Status:** implemented 2026-07-28 · **Pillar:** Core · **Stories:** [C-97](../stories/C-97-workspace-context-runtime-seam.md) · [C-98](../stories/C-98-git-worktree-enter-op.md) · [C-99](../stories/C-99-git-worktree-leave-op.md) · [C-100](../stories/C-100-worktree-engine-and-subagents.md)

## Why

Agents that mutate a repository while the user (or another agent) works in the same checkout step on
each other. Git worktrees are the natural isolation primitive, but flux has no built-in way for an
agent to *enter* one — and a naive implementation would call `std::env::set_current_dir`, which is
process-global and would leak the transition into every other agent context sharing the process.

This epic adds `git_worktree_enter {}` / `git_worktree_leave {}` as guarded Git built-ins that move
**only the calling agent context** into a temporary worktree (under a private on-disk
directory — `$FLUX_WORKTREE_DIR`, defaulting to `~/.flux/worktrees` — writable, outside the
original PWD), and on leave merge the work back into `main` and restore that
context's original root. The per-agent workspace state this requires — a context-owned, swappable
active `System` — is a runtime seam with value beyond worktrees.

Plan provenance: distilled from a decision-complete Plan Mode session (codex,
2026-07-27/28); the plan resolved all lifecycle and failure-boundary decisions listed below.

## Approach

**Runtime seam (C-97).** A `WorkspaceContext` owned by `ToolContext`:

- a snapshot-able active `Arc<System>`;
- optional worktree session state: original `System`, captured `main` commit, generated branch,
  worktree path, and cleanup phase;
- no nesting in v1 — `enter` while a session is active returns a recoverable error.

Direct `ctx.system` use is replaced by an active-system accessor/snapshot so every filesystem,
process, flow, and toolchain operation uses the context's current root (~64 call sites, mostly in
`flux-tools/src/lib.rs` — mechanical). A tool already running keeps its initial snapshot; later
calls see the transitioned root. Never `set_current_dir` — no process-global state changes
(verified: production code never calls it; spawn cwd flows solely through
`System::build_command`'s `current_dir(workspace.root())`).

**Plugin ops are explicitly out of scope in v1**: `PluginTool` executes through the `PluginHost`'s
own `System` captured at subprocess spawn (and `SystemHostCaps` captures another assembly-time
`Arc<System>`), so already-spawned plugin subprocesses keep the original root. Documented
limitation; follow-up story C-122 tracks the re-spawn/notify design that lifts it.

Guarded `flux-system` helpers: derive a new guarded `System` rooted at an existing worktree while
retaining the source sandbox and configured access posture; create and clean a private
`flux-worktree-*` parent directory (under `$FLUX_WORKTREE_DIR` / `~/.flux/worktrees`, C-120)
through guarded system IO. No tool touches raw filesystem or
process APIs.

**`git_worktree_enter {}` (C-98).** Requires a Git repository, a clean non-detached checkout on
`main`, and no active worktree session. Captures `main`'s `HEAD`, generates a collision-resistant
`flux/worktree/...` branch, runs `git worktree add -b <branch> <tmp>/checkout <captured-head>` via
argv-only `System` execution, then switches this context to the equivalent relative directory in the
new checkout (or the worktree root). Returns the branch and worktree path.

**`git_worktree_leave {}` (C-99).** Requires a clean (committed) worktree — it never stages or
commits automatically. Verifies original `main` is still clean, checked out, and at the `enter`
commit; otherwise leaves the context untouched. Performs a no-commit merge trial and aborts it on
conflict (proving the real merge cannot leave `main` conflicted), then merges with
`--no-ff --no-edit`, removes the worktree, deletes the merged branch, and restores the original
context only after successful cleanup. On merge failure the agent stays in its worktree with a clean
original checkout preserved. Partial cleanup records a recoverable cleanup-pending state with
precise diagnostics; retrying `leave` completes cleanup without re-merging.

Both ops are Git-group members, high-risk and non-idempotent, with explicit non-empty permission
subjects and process/local-system effects — authorization → approval → guarded IO stays mandatory.

**Engine + sub-agents (C-100).** `FlowEngine` probes the context's active root each turn for
tool-group discovery rather than its assembly-time `cwd`. Project configuration, agent role,
permissions, and loaded skills remain fixed for the session — entering a worktree changes the
working directory, not the agent's authority. `SpawnRequest` / the local spawner give a child a copy
of the parent's active-system snapshot but its own independent `WorkspaceContext`; a child
transition never changes its parent.

## Alternatives considered

- **`std::env::set_current_dir` on enter** — process-global; leaks the transition across every agent
  context in the process. Rejected outright.
- **Globally mutable `System`** — would make every executor observe another agent's transition; the
  context-owned session with snapshot semantics keeps isolation explicit.
- **Caller-supplied branch names / non-`main` targets** — deferred; v1 generates internal branch
  names and integrates only into `main`.

## Feasibility review (2026-07-28, grounded in code)

Confirmed viable:

- `System` is `Clone`, immutable after construction, held as `Arc<System>`; `Workspace` jails paths
  to a canonicalized root plus named/read roots, and nothing forbids a `/tmp` root (flux-eval
  already roots `System`s in temp dirs). The missing piece is a derive helper — today's
  constructors lose either the sandbox (`System::new`) or the extra roots (`System::from_env`).
- The OS sandbox needs **no rule changes**: `SpawnPolicy::for_workspace` derives the writable set
  from `workspace.root()` (so a swapped root is bound automatically), `/tmp` is unconditionally
  writable, and `linked_worktree_writable_roots` already admits a linked worktree's external
  `.git/worktrees/<id>` + commondir.
- Permission subjects are re-normalized per call against the *active* workspace
  (`Executor::gate` → `path_identity`, workspace-relative when under the root), so relative allow
  rules keep matching inside the worktree; without a swap the gate hard-denies rather than
  escaping. Default policy grants use `path: "*"`, so `/tmp` paths are not denied by the floor.
  Session "always allow" is tool-scoped, not path-scoped — approvals survive the transition.
- Surfacing is already re-probed per turn (`surfaced_for_turn` → `surfaced_op_names(&self.cwd)`);
  only the root argument needs to come from the active system.
- `ToolContext` is the right cross-turn home — it already carries `read_times`/`evidence`/
  `cap_scopes` as shared-mutable session state, and `into_executor` is the single threading point
  (both the fresh-context branch and the `exact_context` bridge).

## Risks & open questions

- The `ctx.system` → accessor sweep touches every built-in tool; in-flight-snapshot semantics must
  be tested, not assumed. It is a source-breaking public-API change (a `pub` field becomes an
  accessor) across ~8 crates.
- The `System` derive helper **must preserve named roots (`@global_flows`!), read roots
  (`FLUX_ADD_DIRS`), the unconfined flag, and the sandbox** — dropping named roots would break
  `flow_run`/global ops inside the worktree.
- Cleanup-pending recovery (merge succeeded, worktree/branch removal failed) needs explicit state so
  a retry never re-merges.
- **Stale system-prompt project context**: `EnvContext`/`GitContext`/`RepoSignal`/`ProjectFiles`
  are rendered once at assembly, so after `enter` the model is told the original root's cwd/branch
  while its tools operate in the worktree. Mitigation: op results state the new root prominently,
  and the `cwd` op (in `DEFAULT_ALLOW`) reads the active system post-sweep. A turn-scoped context
  note is a candidate follow-up.
- The spawner (`LocalSpawner`) holds its own assembly-time `Arc<System>` and the child engine's
  `cwd` defaults to `"."` today (a latent bug — children probe the *process* cwd). The
  `SpawnRequest` snapshot must also fix `spec.cwd`, and nested delegation (`at_depth`) must carry
  the child's own snapshot, not the grandparent's.
- Sticky-monotonic surfacing means groups surfaced by worktree-local signals stay advertised after
  `leave` (advertising is not granting — acceptable, but document it).
- On the server/app, one `ToolContext` per engine means the worktree session is shared by every
  conversation on that engine; "context" == engine there. Correct for the CLI (one engine per
  process); C-97 states this scope explicitly.
- `SubAgentApprover` denies destructive intents outright, so children cannot enter/leave worktrees
  without an injected approver — intended for v1 (children inherit a snapshot; they don't
  transition unless their surface grants approval).
- User-authored `[policy]` grants with concrete path globs rooted at the original checkout would
  deny worktree paths — a documented consequence, not a code fix. `persist_allow_rules` writes to
  the original project's `.flux/config.toml` (process cwd), which is correct: authority stays with
  the original root.
- **Worktree parents must live on real disk (C-120).** The original `/tmp` choice failed in
  practice: `/tmp` is commonly a RAM-backed tmpfs (32 GB on the dev machine), and a build inside
  an entered worktree writes a multi-GB `target/` there — observed filling the tmpfs and breaking
  every process needing `/tmp` during the epic's own merge verification. Allocation now defaults
  to `~/.flux/worktrees` with `$FLUX_WORKTREE_DIR` as the override; the temp dir is only a
  last-resort fallback when `$HOME` is unset.

## Acceptance / done

- Two contexts sharing one repository: entering/leaving a worktree in one changes neither the other
  context nor process-wide PWD (failing-first).
- Real temporary-repository round trip: enter → edit/commit → leave → `--no-ff` merge into `main` →
  worktree and branch removed → original context restored.
- Rejections covered: dirty/non-`main` entry, nested enter, dirty worktree, moved/dirty original
  `main`, merge conflict with clean abort, each cleanup-pending recovery state.
- Child agents inherit the parent's active-root snapshot but transition independently.
- Both ops in the Git group, registry/catalog tests, `crates/flux-flow/docs/ops-reference.md` rows,
  CHANGELOG + WHATS-NEW entries; full repository gate green.
