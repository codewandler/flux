---
id: C-158
title: Stream partial tool output onto running tool cards
pillar: Core
status: backlog
epic: tui-polish-round-2
design:
note: "deferred/tier 3 — the in-flight badge is ALREADY animated per tick (C-109, lib.rs:1793-1797); what is missing is content, and streaming partial bash/task output means changing the entry pipeline, not the renderer"
---

# Stream partial tool output onto running tool cards

## Goal
A long `bash` or `task` call shows an animated `◌ running` badge with live elapsed (C-109,
`lib.rs:1793-1799`) but no content: the summary line only renders once `tool.result` is `Some`
(`lib.rs:1812-1826`). For multi-second ops the user cannot tell whether the op is progressing or
stuck. Show the last line (or a bounded tail) of in-flight output under the header.

## Acceptance
- [ ] A running tool card renders a bounded, redacted tail of partial output that updates as the op
      runs, and is replaced by the normal summary/detail when the result lands — failing-first test
      driving partial output through the entry pipeline.
- [ ] Partial output flows through the same redaction the final result gets; nothing bypasses the
      guarded/redacted result path.
- [ ] The C-109 badge patch and the running-row pairing (`lib.rs:1554-1567`) still hold with the
      extra row present.
- [ ] Ops that produce no incremental output render exactly as today (no empty placeholder row).

## Progress
- (not started)

## Notes
- Correction recorded during review: the original "dead air" framing was wrong — motion already
  exists. This story is about content, and it is deliberately last in the epic because it needs
  incremental output to reach the TUI entry pipeline before `tool.result` is set; that is an event
  path change, not a rendering change.
- Sequence after C-149 so the card layout is settled; otherwise independent of the other stories.
