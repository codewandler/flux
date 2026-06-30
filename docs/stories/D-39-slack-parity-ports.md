---
id: D-39
title: slack fluxplane parity ports — Block Kit messaging, text_format, ticket extraction, file content_bytes, list filters
pillar: Agent
status: done
priority:
epic:
design:
note: field-by-field re-audit vs ~/projects/fluxplane/fluxplane-plugins/slack/ (audit: .flux/plans/d36-parity-audits/slack.md) surfaced 14 ops with real feature gaps + 1 dead param; all ported from the Go reference
---

# slack fluxplane parity ports

## Goal

Close the fluxplane parity gaps the D-36 re-audit surfaced in `slack`. Audit:
`.flux/plans/d36-parity-audits/slack.md`. The highest-value gap was the missing Block Kit surface
on `message.send`/`message.edit` (the model previously *could not send Block Kit messages* through
flux slack).

## Acceptance
- [x] `slack.message.send` / `slack.message.edit` — Block Kit parity: added `markdown`,
      `blocks` (`Vec<Value>`), `unfurl_links`, `unfurl_media`, `parse`; `text` relaxed to optional
      (fluxplane `Text` is optional because blocks/markdown can carry content). A
      `message_content()` helper mirrors fluxplane: blocks require a text fallback, markdown renders
      as a mrkdwn section, unfurl/parse flow into the API body. Contracts updated.
- [x] `slack.message.list` / `slack.thread` — `text_format` (`markdown`/`mrkdwn`/`both`, default
      `markdown`) with a best-effort `mrkdwn_to_markdown()` renderer; `slack.thread` also gained
      `max_bytes` (per-image download cap, default 10MB — parsed/defaulted; thread doesn't download
      images, so not enforced yet).
- [x] `slack.search` — `tickets` + `ticket_keys` (`Vec<String>`); extracts per-match tickets +
      aggregate `{key, mentions, permalinks}` records.
- [x] `slack.mentions` — `bot` (resolve bot-token identity) + `tickets`/`ticket_keys`; fixed
      `ticket_keys` type `Vec<Value>`→`Vec<String>` (the audit flagged non-string items silently
      dropped).
- [x] `slack.file.upload` — dead param `alt_text` wired into `files.completeUploadExternal`; added
      `content_bytes` (base64 inline alternative to `blob_ref`); `blob_ref` relaxed to optional
      (exactly one content source required).
- [x] `slack.file.download` / `slack.download` — `blob_ref` seed (returned ref starts with the
      supplied seed, matching fluxplane `BlobWrite.Ref`).
- [x] List filters: `query` + `limit` on `file.list`/`channel.list`/`user.list`/`bookmark.list`;
      `emoji.list` also gained `mode` (`custom`/`builtin`/`all`, default `custom`) +
      `include_aliases`. Client-side case-insensitive filtering + truncation.
- [x] Failing-first MockHost tests per change; `cargo build/test/clippy -D warnings/fmt` green for
      `slack` (65 tests, +18). `schema_contract` gained `Kind::ArrayStr` for `Vec<String>` arrays.
- [x] `endpoint_ref` + per-call `role` architectural splits left as-is (do-not-port).

## Notes
- `slack.thread` does not download attached images, so `max_bytes` is parsed/defaulted but not
  enforced; the raw message envelope is returned as before. Follow-up if image downloads land.
- Audit report: `.flux/plans/d36-parity-audits/slack.md`.
- `plugins/slack/Cargo.toml` gained `base64` (for `content_bytes` inline decoding).
