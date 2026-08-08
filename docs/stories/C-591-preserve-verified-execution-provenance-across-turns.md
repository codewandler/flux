---
id: C-591
title: "Preserve verified execution provenance across turns"
pillar: Core
status: ready
priority: 2
epic: session-truth
design: docs/designs/session-truth-and-self-inspection.md
areas: [flux-agent, flux-flow, flux-runtime]
depends_on: [C-590]
note: "a later turn must not infer 'nothing ran' from conversational context that omitted task-child events"
---

# Preserve verified execution provenance across turns

## Goal

Carry a small host-derived record of what the previous turn actually executed, and make durable
inspection the required fallback before an agent contradicts verified execution history.

## Acceptance

- [ ] Failing first, a recorded `task` child successfully performs and verifies a mutation; the next
      turn sees only user/assistant prose and can falsely claim the action and child were fabricated.
- [ ] Turn settlement records `TurnExecutionReceipt/v1` with session/turn, accepted batches,
      operation/status/effect class, child ids/roles, verified result state, usage and explicit
      omission metadata. It contains no reasoning or raw operation bodies.
- [ ] The latest receipt enters the next turn as host facts and is reconstructed identically after
      compaction, `-c`, `/resume`, crash resurrection and backend restart from canonical events.
- [ ] Agent instructions distinguish the direct surfaced schema, host operation registry and a
      delegated child's narrowed/different capabilities. The absence of direct `bash` cannot prove a
      `task` child did not invoke `proc.run`.
- [ ] When challenged about a prior action, the loop consults the receipt and may call
      `session.inspect`; it distinguishes “not visible in chat context” from “did not happen” and
      cannot label host-verified success fabricated without contrary durable evidence.
- [ ] The hermetic `s_2013` regression covers a failed unknown role followed by valid children, a
      real uninstall, verification, a later capability question and a transcript question. Every
      answer preserves the true causal account and links the child/action evidence.
- [ ] Receipt sizing, secret redaction, corrupted/missing child streams, duplicate events and legacy
      sessions fail safely; usage is not double-counted and full gates/public delegation docs pass.

## Progress

- 2026-08-05 — contracted from `s_2013`, whose durable stream proves the later conversational
  corrections—not the original execution report—were false.
