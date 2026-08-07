---
id: C-666
title: "A scheduled scout app inspects tickets on a timer"
pillar: "Core"
status: backlog
priority: 12
epic: scouting-makes-the-backlog-schedulable
areas: [flux-app]
depends_on: [C-665]
note: "the schedule channel, the trigger binding and the app runner all exist and are e2e tested; nothing new is needed to make something run on a clock"
---

# A scheduled scout app inspects tickets on a timer

## Goal

Run the scout as a flux app woken by a clock, not as a bespoke daemon.

Every part of the mechanism already exists and is tested:

- `crates/flux-channels/src/adapters/schedule.rs` — the schedule adapter (`kind = "schedule" |
  "cron"`), a real timer accepting 5-field crontab or the native 6/7-field seconds-first form, plus
  a one-shot `startup`.
- `crates/flux-channels/tests/e2e.rs::cron_tick_runs_journey_via_app` — proof that a tick wakes a
  real `App` and its trigger runs a journey.
- `TriggerDecl` (`crates/flux-lang/src/program.rs:169`) — `on`, `run`, and an optional `agent`, so a
  journey runs as a named persona.
- `flux app run <program>` serves declared channels until Ctrl-C, and
  `examples/channels-app.flux` is a working template of exactly this shape.

So this story writes a program, not a scheduler.

Each tick scouts the **N least-recently-scouted** items and stops. Bounded work per tick is the
point: a missed tick costs latency, never correctness, and the debt counter from `C-665` makes a
stopped app show up as a rising number instead of as silence — which is the failure that has already
cost this project a wave.

## Acceptance

- [ ] A `.flux` program declares a `schedule` channel and a trigger that runs the scout journey, and
      `flux app run` serves it.
- [ ] One tick scouts at most N items, chosen least-recently-scouted first, and a tick that finds no
      debt does nothing and says so.
- [ ] The scout derives, per item: `areas`, the repositories it reaches, the services it touches, and
      its discipline — from reading the story, its linked design, and the paths those name.
- [ ] It writes **only** through the `C-665` annotation operation. Its capability set contains no
      `edit`, no `git`, no `shell`, and a test pins that, exactly as the story worker's fences are
      pinned.
- [ ] The journey's `ai_segment` tool list stays within the **four tool-family cap**. A wider list
      kills workers instantly and silently, so this is asserted rather than assumed.
- [ ] An overrunning tick does not overlap the next one; a scout already running is a no-op, not a
      second scout.
- [ ] Failing first, an end-to-end test mirrors `cron_tick_runs_journey_via_app`: a tick fires against
      a fixture board and one item gains `areas` and a stamp.

## Notes

- **Discipline is not `task_kind`.** Discipline describes the work (`writing`, `engineering`,
  `development`, `planning`, `architecture`, `research`); `task_kind` selects the loop; the persona
  selects the agent. A `discipline → task_kind` map in `fleet.toml`, shaped like the existing
  `[loop_policy]`, joins them without fusing them — which is what lets `C-669` request a new persona
  without touching a loop.
- The app program and its `agent_templates` entry are fleet configuration and live with the fleet,
  not in this repository's crates. The story's own code artefacts are the program, the tests, and the
  discipline map.
- Model the program on `examples/channels-app.flux`; model the journey's tool use on
  `.flux/fleet/loops/main-coordinator.flux`, which is the existing precedent for calling board
  operations from an authored loop.
