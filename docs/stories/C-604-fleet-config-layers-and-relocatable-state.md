---
id: C-604
title: "Layer Fleet configuration, and separate a fleet's config source from its state store"
pillar: Core
epic: fleet-harness-throughput
status: ready
priority: 22
areas: [flux-cli, flux-config]
note: "safety invariants like 'a writer gets no shell' are per-directory today, so one forgotten fleet dir reintroduces the hole"
---

# Layer Fleet configuration, and separate config from state

## Goal

Let reusable Fleet direction — provider, authored loops, agent instructions, safety invariants,
budgets — be declared once and inherited, while a workspace declares only what is genuinely local.
And let a fleet's durable state live somewhere other than beside its config, so the same authored
configuration can drive a fleet that runs elsewhere.

## Acceptance

- [ ] Fleet configuration resolves through layers (user/global → workspace), following the existing
      precedent in `flux-config` for `[agent]` settings rather than inventing a second mechanism.
- [ ] A local layer may **narrow** but never **widen** a safety invariant. Failing first: a workspace
      layer that re-grants `shell` to a writer whose global layer forbids it is refused, in the same
      spirit as `mode: read-only` already refusing capabilities outside `read`/`git-read`.
- [ ] The fleet's state store is addressable independently of its config root, so one authored config
      can back several fleet instances without them sharing or clobbering `state.json`.
- [ ] `flux fleet validate` reports the resolved layers and where each setting came from — a fleet
      whose safety posture depends on inheritance must be able to show its provenance.

## Notes

- **The motivating incident.** The `story-worker` template carried `shell`, so a writer could run
  arbitrary `bash` — which defeats the entire ceiling: fences, typed effects, `permission_subjects`
  and the operation allow-list are all bypassable through arbitrary argv. It was replaced with the
  typed `rust`/`node` bundles. But that fix lives in **one** `fleet.toml`, at
  `[loop_profiles.implementation] revision = "7"` of one directory. Every other fleet directory —
  present or future — still grants shell, and nothing detects it. A safety invariant that has to be
  re-applied per directory is not an invariant.
- **What is already relocatable, and what is not.** `worktree_root` and each
  `[[repositories]] root` are already configurable, so the large disposable working area can live on
  a different disk or volume today. What is glued to `--root` is the pair that matters for running
  elsewhere: `fleet.toml` discovery and `.flux/fleet/state.json`. Splitting those is the actual
  request.
- **Rough division of concerns:**
  - *Global*: provider/model (including that bare `opus` bills a credit-less API key while
    `claude/opus` uses the subscription), the authored loop library, agent instruction templates,
    safety invariants, round/history/token budgets, machine capacity.
  - *Local*: member repositories, `canonical_ref`, `gate`, board selection, wave composition,
    `worktree_root`.
- `FleetConfig` is `#[serde(deny_unknown_fields)]` and read from a single path today, so layering is a
  real change to how the config is resolved, not just a merge helper.
- Related: [C-602](C-602-fleet-workers-report-activity-back-to-the-coordinator.md) — for genuinely
  remote workers the location question mostly dissolves, because each worker already gets an isolated
  store and worktree; what matters there is the transport.
