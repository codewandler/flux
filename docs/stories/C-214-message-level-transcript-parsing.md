---
id: C-214
title: "Message-shaped extraction — keep the transcript text the usage parsers walk past"
pillar: Core
status: ready
priority: 11
epic: harness-history
design: docs/designs/harness-history.md
note: "all three external parsers already descend to the object holding the message body and read only usage/model out of it; the extraction is one field wide, the scan budget is the hard part"
---

# Message-shaped extraction — keep the transcript text the usage parsers walk past

## Goal
Give the C-213 scan layer a message-shaped output: a `HarnessMessage` carrying role, text, timestamp,
model, session id and workspace, for all four harnesses. Today every parser reaches the object that
holds the body and takes only the token counts out of it — claude at `usage.rs:963-969`
(`v["message"]` → `usage`, `model`), codex at `:1058-1125` (filters on `"user_message"` /
`"agent_message"`, descends to `/payload/message`, reads `usage`), opencode at `:1214-1220` (selects
`data` from the `message` table). The extraction itself is one field wide.

The engineering weight is not the parse. It is that a message record is per-message and carries full
text, where a usage record is per-turn and carries eight integers — the same scan now produces one to
three orders of magnitude more output, against user directories that hold years of history.

## Acceptance
- [ ] A `HarnessMessage { harness, session_id, index, role, text, model, workspace, ts_ms, path }`
      produced by all four adapters, with `role` normalized across the harnesses' differing
      vocabularies rather than passed through raw.
- [ ] **Failing-first, per harness**: a fixture transcript in each of the three external shapes
      (codex JSONL, claude-code JSONL, opencode SQLite) whose message text is asserted to come back
      intact — including a multi-part / structured-content message, which is where a naive
      `as_str()` silently yields empty text rather than failing.
- [ ] **The scan budget is enforced against bodies, not just files, and is proven by test**: a
      total extracted-bytes ceiling, a skip count that is reported rather than swallowed, and a test
      that a pathological input (one enormous message; a file above `MAX_JSONL_FILE_BYTES`) degrades
      by skipping and counting instead of exhausting memory. The inherited file/count caps are
      necessary and **not** sufficient here.
- [ ] Malformed input never aborts a scan: one bad line, one unreadable file, one unexpected schema
      is skipped and counted, matching the behaviour `usage.rs` already establishes. Pinned by a test
      per harness with a deliberately corrupt record.
- [ ] Ordering within a session is stable and reproducible across re-scans — `index` must address
      the same message on a second run, since C-215 builds record ids on it.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — filed with the epic. Depends on C-213; do not start before it lands.

## Notes
- **Structured content is the trap.** None of these harnesses stores a message as a plain string in
  every case: content is frequently an array of typed blocks (text / tool_use / tool_result / image).
  Decide explicitly what a `HarnessMessage.text` contains for a tool-call-only message, and write the
  decision down — "" and "the tool name" are both defensible; silently dropping is not.
- **This story extracts; it does not sanitize.** Redaction and escaping belong to C-215/C-216, at the
  ingest seam, so there is exactly one place they can be forgotten. Do not scatter them here.
- Do not widen `HarnessMessage` to carry cost/pricing — `flux usage` keeps its own projection
  (C-213). Two consumers, two projections, one scan.
- Read-only always; the opencode adapter keeps `SQLITE_OPEN_READ_ONLY`.
