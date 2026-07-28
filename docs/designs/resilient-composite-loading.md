# Resilient persisted-composite loading — prune unresolvable ops instead of failing assembly

Status: proposed (story C-117)
Owner seams: `crates/flux-flow/src/composites.rs`, `crates/flux-flow/src/engine.rs`, `crates/flux-flow/src/registry.rs`, `crates/flux-orchestrate/src/lib.rs` (tests only)

## Problem

Engine assembly validates **every** persisted composite op against the engine's own tool registry,
all-or-nothing, and hard-fails on the first unresolvable reference:

- `DynamicComposites::load` pulls composites from the unified flows home — `@global_flows`
  (`~/.flux/flows`) + `.flux/flows`, plus legacy `@global_ops` / `.flux/ops`
  (`crates/flux-flow/src/composites.rs:76-98`).
- `FlowEngine::assemble_with_loop` then calls `composites.validate_base(executor.registry())?`
  (`crates/flux-flow/src/engine.rs:304`), which validates the merged global+project set via
  `analyze_composites` and turns any diagnostic into a fatal `Err`
  (`crates/flux-flow/src/composites.rs:100-106`, `:305-314`).

Two blast radii, both observed or directly implied:

1. **Sub-agent delegation is bricked wholesale.** A child engine's registry is the base narrowed by
   role `tools` ∩ the caller's cap scope (`crates/flux-orchestrate/src/lib.rs:310-322`), so it
   contains no plugin ops and no cognition ops unless the role asks for them. A single global
   composite that uses any of those (live repro: `~/.flux/flows/mr_update.flux` calling
   `gitlab.mr.show`, `ai.reason`, `gitlab.mr.update`) makes **every** spawn of **every** role fail
   at child-engine assembly with `composite validation failed: unknown operation: …` — before the
   child's first turn, regardless of the task.
2. **Top-level startup has the same landmine.** The same `validate_base` guards the parent engine,
   so a global composite referencing an uninstalled plugin's ops bricks `flux` in every workspace
   on that machine until the file is deleted.

