---
id: D-253
title: "flux-datasource 1.x counterparties — flux-connectors and flux-exchange compose the published contract; board.rs is flux-internal within 1.x"
pillar: Agent
status: backlog
areas: [flux-datasource]
note: "Decision 0006 rule 4: flux owns the wire vocabulary on its own protocol line; after plugin removal the consumers change but the composition rule (Decision 0004, connector-secrets) applies verbatim"
---

# flux-datasource 1.x counterparties — the published wire vocabulary after plugin removal

## Goal

Record and enforce the counterparty change on the published datasource contract crate
(`codewandler-flux-datasource`, protocol line 1.x): after plugin removal (C-506) its consumers
become **flux-connectors** and **flux-exchange**, under the Decision 0004 `connector-secrets`
composition rule — one repository owns the format and its public port; consumers compose the
published crate, never duplicate it, never reach into private representation, and never publish a
second independently shaped contract with the same or a competing name. A mismatch is a release
blocker.

## Acceptance

- [ ] The crate's protocol-line documentation (`crates/flux-datasource/Cargo.toml` header comment
      and crate docs) names the post-plugin-removal counterparties and the Decision 0004 composition
      rule, replacing any framing that scopes the line to plugin-manifest travel only.
- [ ] `board.rs` is marked **flux-internal within 1.x**: the board vocabulary left the datasource
      concept (Decision 0006), so external counterparties must not build on the module inside this
      protocol line; its extraction or re-homing is decided with the first-class-board
      generalization (A-148, Milestone 3), not here.
- [ ] The record/row/page/schema vocabulary moves only when the wire vocabulary changes — the
      versioning rule is stated with the counterparties so a workspace-version bump is never
      mistaken for a protocol bump (the existing C-143 rule, restated for the new consumers).
- [ ] A drift check exists or is chartered: the published contract and its consumers' pinned
      versions are compared the way `scripts/check-host-kit-protocol-drift.sh` compares the plugin
      protocol line, so "mismatch is a release blocker" is mechanical rather than prose.
- [ ] Standard gate green in both workspaces.

## Progress

- (not started)

## Notes

- Filed 2026-08-04 by C-514 from Decision 0006 rule 4.
- The consumer change is gated on the migration program: plugins remain a compatibility consumer
  until C-506 removes them; this story's documentation half can land before that, the enforcement
  half rides the migration.
- Cross-repository counterparts consume, not fork: flux-connectors' datasource surface (0006 rule 6)
  and flux-exchange's read seam (rules 7–8) both speak this crate's vocabulary.
