---
id: C-707
title: "The agent can propose a host binding it may not grant itself"
pillar: "Core"
status: backlog
priority: 3
epic: first-class-hosts
areas: [flux-capabilities, flux-cli]
design: docs/designs/the-substrate-seam.md
note: "C-649 shipped only the read half of host.*; add/rm are CLI-only, so an operator cannot say `configure access to my cluster` and have it done"
---

# The agent can propose a host binding it may not grant itself

## Goal

The `host.*` operation group is read-only: `host.list`, `host.info`, `host.probe` and
`host.metrics`. Creating or removing a binding exists only as `flux host add` / `flux host rm`, so
an operator cannot say "configure access to my dev cluster" and have the agent do the legwork —
discover the endpoint, resolve the credential *location*, write the binding, probe it — even
though every read it would need is already an operation it holds.

The reason the write half was withheld is real and must survive this story. A binding names where
effects land. An agent that could create a binding **and** grant itself selection could route its
own effects anywhere, which is exactly the escalation Decision 0018 rule 4's deny-by-default grant
exists to prevent. The resolution is to split the act: **the agent may propose, the operator
grants.** A binding the agent writes is inert — it exists, it lists, it probes, and it cannot be
selected until a human grants it, which is the same act that already gates every binding today.

The natural home is a self-configuration group distinct from `host.*`'s reads, because these
operations mutate the operator's configuration rather than reporting on a substrate.

## Acceptance

- [ ] A self-configuration operation group carries the mutating verbs (create, remove, and amend a
      binding's non-grant fields). It is high-risk, ambient-gated like `host.*`, and every verb
      passes the approval gate — a `--yes` run still records what was written.
- [ ] A binding the agent creates carries **no grant** and cannot be given one by any operation: the
      grant is settable only by the operator surface, and a test proves an agent-created binding is
      unselectable until a human grants it.
- [ ] No verb accepts a credential *value*; only a reference in the existing vocabulary, validated
      at write time, with the same refusal `flux host add` already gives for an inline secret.
- [ ] Removal cannot silently strand a running session: removing the binding a live session
      selected is refused with the reason, since selection is immutable for that session.
- [ ] The written configuration is exactly what the CLI path writes — one validated writer, not a
      second one — and a test asserts an agent-written binding and a CLI-written binding are
      byte-identical for the same inputs.
- [ ] The docs state the split plainly: the agent can prepare access, a human decides it may be
      used, and neither half is a rubber stamp for the other.
