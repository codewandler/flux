---
id: C-75
title: Type Slack channel, user, and message read families
pillar: Core
status: done
epic: typed-plugin-migration
design: plugins/TYPED-MIGRATION.md
note: bind the bounded Slack list/thread handlers to typed inputs and stable envelopes without narrowing vendor objects
---

# Type Slack channel, user, and message read families

## Goal

Move the bounded Slack channel, user, and message read families onto the executable typed-plugin
contract. Keep Slack's evolving channel, member, and message objects intact while making each
operation's input and stable result envelope impossible to drift from its manifest schemas.

## Acceptance

- [x] `slack.channel.list`, `slack.user.list`, `slack.message.list`, and the message-show
      `slack.thread` operation register through `PluginBuilder::operation_typed` with inputs that
      derive `Deserialize + Serialize + JsonSchema` and reject unknown fields.
- [x] Each migrated handler accepts its typed input exactly once and returns a generated, truthful
      output schema for its stable envelope; Slack-owned channel/member/message objects and
      response metadata remain lossless rather than being narrowed to a guessed vendor schema.
- [x] Failing-first manifest tests pin input and output schemas for all four operations, including a
      path-aware wrong-field-type failure and an unknown-field refusal that occurs before HTTP.
- [x] Hermetic handler tests preserve API paths, bot-token auth, query/limit filtering,
      `text_format` rendering, cursor/metadata passthrough, and channel/user datasource
      contributions while retaining unmodeled vendor fields.
- [x] Operations outside this bounded batch remain on `operation_flexible` only where their
      payload is intentionally open or not yet migrated, with that phased rationale recorded in
      `plugins/TYPED-MIGRATION.md`.
- [x] The Slack package build, tests, clippy, formatting check, and host-kit guest dependency
      boundary pass from the nested plugin workspace.

## Progress

- 2026-07-15 — Story opened from the C-68 migration matrix and Slack D-36 parity audit; scoped to
  channel/user list plus message list/thread so vendor-specific write/search/file semantics remain
  outside this batch.
- 2026-07-15 — Failing-first schema test observed open inputs/missing output schemas; input-drift
  test proved an unknown channel filter reached HTTP before typed dispatch.
- 2026-07-15 — Migrated all four operations to typed handlers with map-backed open Slack objects,
  lossless response extensions, exact request/auth and contribution regressions, and generated
  schema pins. `cargo build -p slack`, `cargo test -p slack` (69 tests),
  `cargo clippy -p slack --all-targets -- -D warnings`, `cargo fmt -p slack -- --check`, and
  `cargo test -p codewandler-flux-host-kit --test guest_dependency_boundary` are green.

## Notes

- Migration matrix: [`plugins/TYPED-MIGRATION.md`](../../plugins/TYPED-MIGRATION.md).
- Slack parity audit: [`.flux/plans/d36-parity-audits/slack.md`](../../.flux/plans/d36-parity-audits/slack.md).
- Primary implementation: `plugins/slack/src/{manifest.rs,schema.rs,operations/messages.rs,operations/workspace.rs}`.
