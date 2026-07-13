---
title: Saved flows and custom operations
description: "Reuse deterministic Flux-Lang behavior through project/global flow catalogs and scoped composite operations."
---

# Saved flows and custom operations

Saved flows let people and agents reuse a reviewed Flux-Lang plan without asking a model to compile
it again. Composite operations let a module give a meaningful name to a reusable sub-flow.

## The flows home

Place `.flux` files in:

| Location | Scope |
|---|---|
| `.flux/flows/` | current project; wins name collisions |
| `~/.flux/flows/` | user-global |

Each file may contain a flow, a composite `op`, or a module containing both. The legacy
`.flux/ops/` and `~/.flux/ops/` directories remain readable, but new authored files should use the
unified flows home.

```flux
op repo-state -> String
  description "Return the current git worktree state"
  effects [read, process]
  expose true
  return git_status()

flow release-note(version: String) -> String
  $status = repo-state()
  return fmt("release {version}\n\n{status}")
```

Composite calls do not bypass safety. Every inner operation is analyzed and dispatched through the
same authorization, approval, and guarded-IO path.

## Run from the terminal

```bash
flux flow list                         # alias: flux flow ls
flux flow run release-note --arg version=0.14.5
flux flow run release-note --inputs '{"version":"0.14.5"}'
flux flow run release-note --map-inputs "write notes for 0.14.5" -m sonnet
```

Files win over catalog names during resolution. Declared inputs are strict: unknown, missing, or
concretely mistyped values fail before effects. Repeatable `--arg` overrides `--inputs`, and the last
duplicate argument wins. `--map-inputs` is the only mode that asks a model to fill missing values;
fully supplied deterministic flows require no provider credential.

## Run from an agent

The agent sees the same catalog through:

- `flow_list()` — names, descriptions, parameters, composite ops, and parse errors;
- `flow_run({name, inputs?})` — execute a saved flow inside the current session.

The catalog is loaded once for a run. A malformed file is reported without making every other saved
flow unavailable.

## Register an operation during a turn

`op.register({source, scope, replace?, expose?})` accepts source containing exactly one top-level
composite `op`, analyzes it against the live operation catalog, and installs it only if valid.

| Scope | Lifetime / storage |
|---|---|
| `turn` | current turn only |
| `session` | subsequent turns in the current session; stored in session state |
| `project` | persisted as `.flux/ops/<name>.flux` |
| `global` | persisted in the configured global ops root (`~/.flux/ops`) |

Session scope is the safe default for agent-created vocabulary. Project/global scopes are guarded
filesystem writes. `replace` must be explicit when a name already exists. `expose` overrides the
declaration's `expose` metadata: exposed ops enter model-stage catalogs; unexposed ops remain callable by
other declarations that already know their name.

For authored, reviewed definitions, prefer committing them under `.flux/flows/`; reserve
`op.register` for vocabulary created as part of an active session.

## Related docs

- [Tooling](../language/tooling.md) — execution and input details.
- [Modules, composite ops & programs](../language/modules-and-programs.md) — declaration syntax.
- [Operations](../language/ops.md) — `flow_list`, `flow_run`, and `op.register` signatures.
- [Time Machine](./time-machine.md) — resumable flow execution and cassette capture.
