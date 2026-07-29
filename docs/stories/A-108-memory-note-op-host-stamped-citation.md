---
id: A-108
title: "memory_note — the op whose citation the model cannot supply, forge, or omit"
pillar: Agent
status: backlog
epic: evidence-pinned-memory
design: docs/designs/evidence-pinned-memory.md
note: "the epic's load-bearing invariant: the model supplies the claim, the HOST supplies the receipt + git SHA + paths — same property that makes ActionBatch trustworthy (staged.rs:203)"
---

# memory_note — the op whose citation the model cannot supply, forge, or omit

## Goal
Make provenance unforgeable by construction. `memory_note(claim, scope)` takes no receipt, no SHA,
and no paths — the host reads those from the live turn context and the workspace. A model that could
write its own `receipt` field could manufacture provenance for a hallucination, and the citation
would be decoration rather than evidence.

## Acceptance
- [ ] `memory_note(claim: string, scope: "project" | "global") -> memory id`, declared with
      `Effect::Write` so it passes the ordinary envelope (policy, approval, audit) — writing durable
      cross-session state is a real effect and is gated like one.
- [ ] **The forgery test**: the op's input schema has exactly two properties (`claim`, `scope`);
      there is no public path by which a caller supplies `receipt` or `git`. Assert against the
      generated schema, so adding a field later fails the test loudly.
- [ ] The host stamps `receipt` from the live `RuntimeTurnContext` (stream, event id, turn id).
- [ ] The host stamps `git.paths` from the **turn's evidence trail** — the workspace paths the
      citing turn actually read or wrote — not from the model, and not from a whole-workspace scan.
- [ ] Outside a git repo, `git` is `None`; the entry is still written and is never later reported
      stale (it has nothing to be stale against).
- [ ] Compensation (A-91, if landed): `Compensation::Inverse { op: "memory_forget" }`. If A-103 has
      not landed, leave a note rather than a stub.
- [ ] Full gate green.

## Progress
- Not started.

## Notes
- Design: [evidence-pinned-memory.md](../designs/evidence-pinned-memory.md).
- Blocked by A-107.
- The invariant mirrors `ActionBatch`'s ("the host, never the model, constructs this value",
  `flux-flow/src/staged.rs:203`). If a future change makes citation a model-supplied field for any
  reason, this epic's value is gone — say so in the code comment, not just here.
