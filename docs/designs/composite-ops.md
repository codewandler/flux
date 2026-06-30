# Design: Flux-Lang composite ops

## Summary

Composite ops let a `.flux` module define reusable operations in Flux-Lang itself. A composite op is
a named, typed sub-flow that appears in the op catalog and can be called like any other op, but its
body is ordinary Flux-Lang: existing tool calls, control flow, context packs, model ops, and process
ops. It is not a plugin and it is not a Rust `Tool`; inner IO still crosses the existing
authorization -> approval -> guarded IO envelope.

## Shape

```flux
op repo_health(path: String, prior: Ctx) -> Health
  description "Check git state, run tests, and summarize failures"
  risk "medium"
  idempotency "idempotent"
  effects ["read", "process", "local_system", "model"]
  limits {dispatches: 20, timeout_ms: 120000, context_chars: 8000}
  expose true

  $status = git_status()
  $tests = cargo_test({args: ["--workspace"]})
  ctx $pack
    purpose "repo-health"
    budget 8000
    include $prior, $status, $tests
  $summary = ai.reason({ask: "Summarize repo health", ctx: $pack})
  return {status: $status, tests: $tests, summary: $summary}
```

Top-level `op` declarations live beside `agent`, `channel`, `datasource`, `trigger`, `journey`, and
`flow` declarations in `flux_lang::program::Program`. Their params and return type reuse the existing
flow header types. Metadata lowers to the same signature fields the planner already sees:
description, effects, risk, idempotency, and parameter JSON Schema.

## Runtime model

Composite ops are resolved before host dispatch. The interpreter maps positional arguments through
the composite signature, then runs the composite body in a scoped store overlay:

- params are seeded as hidden symbols;
- local binds stay in the overlay and do not leak to the caller;
- immutable values and run events are written to the parent store;
- evidence, approvals, redaction, cancellation, read-before-write state, and guarded `System` stay on
  the same host/executor;
- the caller receives only the composite return value.

This keeps composites explicit and hygienic while preserving the single safety envelope. A composite
cannot perform IO directly; it can only call ops that already exist.

## Agent registration

Agents can register one new composite op at runtime through the root op `op.register`. The input is
normalized Flux-Lang source containing exactly one top-level `op` declaration plus an explicit scope:

- `turn` installs the op for later planner iterations in the current turn only;
- `session` persists normalized source in the flow store for the current session;
- `project` writes `.flux/ops/<name>.flux` through the guarded workspace `System`;
- `global` writes `@global_ops/<name>.flux`, a named guarded root backed by `~/.flux/ops`.

Every registration is parsed and validated before it is installed. Existing active names require
`replace=true`, built-in tool names cannot be shadowed, and persisted definitions reload as normal
Flux-Lang source in later engines. Scope precedence is global -> project -> session -> turn, with later
scopes overriding earlier ones only when replacement was explicit.

## Safety rules

- `await` inside a composite is rejected for v1; composites are synchronous sub-flows.
- Direct and indirect recursive composite calls are rejected.
- Static analysis computes transitive effects/risk from the body and fails if declared metadata
  understates the body.
- Runtime dispatch remains the final safety floor; every inner real op still goes through
  `Executor::dispatch`.
- `op.register` itself dispatches through `Executor::dispatch`, declares write/filesystem risk, records
  an `op.registered` observation, and writes project/global definitions only via `flux_system::System`.
- `bash` remains group-gated. A new `proc.run` op provides the preferred generic argv-only process
  escape hatch.

## Implementation notes

The language layer owns the pure declaration and catalog seams. `flux-flow` merges parsed composite
definitions with the live `ToolRegistry` catalog for analysis, planning, optimization, and execution.
`flux-runtime::Tool` is intentionally unchanged: `Tool::execute` does not own the flow store/session/sink
needed for scoped sub-flow execution.
