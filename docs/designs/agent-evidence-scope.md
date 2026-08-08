# Agent evidence scope

**Status:** proposed · **Epic:** [C-606](../stories/C-606-agent-evidence-scope-epic.md)

## Why

Flux governs what an agent may *do* and what it is *told*, but not what it may *look at*.

Two of the three scopes are mature. **Operation scope** is intersecting and transitive:
`Executor::push_cap_scope` intersects with the top of the stack, pops via RAII, records
`cap_scope_enter`, and `at_depth` rebases a grandchild spawner on the already-narrowed registry, so no
descendant can resurrect a tool an ancestor removed. **Context scope** is real too: a child gets a
fresh conversation, a fresh evidence log, no repository policy layers, and an authored `ai_segment`
validates its declared ceiling ⊆ the live ceiling before installing it.

**Evidence scope — the paths an agent can read and write — has no equivalent.**

`Workspace` has no narrowing operation. `add_read_root`, `add_named_root` and `set_unconfined(true)`
widen; `with_root` is documented as deliberately posture-*preserving* ("would drop the widened roots and
so break `@named`-root operations"). There is no `remove_read_root`, no `narrowed()`. And
`SpawnRequest` has nowhere to carry one: a `task` child receives the parent's `Arc<System>` verbatim —
same root, same read roots, same `unconfined` flag. So on a `flux --add-dir ~/other-repo` invocation
every sub-agent can read that repository, with nothing recording that it was granted.

This surfaced from an operator watching a Fleet worker's transcript: sibling worktrees, other agents'
session stores and fleet state logs should not be inspectable — and beyond confidentiality, a reachable
sibling is an invitation to wander out of the assignment, which is what makes a worker expensive and
unreliable. Measured on wave-286: 35 `read` and 14 `grep` calls against 25 `edit`s.

## What the audit established

**The `Workspace` guard itself is strong.** `resolve_in` refuses `..`, absolute paths, and symlink
escapes; `resolve_within_root` walks component-by-component using `symlink_metadata` (so a *dangling*
symlink to an outside target is also caught), bounded at 40 hops, followed by a second-order
`canonicalize_existing_ancestor` check. Containment is component-wise, so `stories/C-5421` does not
match root `stories/C-542`. `walk_files` refuses symlink traversal outright.

**But it is the only read guard, and three things puncture it.**

1. **`shell` bypasses it entirely.** `bash` and `proc.run` take arbitrary argv and never call
   `Workspace::resolve*`. `DEFAULT_STORY_CAPABILITIES` includes `shell`.
2. **The OS sandbox does not restrict reads — on either backend.** Linux binds `--ro-bind / /`: the
   whole filesystem, read-only, *into* the sandbox, with write confinement layered on afterwards.
   macOS uses `(allow default)` and denies only `file-write*` and `network*`. The sandbox is not a
   second line of defence for reads; it is not a line of defence for reads at all.
3. **The sandbox is off for Fleet workers by default.** `Commands::Fleet(..)` sits in the
   unattended-exempt arm of `dispatch.rs`, under a comment stating "none of them starts a turn" —
   which is the one thing `flux fleet run` exists to do.

Together: while `shell` was granted, a worker had unbounded **read** authority over the whole host.

**Two shared channels sit outside every worktree and every fence.** `~/.flux/flows` and `~/.flux/ops`
are registered as `@named` roots by `workspace_with_flow_roots`, and `@named` roots are *write-capable*
— so `write("@global_flows/x")` needs only `read`+`edit`, is invisible to every diff, and is readable
by every concurrent worker on the host. `SpawnPolicy::for_workspace` adds `named_roots()` to the
sandbox writable set, so confinement does not remove it. Separately, `linked_worktree_writable_roots`
returns `vec![admin, common]` where `common` is the whole `.git`, so a worker's writable set includes
sibling workers' session stores under `<common>/worktrees/<other>/flux-fleet/sessions/`.

**Confinement is uneven across Fleet agent kinds.** Story workers are tight: `read_roots = []`, pinned
at admission and verified across every live agent record, with zero `--add-dir` in their argv. But
`main` and any `fleet spawn`ed agent take the **entire fleet root as their writable root** plus every
repository root as read roots — so they can read *and write* `state.json`, `events.ndjson` and every
worker worktree. `allow_ad_hoc_agents = true` in the live configuration.

**Fences are convention, and misaimed.** They are checked post-hoc in `fleet_handoff` against
`diff --name-only base..commit`. The `.git` fence can therefore never fire, because git does not report
`.git` paths as tracked changes. `.flux/fleet/**` is checked against diffs in the *worker's* repository
while the ledger it names lives in a different one. Template fences are silently dropped at handoff:
admission builds them with `normalize_fences`, handoff hardcodes its own list.

**Operator denials stop at the delegation boundary.** A sub-agent's executor is built with
`PermissionManager::new()` — empty — and no `with_disabled_ops`, so `[permissions] deny` and
`[tools] disable` do not reach children. An empty manager means every child call is `Ask`, and
`SubAgentApprover` allows anything non-destructive.

## Approach

Copy operation scope, in the one direction it does not yet go.

1. **A narrowing constructor on `Workspace`** that can only reduce: drop read roots not in the
   requested set, refuse to re-grant `unconfined`, keep the primary root at or below the current one.
   Its absence is what forces every workaround; nothing else here is possible without it.
2. **An optional evidence scope on `SpawnRequest`** that **intersects** the parent's view. Absent means
   unchanged behaviour. Same monotone rule as `push_cap_scope`, so nested delegation is transitive for
   the same reason A-25 made tools transitive.
3. **Record it like a capability scope** — a read-scope observation beside `cap_scope_enter`, and
   `subagent.trace` extended so a child's granted path scope is auditable after the fact. Refusals
   reuse `operation_unavailable_reason`'s vocabulary, so "why can't I read this" reads like "why can't
   I call this".
4. **Close the punctures first**, because they make the rest moot: no `shell` in the default story
   capabilities, a resolved sandbox posture for Fleet workers, no writable global flow/op roots for a
   worker, and a sandbox writable set narrowed to the git subpaths `git commit` actually needs.
5. **Make the declared Fleet scope the enforced one.** `capability_set_manifest` already digests exactly
   `{mode, capabilities, operations, writable_root, read_roots, fences}` at admission — the right
   bundle, snapshotted in the right place. Enforce fences at guarded path resolution (keeping the
   handoff check as defence in depth), and replace `Workspace::from_env(worktree)` with explicit
   construction so ambient `FLUX_ALLOW_ALL`/`FLUX_ADD_DIRS` cannot re-widen an admitted worker.

### Relationship to D-05

This **extends** `sub-agent-hardening.md` rather than contradicting it. D-05 deliberately made the
child's filesystem view identical to the parent's, with the isolation unit being the whole spawner —
"one account = one workspace-scoped `System` = one `LocalSpawner`" — and concluded "no `Spawner::spawn`
signature change is needed". That is sound for its stated goal: account A's child cannot read account
B's workspace. It simply never needed the case where a child should see **less than its parent**.
Item 2 is precisely that signature change, and it owes D-05 an explicit argument rather than a quiet
amendment.

### Related open work

Fold into rather than duplicate: **L-139** already carries the required sentence ("cannot smuggle an
unapproved read through prompt construction"); **A-127** already names the cross-process divergence, in
which a ceiling becomes "a request, not an enforcement" over the wire; **C-568**'s proposed
`AgentStartContract` already lists a capability ceiling and fences among the fields an agent start
should carry.

## Verification

Unit tests confirm the fix the author imagined. The load-bearing check is adversarial: dispatch two
workers and have one *attempt* to read the sibling's worktree, read the sibling's `events.db`, write
`@global_flows/probe`, and read `state.json`. Each must be refused at the operation, with the refusal
visible in its transcript.
