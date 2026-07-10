---
id: D-127
title: "slack mrkdwn→Markdown renderer mangles and panics on multi-byte chars"
pillar: Core
status: done
note: "byte-wise fallthrough pushes `bytes[i] as char` (mojibake for every non-ASCII char) and leaves `i` mid-sequence → the next `text[i..]` slice panics; message.list defaults to markdown → crashes on any channel with an em-dash"
---

# slack mrkdwn→Markdown renderer mangles and panics on multi-byte chars

## Goal
`mrkdwn_to_markdown` (`plugins/slack/src/main.rs`) must handle non-ASCII text: today its
fallthrough copies one **byte** per iteration (`out.push(bytes[i] as char); i += 1`), which turns
every multi-byte UTF-8 char into mojibake and leaves `i` inside the sequence, so the next
`text[i..]` slice panics (`byte index … is not a char boundary; it is inside '—'`). Because
`slack.message.list`/`slack.thread`/`slack.mentions` default to Markdown conversion, reading any
channel whose history contains an em-dash, umlaut, or emoji kills the plugin process
("plugin closed the connection").

## Acceptance
- [ ] Failing-first test: `mrkdwn_to_markdown("flux 0.14.2 — release")` returns the input intact
      (today: panic); umlauts/emoji round-trip unmangled; conversion of links/bold still works in
      the same string.
- [ ] The fallthrough advances char-wise (`chars().next()` + `len_utf8()`); all other branches
      already advance by whole tokens found at char boundaries.
- [ ] Live proof: `slack.message.list` (default text_format) succeeds on a channel whose history
      contains em-dashes.

## Progress
- 2026-07-10 found live: `slack.message.list` on `#ai-agent-platform` → panic at main.rs:923
  reading history containing `—`. Workaround confirmed: `text_format=mrkdwn` skips the converter.
- 2026-07-10 **DONE.** Failing-first test `mrkdwn_to_markdown_preserves_multibyte_chars`
  reproduced the exact live panic (byte 13 inside `'—'`); fallthrough now advances char-wise
  (`chars().next()` + `len_utf8()`), fixing both the panic and the mojibake. All 66 slack plugin
  tests green. Live proof: default-markdown `message.list` on the same channel parses cleanly,
  em-dashes intact. Ships with the next plugins pack cut (pack slack v0.1.0 carries the bug;
  workaround until then: `text_format=mrkdwn`).

## Notes
- Repro input: any text with a multi-byte char at byte offset n where the next loop iteration
  starts (e.g. `"flux 0.14.2 — release"`, panic at index 13 inside `'—'`).
- Ships with the next plugins pack cut (the pack's slack v0.1.0 binary carries the bug); the
  workaround until then is `text_format: "mrkdwn"` on the read ops.
