---
id: D-200
title: Zendesk plugin foundation and read-side ticket API
pillar: Agent
status: done
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [plugins]
note: "CLOSED AS SUPERSEDED (D-214) — not re-done: flux-connectors' connector-pack projects the same four reads under the same names, so there is no flux-side read API left to build"
---

# Zendesk plugin foundation and read-side ticket API

## Goal

Add a first-party Zendesk plugin that can verify auth and gather the bounded ticket evidence required
by triage workflows without ever receiving an endpoint URL or credential value.

## Acceptance

- [x] Manifest declares `zendesk.endpoint`, one Basic `api_token` purpose, `*.zendesk.com` HTTP
      scope, and `zendesk.ticket` datasource records; metadata validation is clean.
- [x] Typed `zendesk.test`, `zendesk.ticket.search`, `zendesk.ticket.show`, and
      `zendesk.ticket.comment.list` operations use only host endpoint-reference HTTP.
- [x] Inputs enforce positive ids/pages and `per_page` 1..100; results preserve pagination and
      search contributes ticket records.
- [x] Failing-first `MockHost` tests cover method/path/query encoding, auth purpose, output, records,
      validation, and prove no raw token appears in calls or results.

## Progress

- 2026-07-30 — implemented as the typed `flux-plugin-zendesk`; the real manifest also passed an
  isolated local install/status and live `plugin call --dry-run` validation without credentials or
  network. Nested workspace build/test/clippy/fmt are green.
- 2026-07-31 — **closed as superseded by D-214, and deliberately not re-done.** The note above said
  "re-do against that layer"; on inspection there is nothing to re-do. flux-connectors'
  `connector-pack` projects `zendesk-test`, `zendesk-ticket-search`, `zendesk-ticket-show` and
  `zendesk-ticket-comment-list` as `zendesk.test`, `zendesk.ticket.search`, `zendesk.ticket.show` and
  `zendesk.ticket.comment.list` — the same four operations under the same names this story's
  acceptance listed. The bounded-page constraints it enforced live in that repo's catalogue schemas
  (`per_page` 1..100, positive ids), and its "no raw token in calls or results" property is the
  pack's, held by `crates/connector-pack/tests/credentials.rs` against all four surfaces
  `Executor::dispatch` scrubs. **No flux-side read API remains to build**, so filing a replacement
  story here would be filing work that belongs to another repository.
