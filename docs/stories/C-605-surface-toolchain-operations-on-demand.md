---
id: C-605
title: "Surface toolchain operations from the repository, not from prose"
pillar: Core
epic: fleet-harness-throughput
status: ready
priority: 28
areas: [flux-cli, flux-tools, flux-flow]
note: "naming cargo_* in shared instructions is wrong in a non-Rust repo; a small ceiling is already fully visible in the request schema"
---

# Surface toolchain operations from the repository, not from prose

## Goal

Stop naming toolchain operations in instruction prose. An agent's available set is already in its
request schema; what it needs is for that set to reflect the repository it was assigned, and for a
small ceiling to be self-evident without narration.

## Acceptance

- [ ] An agent template can express its validation capability without naming
      language-specific operations, so one shared instruction body is correct in a Rust repo, a Node
      repo and a mixed one.
- [ ] Toolchain operations surface based on the assigned repository rather than being granted
      uniformly. Failing first: a worker assigned a repository with no Cargo manifest does not receive
      `cargo_*` in its available set.
- [ ] When an agent's total ceiling is small (roughly ten operations or fewer), the whole set is
      presented up front and no prose enumerates it — no system-prompt text is spent describing tools
      the schema already carries.
- [ ] Instruction bodies for Fleet agents contain no operation names.

## Notes

- **What prompted it.** Removing `shell` from the story worker meant granting `rust` + `node`, and the
  worker instructions were then edited to name `cargo_check`, `cargo_test`, `cargo_clippy`,
  `cargo_fmt`, `npm` explicitly. Those instructions are shared across `flux`, `flux-connectors` and
  `flux-exchange`, and under [C-604](C-604-fleet-config-layers-and-relocatable-state.md) would be
  shared more widely still — so the enumeration is wrong the moment a member repository is not Rust.
  The prose has since been made capability-agnostic; this story removes the underlying need for it.
- **The mechanism already half-exists.** `ToolGroup` carries `surface_when` matchers keyed on
  `KIND_TURN_INTENT`, and `build_families` reads their `routing_signals` — so demand-driven surfacing
  is an established pattern for groups. What is missing is repository-derived selection for toolchain
  groups, and the recognition that a small authored `tools:` ceiling needs no narration at all because
  every entry is already in the provider request.
- Related evidence from a live worker (wave-286): 35 `read` and 14 `grep` calls against 25 `edit`s,
  with the indexed datasource operations in its ceiling and unused, and `read_many` *absent* from the
  ceiling while the shared explore prompt instructed the model to use it. Prose that references
  operations an agent does not have is worse than silence — it spends context describing an
  unfollowable instruction.
