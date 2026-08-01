---
id: C-447
title: "One `FlowEngine` serializes every turn — can the mutex go without weakening turn identity?"
pillar: Core
status: ready
priority: 8
design: docs/designs/pi-comparison-remediation.md
epic: pi-comparison-remediation
areas: [flux-flow]
note: "F3 of the Pi comparison: all public turn entries acquire the same `turn_gate` (flux-flow/src/engine.rs:713). ⚠ It is a STRENGTH and a ceiling at once — the review calls it `a strong identity/session-integrity simplification and a real throughput ceiling`. Answer the question before optimising"
---

# The mutex that buys identity and costs throughput

## Goal

Answer the review's open question: *can the per-engine turn mutex be removed without weakening
immutable turn identity or session validity, and what real throughput target requires it?*

## Why this is an investigation, not an optimisation

> *"All public turn entries acquire the same `turn_gate` mutex. This is a strong identity/session-
> integrity simplification and a real throughput ceiling for a server sharing one engine. Scale-out
> requires multiple engines/replicas rather than treating one engine as a high-concurrency scheduler."*

⚠ The mutex is load-bearing. C-408 and C-415's per-principal identity work rests on turns being
serialized within an engine; removing it casually would put that at risk in a way tests may not catch.
**And no throughput target has been stated** — the review notes concurrent-session throughput was
*"undetermined"* for both harnesses.

## Acceptance

- [ ] ⚠ **State the throughput target first**, or conclude there isn't one. Optimising a ceiling nobody
      has hit is how a safety simplification gets traded for nothing.
- [ ] Enumerate exactly what the mutex guarantees today — turn identity, session validity, ordering —
      each with the code that depends on it, not a summary.
- [ ] A measurement: what throughput one engine actually sustains, so "ceiling" is a number.
- [ ] The answer recorded either way. ⚠ *"Multiple engines/replicas is the answer and the mutex stays"*
      is a perfectly good outcome — the review already names it as the scale-out path.
- [ ] If it is removable, the failing-first test is about **identity under concurrency**, not throughput:
      two concurrent turns must not be able to observe or inherit each other's principal.
- [ ] Full gate green.

## Notes

- ⚠ Read C-408 and C-415 first. Both establish that a turn's identity is installed in a task-local scope;
  concurrency within one engine is exactly the condition their tests do not exercise.
- Interacts with [C-436](C-436-flux-tui-remote.md) and the server: a remote or multi-tenant deployment is
  where the ceiling would first be felt.

## Progress
- Filed 2026-08-02 from the Pi comparison.
