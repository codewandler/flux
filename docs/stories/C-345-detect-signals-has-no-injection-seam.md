---
id: C-345
title: "`detect_signals` has no injection seam, so 30 tests probe the operator's home"
pillar: Core
epic: road-to-stable
status: ready
priority: 5
areas: [flux-runtime, flux-flow, flux-app]
note: "C-332's census, tranche B. `detect_signals(cwd)` reaches skill/command discovery and `~/.kube/config` with the process's real HOME; every flux-flow engine turn calls it via `surfaced_op_names`, so the surfaced tool catalog 26 engine tests assert against is a function of the developer's home"
---

# `detect_signals` probes the operator's home

## Goal

`flux_runtime::detect_signals(cwd)` (`crates/flux-runtime/src/lib.rs:2077`) is the per-turn workspace
probe that decides which evidence-gated tool groups surface. It reaches user-global skill and command
discovery, and `~/.kube/config`, using the process's real `HOME` — it takes a `cwd` and nothing else.

That makes it the last of the three injection points [C-297] identified and never filed. It is also
the one with the widest blast radius, because `flux_flow::engine::surfaced_op_names`
(`crates/flux-flow/src/engine.rs:1848`, called at `:1870`) runs it on **every turn**: the surfaced
op catalog that 26 `flux-flow` engine tests assert against is, today, partly a function of what the
developer has in `~/.claude/commands` and `~/.flux/skills`.

[C-332](C-332-home-reading-tests-need-an-injection-seam.md) established the shape to copy —
`load_config_in(cwd, &DiscoveryEnv)` beside `load_config(cwd)`, exactly as C-297 did for
`discover_skills_in` / `discover_commands_in`.

## Acceptance

- [ ] **Failing-first**: with a fixture `HOME` containing an agent-triggerable
      `~/.claude/commands/<name>.md`, a named test's verdict differs from the same test under an
      empty `HOME` at the merge base, and is identical under both afterwards. C-297 already recorded
      the sharp instance of this: a test asserting a name is *absent* inverts when the operator
      happens to have a command of that name.
- [ ] `detect_signals_in(cwd, &DiscoveryEnv)` exists beside `detect_signals(cwd)`, which delegates
      with `DiscoveryEnv::from_process()`. No third injection idiom.
- [ ] The 30 tests pin an env. The re-derived breakdown (C-332, 2026-08-01):
      | crate / file | tests | reaches it via |
      |---|---|---|
      | `flux-flow/src/engine.rs` | 26 | `surfaced_op_names` on every turn |
      | `flux-runtime/src/lib.rs` | 3 | `detect_signals` directly (`:7031`, `:7066`, `:7318`) |
      | `flux-app/src/app.rs` | 1 | `detect_signals` (`:3041`) |
      The 26 `flux-flow` figure is the count of engine tests that drive a turn, so a single pinned
      construction in the shared engine-building fixture may cover most of them — confirm rather
      than assume, and report the number actually touched.
- [ ] ⚠ `surfaced_op_names` is `pub(crate)` but `detect_signals` is **public** and re-exported
      through the `flux-flow` facade. Threading a parameter reaches `flux-app`, `flux-flow` and
      `flux-cli`, so the new entry point must be additive — the existing signature keeps working.
- [ ] Full gate green in both workspaces.

## Notes

- Parent census: [C-332](C-332-home-reading-tests-need-an-injection-seam.md). C-297 estimated 7 for
  this tranche; the re-derived count is **30**, because the estimate counted only the direct
  `detect_signals` callers and missed that every `flux-flow` engine turn goes through
  `surfaced_op_names`.
- The seam to reuse: `DiscoveryEnv` (`crates/flux-runtime/src/metadata.rs`, C-297) and the
  `*_in(cwd, env)` pairs C-332 added beside `load_config` / `config_layers` / `load_groups`.
- `crates/flux-tools/src/groups.rs:34` and `command_invoke.rs:10,34,143` document the invariant that
  the `agent_triggerable` gate and `detect_signals` can never disagree about what is triggerable —
  keep that true, and check whether `command_invoke`'s own tests need the same pin.
- The general guard for this class is C-333 (a codegate lint banning ambient reads in test code).
