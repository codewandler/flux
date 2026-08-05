---
id: C-588
title: "The real roadmap runs from the native workspace Board without legacy coordination glue"
pillar: Core
status: in-progress
epic: first-class-board
design: docs/designs/native-workspace-board-cutover.md
areas: [flux-cli, flux-config, flux-capabilities, docs]
depends_on: [C-550, C-551]
note: "corrective adoption after the roadmap fixture exposed zero program/wave metrics, false open decisions and continued README/AGENTS/script dependence"
---

# The real roadmap runs from the native workspace Board without legacy coordination glue

## Goal

Make native Flux Board the complete cross-repository program and scheduling authority, independently
of Fleet, then prove the actual Flux family can be operated with Flux alone.

## Acceptance

- [ ] Failing first, a hermetic four-root fixture reproduces the shipped adoption defect: plain
      `flux board` at the roadmap root cannot find a Board; workspace stats expose empty tranche/wave
      placeholders; accepted decisions demand attention; Fleet schedules an unscheduled ready story;
      and removing the helper/README/AGENTS scheduling text leaves no complete path.
- [ ] `.flux/board.toml` implements the closed `flux.board-workspace/v1` contract for default Board
      selection, document roots, authoritative member bindings/canonical refs, one active milestone,
      ordered program lanes with exact BoardRefs/cross-repository dependencies, and repository-local
      waves of at most ten items. It is usable with no `.flux/fleet.toml`.
- [ ] With a configured default, plain `flux board show|check|items|get|query|next|graph|stats|report`
      selects the workspace; explicit `--scope`/`--board` still wins, and mutations still resolve and
      authorize the concrete member rather than a workspace-wide subject.
- [ ] Board check rejects escaping/overlapping member roots, missing or ambiguous BoardRefs, duplicate
      program ids/lane order, missing/later-milestone/cyclic dependencies, unknown active milestones,
      cross-repository wave membership, duplicate wave items and wave size above ten.
- [ ] `board next` and `fleet schedule` share the active-milestone projection, combine repository and
      program dependencies, preserve stable program/wave order and never admit an unrelated ready
      story when a program catalogue exists. Fleet reads the Board config rather than owning tranche,
      wave or group schedule fields.
- [ ] Accepted/decided/superseded roadmap decision documents normalize correctly; only a genuinely
      open structured decision creates `attention_required` or blocks its linked work.
- [ ] The metric/report schema replaces `tranche_lanes` with truthful `milestone_lanes`, computes
      program and wave completion from authoritative member story state, and keeps unsupported
      historical dimensions explicit rather than zero. No current public schema/docs use tranche as
      a scheduling concept.
- [ ] A committed roadmap smoke fixture uses plain installed `flux board` plus `flux fleet` across
      Flux, Connectors and Exchange, reproduces the authorized active-milestone order, refuses one
      unscheduled ready story, and references no retired helper, Track generator or private socket.
- [ ] Public Board/Fleet docs, generated skills/schema, C-550/C-551 corrective notes, changelogs and
      embedded docs explain that README/AGENTS are optional prose/policy rather than scheduler input;
      targeted tests, full repository gate and embedded-doc freshness gate pass.

## Progress

- 2026-08-05 — filed after direct operator audit of the supposedly complete native cutover.
