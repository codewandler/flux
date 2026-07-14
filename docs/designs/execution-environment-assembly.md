# Shared execution-environment assembly

**Status:** implemented (2026-07-14) · **Story:**
[C-67](../stories/C-67-centralize-execution-environment-assembly.md)

## Decision

`flux_runtime::ExecutionEnvironment` is the mechanical assembly seam shared by CLI, App, SDK,
`AgentSpec`, and lazy agents. A surface must decide and pass the complete environment:

- one explicit guarded `Arc<System>` and therefore one `Workspace` identity;
- the requested `ToolRegistry`, including already-resolved plugins, endpoints, datasources, and
  surface-only operations;
- permission rules and the surface approver;
- the mandatory `ExecutionAuthorization` policy/identity profile;
- optional redactor, sub-agent spawner, pre-tool hooks, and lexical runtime-turn context.

The environment produces the `Executor` and its `ToolContext` from those same inputs. It never reads
the process current directory, discovers plugins, or silently chooses a second workspace. Cloning an
environment preserves the resolved root and safety profile; lazy construction therefore cannot drift
after a caller changes `cwd`.

## Ownership split

Surfaces still own product choices. The CLI resolves project config and installs its rich plugin,
endpoint, datasource, web, evaluation, and development catalog. SDK builders choose the public SDK
defaults and custom operations. App installs app/journey capabilities and cached agent definitions.
`ExecutionEnvironment` owns only the invariant-preserving conversion of those choices into an
executor.

`AgentSpec::assemble_in` applies the agent's declared tool subset and permission rules, then restores
the canonical authored-loop control-plane operations through the one checked installer. User
selection may narrow ordinary capabilities but cannot remove or shadow the engine's private control
plane. Duplicate operation registration remains an error; intentional replacement uses the explicit
runtime replacement API.

## Compatibility

Existing convenience constructors delegate to this path and use the documented local authorization
profile. Deprecated shims that receive an already-built `ToolContext` use
`ExecutionEnvironment::from_context`; new integrations use the explicit `System` constructor.
Nominally fallible builders propagate invalid workspace, catalog, plugin, and configuration errors
rather than falling back to a smaller or differently rooted environment.

## Parity proof

Cross-surface tests request the same tool set and compare operation names, permission behavior,
authorization denial, workspace root, and control-plane availability across CLI, SDK, App, and
`AgentSpec`. Separate regressions change process `cwd` after construction and prove direct and lazy
agents still read and execute only in the originally supplied workspace.
