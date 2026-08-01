---
id: C-316
title: "Harness ingest holds one envelope per session, and a schema without session ids makes that one per message"
pillar: Core
status: in-progress
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

- [x] **The bound lives in the code, not in an assumption about the schema.** A cap inside ingest, so
      that a degenerate or hostile transcript cannot make retention scale with message count whatever
      the schema does. State what happens at the cap — dropped, flushed, or refused — and why that is
      the right answer for a search index.
      → `MAX_LIVE_SESSION_ENVELOPES` (4096) and `SessionEnvelopes` in
      `datasource/harness_history.rs`. At the cap the oldest envelope is **flushed** — projected,
      upserted and let go. *Refused* would let one unusual database deny the whole index when partial
      recall is this datasource's value; *dropped* would leave a session unsearchable and dangle the
      message→session link every one of its messages carries, silently. The cost of flushing is
      stated rather than hidden and is reported (`sessions_evicted`); the const's doc comment records
      it and the two rejected alternatives (read-back-to-resume, LRU).
- [x] **Failing-first**: a test that drives the fallback path (no `session_id` column, no `sessionID`
      in `message.data`) and observes retention *not* scaling with message count. It must red before
      the bound exists.
      → `session_envelope_retention_does_not_scale_with_message_count`. It measures peak live
      retention **from outside**, by replaying the upsert stream, rather than reading a number ingest
      keeps about itself — and it states the property without naming the constant, so it reds at the
      merge-base with `peak envelopes went 5000 -> 10000` rather than with a compile error.
- [x] C-216's ratio test is updated to assert the new bound rather than the schema property, in the
      same commit.
      → `session_envelope_retention_is_bounded_by_ingest_not_by_the_harness_schema` — the schema
      ratio is now the premise, the bound is the conclusion, and it overflows the shipped cap by 900
      sessions.
- [x] **`meta`'s string values are redacted but not escaped**, where `body`/`title`/`id` are both.
      Latent today — `records_to_context_blocks` writes only `source`/`entity` as tag attributes, and
      `render_match`/`render_record` print id/title/body, so no model-visible surface renders record
      `meta`. Either apply `contain` to `meta` too, or record at the definition why it is exempt so a
      future renderer does not silently make it live.
      → **Both branches, split by provenance.** `contain` now applies to every *transcript-derived*
      meta string — `session_id`, `model`, `workspace`, `path` — on the message record and on the
      session envelope. `harness` and `role` are exempt **with the reason recorded at the
      definition**: they are this crate's own closed enum ids, and `meta.harness` is the key the
      selector lowers onto (`record_is_from`), so running it through the redactor would make a
      filter's correctness depend on the operator's secret list. Pinned by
      `the_harness_id_in_meta_is_exempt_from_containment_because_it_is_the_filters_key`. The
      definition also records what containment does and does not buy: the *attribute* surface was
      never the hazard (`flux_core`'s `open_tag` already `attr_escape`s every value, and
      `escape_knowledge_base_body` is not an attribute escaper) — what it buys is the *body* surface.
      The old comment claiming `records_to_context_blocks` renders record `meta` as tag attributes
      was simply wrong and is corrected.
- [x] Full gate green in both workspaces. → build, `test --workspace`, `clippy -D warnings`, `fmt`
      and `cargo test -p flux-codegate` all green; `plugins/` is untouched and its `fmt --check` is
      clean.

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
  "Status: designed, none started", stale since C-213. **Done** — the header now reads
  `C-213 → C-216 landed; C-316 … implemented`.

## Progress

**Landed.** Two commits on `impl/C-316`: `61057114` (a crash-recovery snapshot of the first
implementor's tree, committed unreviewed and ungated so the work survived) and the finishing commit
that reviewed, corrected and gated it.

What the recovered work already had right and was kept: `MAX_LIVE_SESSION_ENVELOPES`, the
`SessionEnvelopes` FIFO container with flush-on-evict, the shared `Upserts` buffer that gives message
records and evicted envelopes one bound instead of two, the two report observables
(`peak_session_envelopes`, `sessions_evicted`), and all four tests.

What was **wrong in it and is fixed here**: it ran `contain` over *every* `meta` string including
`harness` and `role`. `meta.harness` is the harness selector's key — `record_is_from` compares it to
`HarnessKind::id` — so with a redactor holding a value that occurs inside a harness id, every record
of that harness gets `meta.harness = "[redacted]"` and `search(harness: …)` answers "no matches" over
an index that holds the rows. Under-return, never leakage, so nothing else would have caught it: every
other test in the file builds a bare `Redactor::new()`, for which `contain` on an enum id is a no-op.
The wip's own doc comment was self-contradictory about this ("every string value goes through
`contain`" beside "`harness` and `role` … pass through untouched"), which is the tell that it was
mid-edit when the session died. Now: transcript-derived strings are contained, the two enum ids are
exempt with the reason recorded at the definition and in the design, and
`the_harness_id_in_meta_is_exempt_from_containment_because_it_is_the_filters_key` reds without it.

Also removed: a stray `docs/stories/C-316-*.md.tmp.11507.f5054f2a8046` left by the interrupted atomic
write of this file.

**Review round two** — three MINOR doc-accuracy findings, all closed rather than filed, since a
comment claiming what the code does not do is this story's own thesis:

1. `HarnessIngestReport::sessions()` documented itself as "distinct sessions seen, **not** the number
   of envelope records upserted" — the exact inverse of what it counts. It is one per insertion into
   the live set, so a session re-created after eviction counts twice. Comment rewritten to say so; the
   internal field renamed `distinct` → `projected`, which is what it is.
2. `MAX_LIVE_SESSION_ENVELOPES` documented only half the cost of eviction. The re-created envelope
   also re-seeds `first_ts_ms`/`last_ts_ms`, so the record's time range — and the start timestamp its
   title carries — narrows to the post-eviction part. Stated in the const, in `sessions_evicted`, in
   the design, and now *pinned*: `a_session_that_returns_after_eviction_is_projected_again_and_undercounts`
   asserts the narrowed `ts_ms` and title alongside the undercount.
3. The escaping half of the `meta` change was unobserved. `model`/`workspace`/`path` were already
   redacted at the base, so only a value carrying a real `<knowledge-base>` sequence distinguishes
   `contain` from `redact` — and no other fixture in the suite has one. Pinned by
   `every_transcript_derived_meta_string_is_escaped_and_not_merely_redacted`, whose opencode fixture
   puts a breakout in the session directory, the model id, and the database path (via a directory
   named `<knowledge-base>proj`). It reds at the merge base on all six values.

**Left for whoever picks it up next** (both stated by tests rather than assumed away, neither in this
story's Goal): the adapters' own `session id -> ordinal` maps are a retention this cap does not
cover — `the_adapter_ordinal_map_is_a_retention_this_cap_does_not_cover` — and bounding them trades a
memory bound for silently colliding record ids, so it needs C-214's blast radius; and record ids are
model-visible and not length-bounded, which is the byte half of the same question.
