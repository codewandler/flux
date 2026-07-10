---
id: D-128
title: "slack.file.upload never landed: PUT to the pre-signed URL 302s, alt_text breaks the complete call"
pillar: Core
status: done
note: "found live while attaching the 0.14.7 release demo — the external-upload flow had never been exercised against real Slack (the unit test's MockHost matches by URL substring and ignored the method)"
---

# slack.file.upload never landed: PUT to the pre-signed URL 302s, alt_text breaks the complete call

## Goal
`slack.file.upload` must complete Slack's external-upload flow against the real API. Two contract
violations made it fail live: (1) the pre-signed-URL bytes leg used **PUT**, which
`files.slack.com` answers with a 302 redirect (the contract is POST); (2) `alt_text` was placed
inside the `files.completeUploadExternal` files entry, whose entries accept only `id`/`title` —
Slack answers `invalid_arguments`. Alt text belongs on the **reserve** call
(`files.getUploadURLExternal`) as `alt_txt`.

## Acceptance
- [x] The bytes leg POSTs to the pre-signed URL; the existing upload unit test asserts the method
      via the MockHost calls log (it previously matched only the URL, hiding the wrong verb).
- [x] `alt_text` input rides the reserve call as `alt_txt`; the complete call sends only
      `id`/`title` per file.
- [x] Live proof: a PNG uploaded into a channel thread (`ok: true`, file id returned, visible with
      `initial_comment` + alt text).

## Progress
- 2026-07-10 **DONE** in the same pass as D-127. Found live attaching the v0.14.7 release-demo
  render: first `slack file upload → 302 <html>…` (PUT), then `invalid_arguments` (alt_text in the
  complete call). Fixed both; upload test now asserts `method == "POST"` on the pre-signed leg.
  Live proof: `slack.file.upload` with `content_bytes` + `thread_ts` + `initial_comment` +
  `alt_text` → `ok: true`, file `F0BHC2GQFJL` attached in-thread. Ships with the next plugins pack
  cut (pack slack v0.1.0 carries both bugs).

## Notes
- The MockHost matches canned HTTP responses by URL substring only, so wrong verbs/param placement
  pass unit tests — live smoke against real Slack is what caught this (and D-125/D-127). Worth
  remembering for other conn/http plugins.
- CLI inline uploads: `content_bytes` is one argv argument — Linux caps a single arg at ~128 KiB,
  so raw payloads over ~96 KiB need a blob ref or a smaller file (`pngquant` got the release demo
  from 207 KiB → 47 KiB).
