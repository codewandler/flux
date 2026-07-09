---
id: L-30
title: "Make op-surfacing enforcement recurse through composite bodies"
pillar: Language
status: done
priority:
epic: review-hardening
design: docs/designs/review-hardening.md
note: "hidden_ops_in exempts composite calls and never walks their bodies, and composite registration validates bodies against the FULL registry (advertised=None) — so a turn-registered composite can name a non-advertised op, denting A-04's stated 'the compiler rejects plans calling hidden ops'. Legibility/hygiene only: the op still hits approval+guarded IO and the gather gate is honored transitively"
---

# Make op-surfacing enforcement recurse through composite bodies

## Goal
Make A-04's surfacing enforcement transitive through composites, symmetric with the A-13/L-29 gather gate
(which already is). `hidden_ops_in` walks only the plan's own nodes and skips composite calls
(`crates/flux-flow/src/registry.rs:115-130`), and composite registration validates bodies against the full
`ToolRegistry` — `analyze_composites(composites, tools)` builds `OpRegistry::new(tools)` with
`advertised: None` (`registry.rs:222`) — never the turn's advertised set. So a model can call the
always-advertised, ungrouped `op.register` (`flux-tools/src/reflect.rs:248`, `group: None`) with a
turn-scoped composite whose body names a non-advertised op (e.g. `bash` when the `shell` group is off),
then emit a plan calling that composite; `compile_turn` accepts it. This is a **legibility** gap, not a
security one: the inner op still traverses authorization → approval → guarded IO (so `bash` is still
approval-gated), and the gather gate *is* honored transitively because the composite must declare the
body's effects (`LocalSystem`), which trips `mutating_ops_in`.

## Acceptance
- [x] Failing-first test (mirror `hidden_op_plan_is_rejected_and_repaired`, `compile.rs:2296`): with an
      advertised set excluding `bash`, register a session/turn composite whose body calls `bash` (declaring
      `LocalSystem` so registration passes), emit a plan calling that composite, and assert `compile_turn`
      **rejects** it. Today it is accepted.
- [x] Fix: either validate composite bodies against the turn's advertised set at registration, or have
      `hidden_ops_in` expand composite bodies — matching the gather gate's transitivity.
- [x] Pre-authored flows that legitimately compose hidden-group ops (resolved via `get`) still work; the op
      remains approval-gated regardless.

## Progress
- 2026-07-03 filed — 0.2.11 diff review; grounded 🟡 **low-medium correctness, NOT security** (Opus). The
  raw review ranked this the single gravest finding ("shell runs in a workspace that opted out"); grounding
  showed the envelope holds and the composite exemption is an explicit, commented design choice — so this is
  an A-04 gate-completeness fix, framed as legibility/context-hygiene.
- 2026-07-03 **done**. Fixed by expanding `hidden_ops_in` (`crates/flux-flow/src/registry.rs`) to walk a
  composite call's body transitively instead of exempting it outright — chosen over the
  validate-at-registration alternative because the registration path (`analyze_composites`) has no access
  to the turn's advertised set without threading it through `CompositeRegistrar`/`EngineLoopHost`
  (out of scope). A composite *call* is still never itself reported (composites aren't part of the
  advertised-name set), but a new private `collect_hidden_ops` helper recurses into
  `self.composites`-resolved bodies (with a `visiting` cycle guard) so a hidden op named inside a
  turn/session-scoped composite is caught exactly where a top-level call would be. Failing-first tests
  added in `crates/flux-flow/src/compile.rs`: `hidden_ops_in_expands_composite_bodies_transitively`
  (registry-level: `wrap_bash` composite call surfaces `bash`) and
  `hidden_op_behind_a_composite_is_rejected_and_repaired` (end-to-end `compile_turn`/`plan()`: the
  composite-wrapped plan costs a repair round instead of being accepted outright). Both confirmed failing
  for the right reason before the fix (empty result / `attempts == 1` instead of a repair), then green
  after. Gate: `cargo test -p flux-flow` (183 passed), `cargo clippy -p flux-flow --all-targets -- -D
  warnings` (clean), `rustfmt --check` on the touched files (clean; a pre-existing, unrelated
  `loop_host.rs` fmt diff from a concurrent sibling edit was left untouched, out of scope).

## Notes
- Evidence: `crates/flux-flow/src/registry.rs:114-130` (composite exemption), `:222` (`advertised: None`),
  `:251-268` (effect-declaration forces gather-gate transitivity); `flux-tools/src/reflect.rs:248`
  (`op.register` ungrouped); `flux-runtime/src/lib.rs:496-499` (ungrouped ⇒ advertised).
- Residual of [A-04](A-04-enforce-op-surfacing-in-loop.md) / [L-04](L-04-composite-ops.md). Symmetric with
  [L-29](L-29-gather-effect-gate.md). Design: [review-hardening](../designs/review-hardening.md).
