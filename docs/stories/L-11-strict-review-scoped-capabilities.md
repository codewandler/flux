---
id: L-11
title: Strict review — scoped capabilities (with_tools) enforced at dispatch (Phase 2)
pillar: Language
status: backlog
epic: strict-review-flows
design: docs/designs/strict-review-flows.md
note: analyzer-visible capability-scope node + runtime narrowing threaded into Executor::dispatch
---

# Strict review — scoped capabilities (with_tools) enforced at dispatch (Phase 2)

## Goal

Turn per-block tool restriction from advisory into a **runtime-enforced** guarantee: add an
analyzer-visible capability-scope node (`with_tools` lowering to a `cap_scope` block, or metadata on
`seq`/`parallel`/`each`) and thread the narrowed tool/effect set through `flux-flow` into
`Executor::dispatch`, so a call to a tool outside the active scope fails closed — even when the outer
session policy would allow it. This is the feature that makes strict review not-just-a-skill; it
serves the Language pillar by making capability narrowing a first-class, checkable language construct.

Full design: [docs/designs/strict-review-flows.md](../designs/strict-review-flows.md) — Phase 2 &
"Capability scoping".

## Acceptance

- [ ] **Failing-first test:** a flow with `with_tools ["read_many"]` can call `read_many` and a
  `grep` call inside that block is **denied** with a normal policy/capability error — added red,
  then green.
- [ ] Capabilities narrow (never widen) as execution descends: session ∩ AgentSpec ∩ flow ∩ block ∩
  sub-agent. A sub-agent invoked with `tools: []` cannot perform filesystem/shell/network IO beyond
  the provider call its role requires.
- [ ] Enforcement is in the runtime dispatch path (`Executor::dispatch`), not prompt text.
- [ ] Capability scope **entry/exit and every denial** appear in the evidence log.
- [ ] The analyzer sees the scope node so an undeclared tool inside it can be flagged statically where
  possible.
- [ ] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.
- [ ] CHANGELOG entry.

## Notes
- Open question from the design to settle here: scopes as allowed **tools**, allowed **effects**, or
  both; and whether sub-agent restriction is a typed `task(tools:)` param or a surrounding block scope.
- Builds on [L-10](L-10-strict-review-example-flow.md) (proves the contract this must enforce).
