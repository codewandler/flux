---
id: C-184
title: Retire the calendar FlowEffect — a domain noun in a consequence vocabulary
pillar: Core
status: done
priority:
epic:
design:
note: "FlowEffect::Calendar is the only application-domain variant in an otherwise consequence-class vocabulary; ZERO declaration sites repo-wide, yet the default policy grants flow.calendar and its lowering carries no Network host effect — dead, mis-designed, and taught in the docs"
---

# Retire the `calendar` FlowEffect — a domain noun in a consequence vocabulary

## Goal
Every other `FlowEffect` variant classifies a *consequence* (irreversibly deletes, sends
externally, moves money, writes persistently); `Calendar` names an *application domain* — day-one
vocabulary from the original flux-lang extraction (`1cd0302`) that nothing has ever declared: no
built-in op, no plugin manifest, no example, no test. It is nonetheless taught in the language docs,
granted (approval-gated) in the default policy, and lowered without the `Network` host effect any
real calendar mutation would need. Retire it — without breaking the day-old protocol 1.x line it
rides on (`semantic_effects: Vec<FlowEffect>` serializes the enum on the plugin wire, and flux-spec
is published at 1.0.0).

## Acceptance
- [ ] `FlowEffect::Calendar` is `#[deprecated]` with a note directing authors to `send_external`
      (or `write_db`) and naming its removal at the next protocol major; the crate still compiles
      warning-free under `-D warnings` (internal match arms allowed explicitly).
- [ ] The wire contract holds: a manifest declaring `"calendar"` still deserializes, round-trips
      `tag`/`from_tag`, and lowers — pinned by test — so no existing third-party plugin can be
      broken by this story.
- [ ] The default policy no longer grants `flow.calendar`; like `flow.money` and `flow.delete` it
      is default-deny. Failing-first test: an op declaring the `calendar` semantic effect is denied
      under the default policy instead of approval-prompted.
- [ ] The docs stop teaching it: the effect tables/lists in the website language docs and
      `crates/flux-lang/docs/` no longer list `calendar`; design docs that used it as an example
      name a real variant instead. Historical records (CHANGELOG, closed stories, archive) stay
      untouched.
- [ ] The vocabulary invariant is written down on the enum: variants are consequence classes
      (what could go wrong, who sees it, can it be undone) — never application domains — so the
      next `crm`/`tickets`/`dns` proposal has a rule to fail against.
- [ ] `codewandler-flux-spec` is bumped to 1.1.0 (deprecation is a minor); the flux-line crates and
      the rest of the protocol line are untouched.
- [ ] Standard gate: build, test, clippy `-D warnings`, fmt — both workspaces — plus
      `flux-codegate`.

## Progress
- 2026-07-28: Filed from Timo's review request ("effect 'calendar' makes 0 sense"). Review
  findings: introduced in `1cd0302` (day-one flux-lang vocabulary), moved to flux-spec by C-141 for
  the plugin wire; zero declaration sites repo-wide (`rg` over built-ins, plugins/, examples,
  tests); default policy granted `flow.calendar` with approval (dead grant); lowering returned no
  host `Effect` (a real calendar op is network egress). Full removal rejected for now: flux-spec /
  flux-plugin-protocol / host-kit went 1.0.0 TODAY, and deleting a wire enum variant is a serde
  deserialization break → protocol 2.0.0 cascade + pack cut, disproportionate for a dead variant.
  Deprecate now, delete at the next protocol major.
- 2026-07-28: DONE. `#[deprecated(since = "1.1.0")]` on the variant (serde/JsonSchema derives stay
  quiet; the three internal `tag`/`from_tag`/`lower` arms carry an explicit `#[allow(deprecated)]`
  with a wire-compat comment); vocabulary invariant written on the enum docs; flux-spec bumped
  1.0.0 → 1.1.0 (root pins `version = "1"` — no dep churn; plugins/Cargo.lock re-locked, which the
  codegate `plugin_builds_exclude_host_only_crates` metadata gate caught first). Default policy:
  `flow.calendar` dropped from the approval-gated externally-visible grant — now default-deny like
  `flow.money`/`flow.delete`. Failing-first verified by temporarily restoring the grant
  (`ApprovalRequired` vs expected `Deny`). Docs swept: website `types-and-effects` table +
  `flows-and-syntax` tag list, `crates/flux-lang/docs/{reference,syntax}.md`, `plugins/AUTHORING.md`,
  and the two design docs (`flux-flow`, `typed-authority-requirements`) annotated; the
  flux-markdown corpus fixture and historical records deliberately untouched; `calendar_event`
  (ThingKind) and the `calendar.read` example tool name are different vocabularies, untouched.
  New tests: `flux-spec::deprecated_calendar_stays_wire_compatible_until_the_next_protocol_major`
  (serde round-trip + tag round-trip + lowering pinned),
  `flux-policy::default_local_grants_deny_the_deprecated_calendar_action` (deny pinned, with a
  `flow.send_external` contrast assert proving the surviving grant intact). No WHATS-NEW entry:
  zero declaration sites exist anywhere, so there is no customer-visible change to announce.
  Full two-workspace gate green (build/test/clippy `-D warnings`/fmt + codegate).

## Notes
- The removal-at-2.0 queue should be recorded in the protocol-decoupling design doc when that major
  is planned; this story deliberately does not open it.
- `ThingKind::CalendarEvent` (flux-lang value templates) is a different vocabulary with different
  rules (domain nouns are its point) — deliberately out of scope.
