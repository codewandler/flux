---
id: C-239
title: "The native fleet runs the planning-to-integration loop through Flux CLI (epic)"
pillar: Core
status: in-progress
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli, flux-orchestrate, flux-runtime, flux-tui]
note: "Decision 0010 product epic — local Flux sub-agents, durable manifests/events, one writer per story, one final gate, explicit local apply, and no coordinator scripts"
---

# The native fleet runs the planning-to-integration loop through Flux CLI

## Goal

Turn the delivered fleet primitives into a supported local coordinator that Claude, Codex or a human
operates entirely through `flux fleet`. The model selects and reviews; the host enforces isolation,
evidence, rework budget, integration, gating and the publication fence.

## Acceptance

- [ ] A bare `flux fleet run` chooses a dependency-satisfied wave of at most ten concrete planning
      `BoardRef`s; explicit items use the same validation.
- [ ] Every story has one persistent native Flux writer session and one isolated worktree. Typed
      write sets overlap only through serialization; fenced ledger paths are never delegated.
- [ ] Behavioral handoff proves a test-only failure and targeted success; a fresh reviewer returns
      PASS/REWORK/PARK; two reworks return to the same session and the third parks.
- [ ] Repository commits integrate in dependency order and one unskippable full gate runs on the
      final tree. Red preserves the exact candidate; green leaves local `fleet/<wave>` branches.
- [ ] Only `flux fleet apply` revalidates and merges a green candidate. Fleet never pushes,
      publishes, releases, deploys or deletes worktrees automatically.
- [ ] The durable supervisor supports start/stop/run/task/message/cancel/resume/apply/status/schedule/
      events/logs/note/agents/worktrees/inspect/dashboard/schema/call and renders `flux fleet skill`.
- [ ] The roadmap parity journey retires its worker, schedule, activity, context, progress and
      worktree coordination helpers only after side-by-side tests are green.
- [ ] Offline headline proof: two local fixture repositories, two independent items plus one
      dependency, one rework and one parked result survive coordinator restart with no network.

## Progress

- 2026-08-05 — respecified by Decision 0010. The former reference-Program-only product boundary and
  automatic publication path are superseded; the delivered primitives remain inputs.

## Notes

- Delivery stories: C-244, C-245, C-242, A-117 and C-551 after the board wave.
- Direct Claude/Codex process workers and remote A2A code workers remain later transports. Claude and
  Codex are supported callers of this CLI in V1.
