---
id: C-634
title: "A public tutorial takes a reader from empty repo to an applied fleet wave"
pillar: "Core"
status: backlog
epic: fleet-init-interviews-an-operator-and-writes-a-working-fleet
areas: [website]
note: "fleet loop authoring is 100% undocumented and fleet init scaffolds only commented-out TOML; six-lesson series under website/docs/tutorial/fleet/ with website_contract.rs pins and embedded-docs zip in the same commit"
---

# A public tutorial takes a reader from empty repo to an applied fleet wave

## Goal

Fleet loop authoring is completely undocumented and `flux fleet init` scaffolds only a
commented-out TOML — a reader cannot get a single worker running from the public docs. The
dogfood workspace holds battle-tested exemplars of every missing piece (checkpoint implementation
loop, coordinator loop, instruction files, agent template with ceiling and fences, repository
gate). Turn them into a six-lesson series with a real purpose, in the existing tutorial voice.

## Acceptance

- [ ] A series under website/docs/tutorial/fleet/ (hub + board + fleet-config/loop-authoring + first-wave + integrate-apply + unattended) exists, wired into sidebars.js, with the embedded-docs zip regenerated in the same commit.
- [ ] Every ```flux fence parses and is a formatter fixed point (existing sweeps), and website_contract.rs pins the series including a sidebars containment assertion.
- [ ] The loop-authoring lesson teaches the checkpoint pattern, tool lists bounded by the capability set, and the staged/finalize_plan contract — the three things that killed real waves.
- [ ] CLI surface the tutorial introduces (e.g. fleet run --prepare-only) is documented in agent/cli.md and docs/usage.md (both pinned by cli_reference_covers_every_public_subcommand).
- [ ] Scaffold emission itself is tracked by this epic's other stories, not duplicated here.
