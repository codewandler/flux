---
id: D-201
title: Safe Zendesk triage mutations
pillar: Agent
status: done
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [plugins]
note: "CLOSED AS SUPERSEDED (D-214) — the write-safety rules did carry over: the connector catalogue declares safe_update as a const true, requires updated_stamp, and defaults comments to internal notes"
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
- 2026-07-31 — **closed as superseded by D-214.** The note's claim that "write-safety rules carry
  over" was checked rather than assumed, and it holds: `providers/zendesk.toml` in flux-connectors
  declares `updated_stamp` **required**, `safe_update` as a `const true` the caller can neither supply
  nor drop (the comment there records why: dropping it turns every write into a last-write-wins race),
  `idempotency = "conditional"`, a comment that is an internal note unless `public` is explicitly
  true, and additive tagging. It also declares something this story did not: a `{ ticket, audit }`
  response shape, because a *flat* body Zendesk accepts, ignores and answers `200` to is
  indistinguishable from a real update by status code alone — `audit.events` is the only place those
  two look different.
  **One thing did not carry over, and it is recorded rather than lost:** the plugin exposed these
  writes on a separate `flux plugin call` surface, so they were absent from a session's registry
  unless invoked deliberately. Registering the connector pack brings all seven operations into the
  registry together. The boundary is now approval and policy rather than registry absence, which is a
  weaker default and is why `docs/zendesk-triage.md` and the website page now state it explicitly
  instead of implying the old shape.
