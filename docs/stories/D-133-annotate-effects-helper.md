---
id: D-133
title: annotate_effects — per-node effect/risk annotation over an analyzed flow
pillar: Language
status: backlog
epic:
design:
note: "downstream nice-to-have (ai-agent-platform flows arc): HirFlow.effects is the flow-level union; consumers wanting per-node badges re-derive via for_each_node + OpCatalog::lookup"
---

# annotate_effects — per-node effect/risk annotation over an analyzed flow

## Goal
A small flux-lang helper: `annotate_effects(ast, ops) -> Vec<(node_path, OpSignature)>` (shape TBD) —
walk a flow and return, per `Call` node, the op's `{effects, risk, idempotency}` keyed by node path.
Today `HirFlow.effects` carries only the **deduped flow-level union** (right for the approval
envelope, lossy for attribution); a consumer that wants per-node badges (e.g. a visual editor pinning
`Money`/`High` nodes) must hand-roll the walk over `analyze::for_each_node` + `OpCatalog::lookup`.

## Acceptance
- [ ] `annotate_effects` (module TBD — `analyze` is the natural home) returns per-call-node
      annotations keyed by the same node-path convention diagnostics use (`body[3].then[1]`).
      Failing-first: a flow with one read + one `Money`-effect write annotates exactly the write node
      with `Money` + its risk tier.
- [ ] Unknown ops annotate honestly (absent/unknown, matching analyze's unknown-op diagnostic) rather
      than being silently skipped. Docs on the docs.rs surface. Gate green.

## Progress
- 2026-07-10 — filed from the ai-agent-platform flows-arc design as a **nice-to-have** (their A-11
  validate endpoint hand-rolls the walk in the meantime — public API suffices; this is
  dedup-across-consumers, not a blocker).

## Notes
- The private `gather_effects`/`host_effect_to_flow` in `analyze.rs` show the existing mapping; this
  helper is the per-node (attributed) sibling of that union.
