---
id: C-214
title: "Message-shaped extraction — keep the transcript text the usage parsers walk past"
pillar: Core
status: done
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
      *Three of four: codex, claude-code and opencode. The flux-native adapter is **split out as
      [C-302](C-302-flux-native-message-adapter.md)**, which runs solo because it needs a
      `flux-events` dependency in `flux-capabilities` (legal — L5 → L2 — but a manifest + lockfile
      change). This story is closed on the three EXTERNAL harnesses, which is what its Goal is
      actually about: the text the usage parsers walk past.*
- [x] **Failing-first, per harness**: a fixture transcript in each of the three external shapes
      (codex JSONL, claude-code JSONL, opencode SQLite) whose message text is asserted to come back
      intact — including a multi-part / structured-content message, which is where a naive
      `as_str()` silently yields empty text rather than failing.
- [x] **The scan budget is enforced against bodies, not just files, and is proven by test**: a
      total extracted-bytes ceiling, a skip count that is reported rather than swallowed, and a test
      that a pathological input (one enormous message; a file above `MAX_JSONL_FILE_BYTES`) degrades
      by skipping and counting instead of exhausting memory. The inherited file/count caps are
      necessary and **not** sufficient here.
- [x] Malformed input never aborts a scan: one bad line, one unreadable file, one unexpected schema
      is skipped and counted, matching the behaviour `usage.rs` already establishes. Pinned by a test
      per harness with a deliberately corrupt record.
- [x] Ordering within a session is stable and reproducible across re-scans — `index` must address
      the same message on a second run, since C-215 builds record ids on it.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — filed with the epic. Depends on C-213; do not start before it lands.
- 2026-07-30 — **three of four adapters landed.** `crates/flux-capabilities/src/harness/`
  gains `message.rs` (the model, the role normalization, the content flattener, the body budget),
  `claude.rs`, `codex.rs` and `opencode.rs`. Extraction **streams** — adapters push into a
  `MessageSink` and never build a `Vec`, because a full scan of one real `~/.claude/projects` is
  474 MiB of message text.
- **Not delivered: the flux adapter.** `flux.rs` needs `flux-events`' `EventStore`, and
  `flux-capabilities` does not depend on it — adding it edits `crates/flux-capabilities/Cargo.toml`
  and `Cargo.lock`, both fenced for this wave. Reading `~/.flux/events.db` by hand instead was
  rejected: without `flux-events` in dev-dependencies the only available fixture is one this
  adapter writes itself, which tests the guard against its own assumptions rather than against the
  store. One manifest line unblocks it; the rest of the adapter is ~60 lines against
  `EventKind::Message`.
- **Budget defaults were calibrated against real history, not guessed.** The first pass shipped a
  64 MiB total ceiling; run against this machine's actual harness state it truncated claude-code
  after 54 of 349 sessions. Real full-scan figures now recorded in `scan.rs`: claude-code 474 MiB /
  538 420 messages / 2 453 files, codex 39 MiB / 40 614 messages, opencode 36 MiB / 80 643 messages.
  Ceilings are now 2 GiB and 5 000 000 messages.
- 2026-07-31 — **review fixes; the body ceiling is now real on the opencode path.** Four defects an
  independent review of the branch tip found, each now pinned by a test that failed on that tip:
  - **The per-message ceiling did not bind opencode at all.** The accumulator charged only a part's
    `text` key, so opencode's own dominant shape — a `tool` part carrying its output under `state` —
    contributed *zero*. The query caps one part; nothing capped how many arrived, so the assembled
    `Vec<Value>` grew to `max_message_bytes²/17` (≈64 GB at the 1 MiB default) while the emitted
    body, being `[tool_use: …]` markers, stayed small and the stats reported it small. The ceiling is
    now charged the retained part JSON, whole — which bounds the part *count* as a side effect — and
    `an_opencode_message_is_bounded_by_the_body_ceiling_however_its_parts_carry_their_payload` drives
    a budget through `opencode_messages` for the first time (before: `emitted: 2, body_bytes: 156`
    for 3.6 KB of parts against a 512-byte ceiling; after: `skipped_oversize: 1`).
  - **A file over `max_file_bytes` was filed as `skipped_unreadable`**, contradicting the documented
    meaning of `skipped_oversize`, on both JSONL adapters. The old test asserted the *sum* of the two
    buckets and so could not see it; it now asserts each bucket, for both adapters.
  - **An opencode `rows.next()` failure counted one `skipped_malformed` and broke**, reporting a
    truncated scan as a complete one. It now propagates, matching `usage.rs:1214`'s
    `while let Some(row) = query.next()?` — a failed step is the database failing, not a record being
    unreadable, and there is no next row to skip to.
  - **`flatten_content` did not honour its own documented bound** — a 200 KB single-string body was
    materialized whole and only then dropped. The append now clamps to just past the cap on a char
    boundary; the one byte of overshoot is deliberate, since it is what `MessageSink::offer`
    recognizes as over-cap, and clamping to exactly the cap would emit a silently truncated body in
    place of a reported skip. The doc now also states the honest per-message *memory* bound: for the
    JSONL adapters that is `max_line_bytes` (8 MiB), because a record is parsed into a `Value` before
    it is flattened — not `max_message_bytes`.
- **Two findings only real data could produce**, both fixed: a fifth of claude-code assistant
  records are redacted thinking blocks (`{"type":"thinking","thinking":""}`) and were yielding
  *empty* bodies — the exact silent-drop this story names — now `[thinking]`; and codex's
  pre-filter had to narrow to `"role"`, because parsing all 1.7 GB of captured tool output was the
  scan's dominant cost.

## Decisions
- **`text` for a tool call is `[tool_use: <name>]`**, never `""`. A tool-call-only message is a real
  message and must stay addressable; the tool name is the part of it worth finding. Unknown block
  types leave `[<type>]`, so nothing is dropped without a trace.
- **codex reads `response_item` payloads, not `event_msg`.** codex mirrors every turn into both;
  reading both double-counts. `response_item` carries the normalized role and the structured
  content, at the cost of also surfacing the instruction preamble as a user turn.
- **opencode bodies come from the `part` table**, not `message.data` — verified against a real
  80 676-message database, where the message row carries only role/model/tokens. Structural parts
  (`step-start`, `step-finish`, `snapshot`) are named and excluded; everything else keeps a marker.
- **`index` counts transcript position, not output position.** A skipped-over-budget message still
  consumes its ordinal, so a skip cannot renumber what came after it.
- **An over-budget message is skipped whole and counted; it is never truncated.** Retaining the parts
  that fit and dropping the rest would emit a body that reads complete and is not — the silent drop
  this story exists to avoid. This extends to the *sum* of an opencode message's parts the rule the
  adapter already applied to a single over-cap part.
- **A failed SQL row step is an error, not a skip.** Every *record*-level problem degrades (bad JSON,
  unknown role, unexpected schema, over-budget body). A step that fails has no next row to skip to,
  so swallowing it would hand back a truncated scan wearing a complete scan's stats.

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
