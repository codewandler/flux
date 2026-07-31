# Harness route integrity — 2026-08-01

## Context

The two 2026-07-31 harness reviews produced **zero** board items; C-255 was scoped to the three
security reviews only. The validation pass confirms the substantive half of their complaints and
corrects the framing of the rest.

What exists today: `flow_run`, `flow_list` and `flow_render` are all registered and always
advertised (ungrouped ops are core). So "no flow runner was surfaced" is **not** reproduced
structurally. What *is* reproduced is sharper and worse:

- **`flow_run` has no path parameter.** Its input is `{name, inputs?}` with `deny_unknown_fields`,
  resolved only within `.flux/flows`, `.flux/ops` and their global twins. `examples/commit.flux` is
  unreachable from the op by construction, while the CLI `flux flow run <path>` resolves a file
  first. The documented dogfood invocation has no model-facing equivalent.
- **Discovery and execution live in different families.** `flow_list`/`flow_render` route to
  `workspace.read`; `flow_run`, having empty effect and access sets, falls through to `core`, whose
  description reads "Pure and generally useful deterministic operations." A `Risk::Medium`,
  `NonIdempotent` flow runner is not that.
- **No preflight exists anywhere.** Lowering happens inside `run_authored_flow` — after the decision
  to execute. There is no pre-mutation answer to "is this flow runnable here".
- **Nothing binds completion to a route.** `required_route`, `substitution_allowed` and every
  synonym return zero source hits. `ExecutionReport`/`ActionResult` carry `{id, op, status, result}`
  with no route identity, and `agent-loop.flux` never compares receipts against the declaration.
  The only guard is one sentence of prompt prose.
- **`examples/commit.flux` has never been executed by a test.** `examples_validate.rs` is parse +
  lower against a `NullProvider`; nothing covers git output, index refusal, approvals, commit
  creation or rollback.
- **ROUTE-01 half-landed, untracked.** Commit `edcd9dcc` enriched `declare_intent` with
  `task_kind`, `effect_mode`, `deliverable`, `constraints`, `uncertainties` and
  `capability_families` — exactly the direction H-1 endorsed — with no story, no board row, no
  CHANGELOG entry and no test named after the property it exists to improve. Meanwhile
  `Family.routing_signals` is wired end to end but populated only from `KIND_TURN_INTENT` matchers,
  which **no first-party group emits**; only installed plugins reach it.

## Finding-to-story traceability

| Residual | Story |
| --- | --- |
| `flow_run` cannot address a workspace path; its result is not a route receipt | C-376 |
| The flow runner routes to `core` under a description that says "pure deterministic" | C-377 |
| No exact-flow preflight separating inspectable from executable | C-378 |
| No route field in the intent contract and no completion check against it | C-379 |
| `examples/commit.flux` is never executed by any test | C-380 |
| No routing measurement; no first-party routing hints; `edcd9dcc` untracked | C-381 |

## Decisions

- **Progressive narrowing is not the defect and is not up for revision.** H explicitly rejects an
  ambient all-tools catalog and nothing here proposes one. The work is routing *quality* and route
  *evidence*, inside the staged design.
- **A family description is a routing input.** Classifying a `Risk::Medium` non-idempotent runner as
  "pure deterministic" misroutes by construction; the virtual-family fallback should not be able to
  produce that pairing.
- **A route-specific task needs a route receipt or an explicit substitution decision.** Reaching an
  equivalent end state by other means is a legitimate outcome — reporting it as the requested route
  is not.
- **The path/name asymmetry is a recorded consequence of L-79, not an oversight.** C-376 revisits
  that decision deliberately, workspace-confined, rather than treating it as a bug fix.
- **Static parse/lower is never evidence of execution.** Where a preflight reports on a flow, its
  output separates `inspectable` from `executable` as distinct fields so the distinction cannot be
  flattened in prose.
