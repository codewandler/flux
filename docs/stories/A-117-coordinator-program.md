---
id: A-117
title: "The durable native fleet supervisor and complete `flux fleet` CLI"
pillar: Agent
status: ready
priority: 49
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli, flux-orchestrate, flux-session, flux-tui]
note: "Decision 0010 headline — the former reference Program becomes a supported CLI supervisor over local Flux sub-agents"
---

# The durable native fleet supervisor and complete `flux fleet` CLI

## Goal

Ship the product coordinator: a durable, restartable supervisor that links federated planning boards
to local execution boards and exposes every lifecycle action through `flux fleet`.

## Acceptance

- [ ] Every fleet has exactly one durable reserved `main` coordinator. All user requirements, tasks
      and agent follow-ups enter its acknowledged intake; workers cannot register another
      coordinator or mutate the roadmap directly.
- [ ] The coordinator plans against revisioned goals scoped to values, company, workspace, project
      and repository. Human decision mode prompts with options/recommendation; auto mode admits a
      fresh adversarial decision agent that challenges the recommendation against those goals.
- [ ] `.flux/fleet.toml` supports reusable main/worker instruction files and named templates. The
      coordinator may also admit ephemeral agents with temporary instructions/model/mode/
      capabilities/fences, subject to the same closed validation, leases and worker limits.
- [ ] Public commands cover `init`, `doctor`, `refresh`, `validate`, `start`, `stop`, `run`, `task`,
      `message`, `cancel`, `resume`, `apply`, `status`, `schedule`, `events`, `logs`, `note`, `agents`,
      `worktrees`, `inspect`, `dashboard`, `schema`, `call` and `skill`; every command supports the
      applicable C-547 human/JSON/NDJSON contract.
- [ ] A bare run selects the highest-priority dependency-satisfied wave; explicit BoardRefs use the
      same checks. Maximum workers defaults to three, maximum wave to ten and rework to two, with
      closed configuration validation.
- [ ] The durable manifest/event log pins board revisions, source commits, write sets, worktrees,
      sessions, attempts, handoffs, reviews, gates and candidate branches. Restart/resume derives all
      in-flight state from it and execution boards, never process memory.
- [ ] Read-only maintenance tasks are the default. Write tasks require explicit mode, one writer and
      one worktree. Follow-ups have accepted/delivered/completed acknowledgements and bounded waits.
- [ ] The host enforces one writer/worktree per story, dependency/write-set serialization, C-244
      evidence, fresh read-only review, C-245 rework and C-242 integration/apply. Prompt text cannot
      bypass any invariant.
- [ ] Offline failing-first journey spans two fixture repositories and three stories: two independent
      starts, one dependency waits for integration, one review reworks the same session, one parks,
      restart resumes, final gates run once, and green apply merges locally with no network/model.
- [ ] Redaction occurs before event persistence; cancellation and crashed workers leave durable,
      inspectable state. Status uses a read lane independent of active dispatch.
- [ ] `flux fleet skill` renders a concise version-matched safe operating loop, uses progressive
      disclosure through `fleet schema`, and every example command executes in the offline fixture.
- [ ] Website docs, migration guide, `WHATS-NEW.md` and changelog are updated. The integrated fleet
      wave passes the full repository and embedded-docs gates once.

## Notes

- Depends on the board wave plus C-244, C-245 and C-242. C-551 supplies the final roadmap parity and
  helper-retirement surface.
