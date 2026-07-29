---
id: C-212
title: "Cross-harness session history — search what was already said, in any local harness (epic)"
pillar: Core
status: ready
priority: 9
epic: harness-history
design: docs/designs/harness-history.md
note: "flux usage already parses codex/claude-code/opencode state and discards the message text one field short; the new work is not acquisition but containment — this is the first datasource whose input is out-of-jail, secret-bearing and injection-shaped"
---

# Cross-harness session history — search what was already said, in any local harness (epic)

## Goal
Let an agent answer "what did we decide about this last month?" against the conversation history of
**every local coding harness**, not just flux — `search(query: "…", harness: "opencode")` over
`flux | codex | claude-code | opencode`, returning addressable message content rather than token
counts.

The acquisition half already exists and is proven in production: `flux usage`
(`crates/flux-cli/src/usage.rs`) locates, opens and parses all four harnesses today, and its parsers
walk **exactly the records that carry the message text** before reading only `usage` and `model` out
of them (`:963-969`, `:1058-1125`, `:1214-1220`). The content is in hand at every site and dropped
one field short.

So the epic's weight is not where it looks. It is in the fact that harness history is a **category of
input flux has never ingested**: it lives outside the workspace jail, it is where credentials go to
be pasted, and it is verbatim adversarial text that an attacker can pre-load once and retrieve
forever. Every existing datasource ingests something the operator deliberately pointed at. This one
would ingest every project they have ever worked on.

## Acceptance
- [ ] C-213 (extract the adapters into `flux-capabilities`), C-214 (message-shaped extraction),
      C-215 (the datasource + `harness` selector) and C-216 (redaction corpus + opt-out proof) are
      done, each with the failing-first test its story names.
- [ ] `search(query: …, harness: "opencode")` returns a real message from a real opencode database,
      addressable back to its harness, session, workspace and timestamp.
- [ ] **The containment properties hold and are tested, not asserted**: the datasource is off unless
      explicitly enabled; every ingested body is escaped as A-21 escapes `<knowledge-base>` bodies;
      every body passes the shared redactor **at ingest**, not at render; and the op declares a
      per-harness permission subject so a policy can allow `flux` and deny the rest.
- [ ] A test proves a **disabled** datasource performs zero reads outside the workspace. "Off by
      default" that is only true of the happy path is not off by default.
- [ ] `flux usage` is behaviourally unchanged throughout — it moves onto the shared discovery layer
      and keeps its own token-shaped projection.

## Progress
- 2026-07-29 — epic opened from a direct request. Design:
  [harness-history.md](../designs/harness-history.md). Every claim about the existing adapters was
  verified against the tree before filing: the discovery roots, the four state shapes, the scan
  budget constants, and the three parse sites that already hold the message text.
- Ordering is **strict** (C-213 → C-214 → C-215 → C-216), not a preference — each story consumes the
  previous one's surface. This epic does not fan out.

## Notes
- **Why the safety envelope sits in C-215 and not in a later hardening story.** The story that
  exposes the data is the story that must contain it. Splitting "ship the datasource" from "make it
  safe" would create a window in which the unsafe version is the shipped version, and this epic's
  whole risk is concentrated in that window. C-216 hardens what C-215 must already establish; it does
  not introduce it.
- **No new crate.** `flux-capabilities` (L5) already owns the datasource and `rusqlite`, and sits
  above `flux-events` (L2) for the flux-native adapter. The repo's standing preference is modules over
  new crates and nothing here argues for an exception.
- **Read-only, always.** No adapter may ever write to another harness's state. The opencode adapter
  already opens with `SQLITE_OPEN_READ_ONLY`; that is the rule, not an implementation detail.
- Non-goals, deliberately: no embeddings in this epic, no live tailing, no cross-machine sync. See the
  design doc.
- ⚠ The scan budget (`MAX_JSONL_FILES = 20_000`, `MAX_JSONL_FILE_BYTES = 200 MiB`) is inherited from
  `flux usage`, but message-level extraction multiplies output per file by one to three orders of
  magnitude. Treat the budget as a correctness property, not a performance tuning knob.
