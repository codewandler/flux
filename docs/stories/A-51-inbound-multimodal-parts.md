---
id: A-51
title: Inbound multimodal parts — accept file/data Parts or refuse cleanly
pillar: Agent
status: backlog
priority:
epic: a2a-conformance
design: docs/designs/a2a-conformance.md
note: "Tier-2: file/data input parts are silently dropped in extract_text; the turn runs on empty input"
---

# Inbound multimodal parts

## Goal
Handle A2A `file` and `data` message parts on inbound requests instead of silently discarding them —
either by surfacing their content to the agent turn, or (until multimodal input is wired end-to-end)
by refusing the request cleanly with the correct error code, so a client is never left thinking a
file it sent was processed.

## Why (evidence)
- `crates/flux-a2a/src/server.rs:195-208` — `extract_text` keeps only `kind == "text"` parts;
  `file`/`data` parts round-trip through `Part.extra` (`types.rs:56-57`) but are never read, so a
  message that is *only* a file/data part runs the turn on empty input with no signal to the client.

## Acceptance
- [ ] Inbound `file`/`data` parts are either (a) surfaced into the turn input in a defined way, or
      (b) rejected with `-32005 ContentTypeNotSupported` (depends on A-50 landing the code) when the
      agent can't consume them — decided in this story, not left silent.
- [ ] `Part` gains first-class `file`/`data` accessors as needed (today they live only in `extra`).
- [ ] Failing-first test: a `message/send` whose only part is a `data`/`file` part produces the chosen
      defined behavior (surfaced or `-32005`), not an empty-input completed Task.
- [ ] Docs: the Message & Parts rows in the support matrix update.

## Progress
- (not started)

## Notes
- Sequenced after A-50 (uses `-32005`). Scope decision (accept vs refuse) belongs in this story's
  design step. Epic: [a2a-conformance](../designs/a2a-conformance.md).
