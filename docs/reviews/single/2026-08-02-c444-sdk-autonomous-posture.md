---
title: C-444 SDK autonomous-posture envelope review
date: 2026-08-02
kind: subsystem-review
lens: envelope-integrity
method: Desk review of the C-444 branch against main, plus targeted SDK/orchestrator tests and one mutation proof; no fuzzing, exploitation, live-provider calls, or live-infrastructure testing
reviewer: agent
triage:
  kind: single
  status: open
  owner_stories: [C-444, C-470, C-471]
  aggregated_into: null
subject:
  repo: codewandler/flux
  version_in_tree: 0.49.0
  published_release_at_review: v0.49.0
  workspace_crates: 38
overall_rating: 8/10
verdict: Mergeable after the full gate: autonomy now carries confinement and finite tree ceilings through both SDK doors, including opaque custom approvers.
ratings: { security_architecture: 9, secure_defaults: 8, implementation_quality: 8, security_assurance: 8, release_supply_chain: 8, product_maturity: 7, community_bus_factor: 5, production_readiness: 8 }
verification:
  status: verified against tree at 0.49.0 on 2026-08-02
  outcome: two material review gaps were corrected and pinned before integration; no release-blocking finding remains in scope
  material_errors: the original branch report treated injected approvers as supervised and did not test the LocalSpawner census-admission wire
top_findings:
  - "Resolved: an opaque custom Approver could blanket-allow while retaining Off and unbounded defaults"
  - "Resolved: deleting LocalSpawner's census admission changed no pre-existing test"
  - "Residual product limitation: file configuration cannot yet select max_live_agents (C-471)"
---

# Verdict

C-444 is mergeable after its full gate. The reviewed tree couples an absent human boundary to
fail-closed process confinement and finite resource limits, and it does so through both public SDK
doors. The review changed the branch materially in two places: an injected approver is no longer
trusted to imply human supervision, and the tree-wide census is now pinned at the actual spawn seam.

This is an envelope review, not a claim that autonomous execution is risk-free. Explicit sandbox and
resource-limit overrides remain authoritative by design; authorization and deny rules remain the
non-bypassable floor.

# Ratings

| Axis | Rating | Evidence-based reading |
| --- | ---: | --- |
| Security architecture | 9/10 | One shared envelope resolves approval, confinement, and budgets for both SDK doors. |
| Secure defaults | 8/10 | Blanket and opaque approval policies resolve conservatively; explicit overrides remain possible. |
| Implementation quality | 8/10 | The execution semaphore remains per-agent while a refusal-only census bounds the tree. |
| Security assurance | 8/10 | Failing-first posture tests, cross-door equality, and mutation-pinned spawn admission cover the load-bearing seams. |
| Release supply chain | 8/10 | Outside this subsystem; inherited from the verified v0.49.0 baseline. |
| Product maturity | 7/10 | The autonomous preset is coherent, but `max_live_agents` is not file-configurable yet. |
| Community bus factor | 5/10 | Unchanged by this subsystem. |
| Production readiness | 8/10 | Safe default posture for unattended SDK use, subject to the documented explicit escapes. |

# Strengths

- `Envelope::needs_autonomous_floor` treats both blanket auto-approval and an opaque custom approver
  as potentially human-free, while explicit posture choices remain visible and authoritative
  (`crates/flux-sdk/src/envelope.rs:82`).
- Sandbox `Require` and `ResourceLimits::autonomous()` are resolved from that same predicate
  (`crates/flux-sdk/src/envelope.rs:106`, `crates/flux-sdk/src/envelope.rs:131`).
- The autonomous preset bounds per-agent execution, tree-wide live agents, retained results, and
  evidence payload (`crates/flux-runtime/src/limits.rs:283`).
- A child keeps an independent execution semaphore but shares the refusal-only agent census, avoiding
  the documented delegation deadlock while bounding total fan-out
  (`crates/flux-runtime/src/limits.rs:353`).
- The client integration test reads the binding guarded `System`, not a builder input
  (`crates/flux-sdk/tests/secure_defaults.rs:68`), and the crate-internal cross-door test compares the
  resolved sandbox and every resource-limit field (`crates/flux-sdk/src/flow.rs:1204`).

# Findings

## F1 — Resolved during review: a custom always-Allow approver escaped the floor

The original branch classified autonomy as `auto_approve && approver.is_none()`. A public custom
approver can return `Allow` for every request, so that predicate recovered the exact unconfined,
unbounded posture C-444 was meant to remove. The failing-first test observed `Off` before the repair.

The reviewed predicate now applies the conservative floor whenever `auto_approve` is set **or** an
opaque approver is injected (`crates/flux-sdk/src/envelope.rs:82`). The regression test constructs the
minimal blanket-Allow policy and asserts `Require` plus bounded resources
(`crates/flux-sdk/tests/secure_defaults.rs:140`).

## F2 — Resolved during review: census enforcement was stored but not wiring-pinned

`ResourceLimits` had thorough census mechanics tests, but no test observed the call that admits a
child at the actual `LocalSpawner` boundary. Deleting that call left the pre-review suite green.

The reviewed tree admits before provider construction (`crates/flux-orchestrate/src/lib.rs:335`) and
has a direct test proving a ceiling of one refuses the child before the provider factory runs
(`crates/flux-orchestrate/src/lib.rs:1467`). Mutation verification deleted the admission call, observed
that test fail because the child completed, restored the call, and observed it pass.

# Open questions

- C-471 tracks the remaining operator surface: `[limits]` cannot currently configure
  `max_live_agents`. This does not reopen the SDK default—the autonomous preset supplies eight—but it
  prevents a file-configured host from selecting its own tree ceiling.
- The preset values (16 concurrent calls per agent, eight live agents, 64 MiB result retention,
  32 MiB evidence retention) are policy defaults rather than workload-derived tuning. They are finite
  safety ceilings, not a capacity recommendation.

# Deployment recommendation

Run the normal workspace gate and both CI sandbox postures. If green, integrate C-444 as a breaking
pre-1.0 change, include an action-needed migration note, and cut a minor release. An embedder relying
on an outer sandbox or intentionally unbounded resources must state that choice explicitly.
