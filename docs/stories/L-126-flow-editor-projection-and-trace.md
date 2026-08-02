---
id: L-126
title: Project authored flows into an editor graph and trace execution back to its nodes
pillar: Language
status: done
epic: flow-editor-contract
design: docs/designs/flow-editor-contract.md
note: "upstream contract for flux-exchange's two-way visual/source editor; projection remains pure L0 and execution keeps using the existing envelope"
---

# Project authored flows into an editor graph and trace execution back to its nodes

## Goal
Give hosts a typed, versioned editor projection of authored Flux and a stable node identity map that
an execution trace can refer back to, without adding an execution model or embedding UI concerns in
the language.

## Acceptance
- [x] Failing-first tests cover source → editor projection → AST/source round trips for the visual
      subset: call/bind, condition, bounded loops, parallel branches and return.
- [x] Valid Flux outside that subset is preserved and reported as source-only rather than repaired,
      dropped or mis-projected.
- [x] Public editor diagnostics carry source ranges and stable node ids where a node exists.
- [x] Runtime events identify the exact authored node, including duplicate calls and loop/parallel
      occurrences, without persisting raw values.
- [x] `flux-lang` remains L0, the ordinary execution signatures stay compatible, and the full gate
      is green.

## Progress
- 2026-08-02: story opened from the accepted flux-exchange flow-editor plan; implementation started.
- 2026-08-02: projection/lowering and opt-in editor-addressed runtime tracing implemented; focused
  tests pass.
- 2026-08-02: full workspace build, test and clippy gates plus formatting and `flux-codegate` pass;
  story complete.
- 2026-08-02: rebased audit against published 0.52/main found and fixed identity drift for in-place
  source edits and stale runtime paths after graph reorder; failing-first regressions cover both plus
  deletion-induced path shifts.

## Notes
- `crates/flux-lang/docs/STATUS.md` currently marks graph projection, inspectors and trace-to-node
  mapping unbuilt.
- The first consumer is `../flux-exchange`; the contract must remain host-neutral.
