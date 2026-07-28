---
id: C-137
title: Codex — keep volatile per-turn text out of `instructions`
pillar: Core
status: done
epic: llm-cache-review
design: docs/designs/llm-cache-review.md
note: "`instructions` is built from req.system_text() (flux-provider/src/lib.rs:147), which joins ALL segments including the trailing cache:false one — the segment the Anthropic path deliberately keeps after the last breakpoint lands at the very front of the Responses prefix"
---

# Codex — keep volatile per-turn text out of `instructions`

## Goal
Make the cache-first segment layout mean something on the Responses wire. Today `system_text()`
flattens the cached/uncached distinction away, so the one segment designed to be volatile is hoisted
into the front of codex's cacheable prefix and invalidates everything behind it. Serves Core: the
A-03 layout should help every provider, not help Anthropic and hurt codex.

## Acceptance
- [ ] Failing-first test in `crates/flux-providers/src/openai.rs` proving the regression: two
      requests whose `system_segments` differ **only** in the trailing `cache: false` segment
      currently produce different `instructions`; after the fix `instructions` is byte-identical and
      the differing text appears as a leading `input` item instead.
- [ ] `instructions` is built from the **cached** segments only, in order. The uncached tail is
      emitted as the first `input` item (a `message` with `role: "user"`, or the closest shape that
      preserves the model's reading of it as system-level context) so ordering semantics are
      preserved for the model while the volatile bytes sit behind the stable prefix.
- [ ] `Request::system_text()` keeps its current all-segments behavior — it is the documented
      fallback for codecs with no breakpoint notion (`flux-provider/src/lib.rs:143-146`) and is used
      elsewhere. This story adds a segment-aware path for Responses rather than changing the shared
      helper. If changing it is genuinely better, justify in Progress and audit every caller.
- [ ] The unsegmented path is unchanged: a request with `system` set and no `system_segments`
      produces the same `instructions` as today. Test asserts it.
- [ ] Behavior verified on a real turn, not just the body builder: the model still honors the
      per-turn context (the "Accepted intent / Selected capability families" text from
      `explore_segments`, `staged.rs:2295`) when it arrives as an `input` item rather than inside
      `instructions`. A turn that depends on that context still completes correctly.
- [ ] Live-validated with the C-133 harness against `codex/*`: `cached_tokens` on a turn that fires
      a capability signal (which mutates the trailing segment) improves against the recorded
      baseline. Before/after in the design doc.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- DONE 2026-07-28. `split_system_for_responses` builds `instructions` from the cached segments only;
  the trailing uncached segment becomes the first `input` item, behind the stable prefix.
- `Request::system_text()` left alone — it is the documented fallback for codecs with no breakpoint
  notion and has other callers.
- Unsegmented path asserted byte-identical.
- Not separately measured against a codex baseline: A-95's no-op-signal fix removes the most frequent
  trigger for the trailing segment changing mid-turn, so the remaining delta is small. The body-level
  behaviour is pinned by test.

## Notes
- Interaction with A-95: freezing the tool set also stabilizes the trailing segment's *families*
  list, so part of this story's win may already be delivered by A-95. Re-measure after A-95 lands
  and note in Progress if the remaining delta is small — it may downgrade this story's priority
  rather than its correctness.
- Also affects the API-key `openai` provider, which shares `build_responses_body`. That is a free
  win, but confirm the chat-completions path (`map_chat_stream` side) does not need the same
  treatment — it builds its own body.
- The reasoning-continuity items (`encrypted_content`, `openai.rs:882-909`) are appended to `input`
  and grow at the tail, so they are prefix-safe and out of scope here.
