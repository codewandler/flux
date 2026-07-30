---
id: D-200
title: Zendesk plugin foundation and read-side ticket API
pillar: Agent
status: blocked
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [plugins]
note: "WITHDRAWN before release — plugin removed pending flux-connectors interop; re-do against that layer"
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