The envelope is doing its job (the ops genuinely aren't in the registry); the failure is that an
unrelated, *inactive* persisted definition is allowed to abort assembly at all.

## Design stance

Extend the lenient-loading philosophy that already governs this exact directory. `load_flows_dir`
deliberately skips unparseable files so "a single malformed file never breaks startup" — the file
stays runnable via `flow_run`, which surfaces the real error lazily
(`crates/flux-flow/src/composites.rs:249-264`). Resolvability should work the same way:

> A persisted composite that cannot resolve against *this* engine's registry is simply not part of
> this engine's catalog. It is excluded with a visible record — never a boot failure.

This mirrors how tool narrowing already behaves (a role that can't see `gitlab.*` just doesn't get
those tools; nothing errors) and how `OpRegistry::with_advertised` narrows the catalog without
breaking resolution (`crates/flux-flow/src/registry.rs:60-68`).

Safety: pruning only ever **narrows** the model-visible catalog. A pruned composite was never
dispatchable on this engine anyway (its callee ops don't exist in the registry); nothing new is
authorized, and every surviving composite still executes through the same envelope.

## Mechanism

### 1. `DynamicComposites::prune_unresolvable`

New method replacing `validate_base` at the assembly seam:

```rust
pub struct PrunedComposite {
    pub name: String,
    pub scope: &'static str, // "global" | "project"
    pub reason: String,      // joined diagnostics, e.g. "unknown operation: gitlab.mr.show (at body[0].value)"
}

pub fn prune_unresolvable(&self, tools: &ToolRegistry) -> Vec<PrunedComposite>
```

Operates on `st.global` + `st.project` (the only scopes populated at assembly; session/turn maps
are empty then). Algorithm — a fixed point, because composites may call composites (the L-30
transitive concern):

1. Let `remaining` = merged active set.
2. For each composite `c` in `remaining`, collect its individual diagnostics against the catalog
   `tools` + `remaining` (structural `lower`, await check, risk/effect surface check, registered-
   tool name conflict — the per-op body of today's `analyze_composites`,
   `crates/flux-flow/src/registry.rs:194-250`). Cycle detection runs on `remaining`; every cycle
   participant counts as invalid.
3. Remove all invalid composites from `remaining` (and from the corresponding state map), recording
   name/scope/reason.
4. Repeat until a pass removes nothing. Terminates: each round removes ≥ 1 entry. Cost is a few
   pure `lower` calls over a handful of definitions — negligible at assembly time.

Dropping a callee can invalidate its callers, which the next round catches — that is the point of
the fixed point (prune `mr_update`; a `review_all` that called it prunes in round 2).

Refactor to share logic instead of duplicating it: extract the per-composite diagnostic collection
out of `analyze_composites` into a private `analyze_one(op, catalog) -> Vec<Diagnostic>`;
`analyze_composites` keeps its exact current behavior (set-level, all-or-nothing) for the callers
that *should* stay strict.

### 2. Engine assembly consumes the prune

`crates/flux-flow/src/engine.rs:304` becomes:

```rust
let pruned = composites.prune_unresolvable(executor.registry());
```

Assembly no longer fails on persisted composites. The pruned list is stored on the engine, and at
turn start — next to the existing `turn.identity` observation (`crates/flux-flow/src/engine.rs:402`)
— a `composites.pruned` observation is emitted when the list is non-empty, carrying
`[{name, scope, reason}]`. It rides the existing durable observation flush, so the exclusion is
auditable per session, not silent.

Downstream this is automatically consistent: `active_for_session` no longer returns pruned entries,
so they never reach `validate_agent_loop` (`engine.rs:306-310`), the per-turn execution set
(`engine.rs:743-751`), or the model-facing `OpRegistry` catalog built via `with_owned_composites`
(`engine.rs:1726-1728`).

`validate_base` has no remaining callers after the cutover (grep: only `engine.rs:304` plus stale
`.claude/worktrees/*` clones) — delete it and flag the removal in the CHANGELOG as breaking
(pre-1.0 minor-bump signal per the release rules).

### 3. What deliberately stays strict

- **`op.register` / `validate_registration`** (`composites.rs:165-190`): registering a *new* op
  still fails loudly on unknown callees — that's live user feedback, not stale state. Side benefit:
  because broken persisted entries are pruned from the active map before any registration, an
  unrelated broken global file no longer fails `validate_registration`'s whole-set candidate check.
- **`validate_agent_loop`**: an authored agent loop referencing missing ops is a build defect and
  keeps hard-failing.
- **Module installs / `flow_run`**: `analyze_composites` behavior unchanged; running a pruned op's
  file via `flow_run` surfaces the genuine unknown-op error lazily, exactly like the malformed-file
  precedent. `flow_list` reads the files directly and still lists them.
- **Session-scoped composites** (`ensure_session_loaded`): unchanged (parse-strict; they were
  validated against their own engine's registry at registration time). Out of scope.

## Alternatives considered

- **Validate but only warn, keep entries in the catalog** — rejected: an advertised op that cannot
  resolve produces guaranteed downstream plan failures and violates catalog honesty.
- **Filter at `active_for_session` per call** — rejected: the registry isn't available there, and
  repeating the analysis per turn does at runtime what one assembly-time fixed point does once.
- **Only prune for sub-agent engines, keep top-level strict** — rejected: the top-level engine has
  the identical failure shape (uninstalled plugin), and two validation policies on one seam is
  exactly the kind of divergence the one-loop architecture avoids.

## Test plan (failing-first)

1. `flux-flow` unit (`composites.rs`): global entry whose body calls an unregistered op → pruned
   with a reason containing `unknown operation`; a valid sibling survives; `active_for_session("")`
   excludes the pruned name.
2. Transitive fixed point: `A` (valid, calls `B`) + `B` (calls a missing op) → both pruned.
3. Engine seam (the bug's exact shape): temp workspace with `.flux/flows/broken.flux` referencing a
   nonexistent op → `FlowEngine::assemble` succeeds (fails today with `composite validation
   failed`), and the first turn emits `composites.pruned`.
4. `flux-orchestrate` integration (the live repro): temp workspace + persisted composite requiring
   ops outside a `tools: [read]` role's narrowed registry → `LocalSpawner::spawn` succeeds and
   returns the scripted text (modeled on `spawner_runs_a_role_and_returns_text`,
   `crates/flux-orchestrate/src/lib.rs:1335`).
5. Strictness pin: `validate_registration` still rejects a new composite naming an unknown op.

## Rollout

- CHANGELOG (engineering): fix entry under C-117 + the `validate_base` removal flagged breaking.
- WHATS-NEW (customer, Fixed): "A saved flow or op that needs an integration you don't have
  installed no longer blocks the agent — or its sub-agents — from starting; it's simply not offered
  until its operations are available."
- Module docs: update the `composites.rs` header to state the resolvability policy alongside the
  existing malformed-file policy.
- Immediate operator workaround until the fix ships: move the offending file out of
  `~/.flux/flows/` (it stays runnable via `flow_run` from anywhere).
