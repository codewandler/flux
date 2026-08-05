---
id: D-251
title: "One authority-subject grammar: `datasource:<name>/<entity>[/<id>]` — live ops gain the prefix, web.page normalizes, boards use `board:`"
pillar: Agent
status: backlog
areas: [flux-spec, flux-capabilities, flux-policy]
note: "Decision 0006 rule 13 — flux's permission subjects currently use five different datasource grammars; this story leaves exactly one, plus a separate `board:` namespace for the write-capable surface"
---

# One authority-subject grammar: `datasource:<name>/<entity>[/<id>]`

## Goal

Normalize every datasource permission subject onto one canonical grammar —
`datasource:<name>/<entity>` with `*` per segment and an id segment where one is meaningful — so a
policy author writes one shape of grant sentence and `datasource.read` reasoning stays uniform.
Today the tree carries several: live ops use bare `<domain>/<entity>`, harness history uses
`datasource:harness.<id>`, the web page sink uses its own subject spelling, and board subjects live
under `<name>/item/<id>`.

## Acceptance

- [ ] A census of every datasource-flavored permission subject in the tree opens this story's
      implementation (live ops, indexed retrieval, harness history, web.page, boards), recorded in
      Progress — normalize from evidence, not recollection.
- [ ] Live-mode subjects gain the missing `datasource:` prefix; indexed and harness-history subjects
      parse in the same canonical grammar; the web page sink subject normalizes into it.
- [ ] Board subjects move to their own `board:` namespace (`board:<name>/item/<id>`) — boards are
      not datasources (Decision 0006), so they do not share the prefix. Coordinated with L-130 and
      A-148.
- [ ] `*` is honored per segment, and an id segment is accepted exactly where one is meaningful.
- [ ] Existing policies keep working or the break is explicit: the story decides (and tests) whether
      old-grammar grants are migrated, accepted with a deprecation diagnostic, or refused with an
      error naming the canonical spelling. Silent non-matching — a grant that used to gate and now
      matches nothing — is the outcome this story exists to prevent; failing-first test pins it.
- [ ] The public permissions documentation shows the one grammar, and
      `website/docs/agent/datasources.md`'s authority section matches.
- [ ] Standard gate green in both workspaces.

## Progress

- (not started)

## Notes

- Filed 2026-08-04 by C-514 from Decision 0006 rule 13.
- Known subject spellings to reconcile (starting points, not the census):
  `crates/flux-capabilities/src/datasource/live.rs:176` (`<domain>/<entity>`, unprefixed),
  `crates/flux-capabilities/src/datasource/ops.rs:153` (`datasource:<source>/<entity>`),
  the harness selector (`datasource:harness.<id>`),
  `crates/flux-web/src/lib.rs:67` (`datasource:web.page`, no entity segment),
  `crates/flux-plugin/src/host/loading.rs:510` (`datasource:<plugin>`), and
  `crates/flux-capabilities/src/datasource/board.rs` (`<name>/item/<id>`, unprefixed).
