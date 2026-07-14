---
id: C-58
title: Make web.fetch and web.crawl record persistence honest in effects
pillar: Core
status: done
note: "2026 codebase review: web read tools advertise network-only effects while optionally contributing persistent web.page datasource records."
---

# Make web.fetch and web.crawl record persistence honest in effects

## Goal

Align the web tools' declared effects, permission subjects, and approval preview with their actual behavior when a record sink is configured, so a network read cannot silently become durable local storage.

## Acceptance

- [ ] The intended contract is decided and documented: contributed `web.page` records are either strictly ephemeral evidence or an explicit durable datasource/write side effect.
- [ ] If records are ephemeral, code enforces that they cannot be persisted by the `web.fetch`/`web.crawl` read tools.
- [ ] If records are durable, `web.fetch` and `web.crawl` specs/effects/access/permission subjects honestly disclose the persistence side effect in policy and approval previews.
- [ ] Regression tests cover both tools with a configured record sink and assert that declared effects match the observed record-contribution behavior.
- [ ] Changelog/WHATS-NEW are updated if the user-facing approval wording or behavior changes.

## Progress

- 2026-07-14 — filed from repository review finding. The issue is not web egress guarding; URLs are still passed through scoped guards. The mismatch is between advertised network-only effects and optional record contribution.

## Notes

- `web.fetch` currently declares only `Effect::Network` and `AccessKind::Network`, but contributes an HTML `Record` through `sink.contribute(&[record])` when `self.records` exists.
- `web.crawl` currently declares only `Effect::Network` and `AccessKind::Network`, but accumulates one `web.page` `Record` per HTML page and contributes the batch through `sink.contribute(&records)` when `self.records` exists.
- Relevant files: `crates/flux-web/src/fetch.rs`, `crates/flux-web/src/crawl.rs`, and the `RecordSink` contract in `crates/flux-web/src/lib.rs`.
