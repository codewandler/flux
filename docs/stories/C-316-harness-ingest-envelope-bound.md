---
id: C-316
title: "Harness ingest holds one envelope per session, and a schema without session ids makes that one per message"
pillar: Core
status: ready
priority: 8
epic: harness-history
design: docs/designs/harness-history.md
areas: [flux-capabilities]
note: "C-215's memory class again, found by C-216's corpus rather than by an OOM — the retention bound is a property of the harness schema, not of the code, and opencode has a fallback path where it degrades to 1:1"
---

# Harness ingest's envelope retention is bounded by schema, not by code

## Goal

`ingest_harness_history` holds one `SessionEnvelope` per session for the duration of a scan. The
reasoning is sound on its face: sessions are three to five orders of magnitude rarer than messages,
so the retained set is negligible.

But that ratio is a property of the **harness schema**, not of the code enforcing anything. C-216
found the branch where it degrades: an opencode database with no `session_id` column and no
`sessionID` in `message.data` falls back to the message's own id, at which point envelopes scale
**1:1 with messages** and the "negligible" set is the whole transcript.

This is the same class as C-215's shipped defect — an ingest that asserted a memory bound its code
did not have, which meant an OOM on real data. It was caught there by review rather than by a crash,
and here by a corpus rather than by a crash. The pattern is worth ending rather than re-finding.

C-216 deliberately did **not** assert an OOM. Its test
(`session_envelope_retention_is_bounded_by_sessions_only_when_the_schema_has_them`) states the ratio,
so the claim is visible and false-able without pretending a test can observe exhaustion.

## Acceptance

- [ ] **The bound lives in the code, not in an assumption about the schema.** A cap inside ingest, so
      that a degenerate or hostile transcript cannot make retention scale with message count whatever
      the schema does. State what happens at the cap — dropped, flushed, or refused — and why that is
      the right answer for a search index.
- [ ] **Failing-first**: a test that drives the fallback path (no `session_id` column, no `sessionID`
      in `message.data`) and observes retention *not* scaling with message count. It must red before
      the bound exists.
- [ ] C-216's ratio test is updated to assert the new bound rather than the schema property, in the
      same commit.
- [ ] **`meta`'s string values are redacted but not escaped**, where `body`/`title`/`id` are both.
      Latent today — `records_to_context_blocks` writes only `source`/`entity` as tag attributes, and
      `render_match`/`render_record` print id/title/body, so no model-visible surface renders record
      `meta`. Either apply `contain` to `meta` too, or record at the definition why it is exempt so a
      future renderer does not silently make it live.
- [ ] Full gate green in both workspaces.

## Notes

- Found by [C-216](C-216-harness-transcript-redaction-corpus.md)'s corpus.
  [C-215](C-215-harness-history-datasource.md) is the prior instance of the same class.
- **Adjacent, and deliberately not folded in here** — C-216 also measured that adapter *coverage* is
  asymmetric: only claude-code surfaces tool output (codex files it as a `function_call_output`
  response item with no `role`, so the prefilter never parses it; opencode files it as a `tool` part
  whose output sits under `state`), and **no** adapter surfaces a tool call's *input*, so a credential
  passed as a tool argument is never indexed. Containment is unaffected — dropping is containment —
  but a reader would reasonably assume the three behave alike. Pinned by
  `no_adapter_but_claude_code_surfaces_tool_output`. Whether to close that asymmetry belongs with
  [C-302](C-302-flux-native-message-adapter.md) or its own story, not with a memory bound.
- Housekeeping owed by whoever closes this epic: `docs/designs/harness-history.md` line 3 still says
  "Status: designed, none started", stale since C-213.
