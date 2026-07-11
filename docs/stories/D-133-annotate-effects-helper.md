---
id: D-133
title: annotate_effects — per-node effect/risk annotation over an analyzed flow
pillar: Language
status: done
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
- [x] `annotate_effects` (module TBD — `analyze` is the natural home) returns per-call-node
      annotations keyed by the same node-path convention diagnostics use (`body[3].then[1]`).
      Failing-first: a flow with one read + one `Money`-effect write annotates exactly the write node
      with `Money` + its risk tier.
- [x] Unknown ops annotate honestly (absent/unknown, matching analyze's unknown-op diagnostic) rather
      than being silently skipped. Docs on the docs.rs surface. Gate green.

## Progress
- 2026-07-10 — filed from the ai-agent-platform flows-arc design as a **nice-to-have** (their A-11
  validate endpoint hand-rolls the walk in the meantime — public API suffices; this is
  dedup-across-consumers, not a blocker).
- 2026-07-11 — implemented. `pub fn annotate_effects(ast: &DraftAst, ops: &dyn OpCatalog) ->
  Vec<(String, Option<EffectAnnotation>)>` in `crates/flux-lang/src/analyze.rs`, alongside a new
  `pub struct EffectAnnotation { effects: Vec<FlowEffect>, risk: Risk, idempotency: Idempotency }`.
  Shape decided: a `Vec` (not a map) preserves visitation order and lets the same op appear multiple
  times at different paths; `Option<EffectAnnotation>` (not bare `EffectAnnotation`) lets an unknown
  op still get an entry — `None` — instead of being dropped from the list, per acceptance item 2. The
  walk (`annotate_node`/`annotate_body`) is a dedicated path-tracking traversal mirrored 1:1 against
  `check_node`/`check_body`'s existing path labels (verified arm-by-arm) and `for_each_node`'s child
  positions, reusing the private `Diags` path-accumulator so the node-path string is byte-for-byte
  the same convention `analyze_flow`'s diagnostics already render. `effects` per node folds
  `gather_effects`'s two contribution sources (op's own host effects via `host_effect_to_flow`, plus
  an immediately-enclosing `bind`/`memo`'s declared `effect` tag) attributed to that one call instead
  of deduped into the flow-wide union — this is what lets `Money` (which never survives the
  `FlowEffect -> flux_spec::Effect` host lowering on its own) show up on the write node when the
  author tags the bind with `effect: money`, exactly like `HirFlow::effects` already gathers it today.
  Two new tests in `crates/flux-lang/src/analyze.rs` (`analyze::tests` module):
  `annotate_effects_attributes_money_to_exactly_the_write_node` and
  `annotate_effects_honestly_flags_unknown_ops_instead_of_skipping`. Verified failing-first by
  temporarily dropping the enclosing-effect fold-in and confirming the Money test fails for the right
  reason, then restoring. Gate: `cargo test -p codewandler-flux-lang` (338 passed, 0 failed — the
  pre-existing, unrelated `website_customer_changelog_is_in_sync` whats-new.md-drift failure is
  present and untouched), `cargo clippy -p codewandler-flux-lang --all-targets -- -D warnings` (clean),
  `cargo fmt -p codewandler-flux-lang --check` (clean), `cargo test -p flux-codegate` (4 passed).

## Notes
- The private `gather_effects`/`host_effect_to_flow` in `analyze.rs` show the existing mapping; this
  helper is the per-node (attributed) sibling of that union.
