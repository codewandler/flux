---
id: D-201
title: Safe Zendesk triage mutations
pillar: Agent
status: blocked
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [plugins]
note: "WITHDRAWN before release — plugin removed pending flux-connectors interop; write-safety rules carry over"
---

# Safe Zendesk triage mutations

## Goal

Expose the narrow writes a deterministic support workflow needs while preventing stale overwrites,
silent tag replacement, and accidental public replies.

## Acceptance

- [x] Typed `zendesk.ticket.update`, `zendesk.ticket.comment.add`, and
      `zendesk.ticket.tag.add` operations declare honest write/network/semantic authority.
- [x] Every input requires `ticket_id` and `updated_stamp`; requests set `safe_update=true`, and
      conflicts surface for refetch rather than being retried internally.
- [x] Empty updates and empty tag/comment payloads fail before HTTP; comments default private and
      only an explicit `public=true` creates an external reply; tags are additive.
- [x] Failing-first `MockHost` tests pin request bodies, validation, private default, public mode,
      additive endpoint, and metadata coherence.

## Progress

- 2026-07-30 — shipped safe update/comment/additive-tag calls. All three declare `write_db`; comment
  creation conservatively declares `send_external` even though the default note is internal.
  Eight Zendesk tests pin validation, authority, and exact request bodies.
