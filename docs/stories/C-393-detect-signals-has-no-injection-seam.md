---
id: C-393
title: "`detect_signals` has no injection seam, so 11 tests probe the operator's home"
pillar: Core
epic: road-to-stable
status: ready
priority: 7
areas: [flux-runtime, flux-flow, flux-app, flux-agent, flux-sdk]
note: "C-332's census, tranche B. `detect_signals(cwd)` reaches skill/command discovery and `~/.kube/config` with the process's real HOME. 11 tests reach it — small, but it is the last unfiled one of C-297's three injection points, and the only one whose reader is a *public* re-exported function"
---

# `detect_signals` probes the operator's home

## Goal

`flux_runtime::detect_signals(cwd)` (`crates/flux-runtime/src/lib.rs:2077`) is the per-turn workspace
probe that decides which evidence-gated tool groups surface. It takes a `cwd` and nothing else, yet
two of its checks read the process's real `HOME`:

- `kubeconfig_present()` (`:2168`) — `~/.kube/config`;
- `agent_triggerable_target_present(cwd)` (`:2150`) → `metadata::discover_commands` /
  `discover_skills`, which include the user-global discovery dirs (`~/.claude/commands`,
  `~/.flux/skills`).

So a test asserting a signal is *absent* can invert when the operator happens to have a kubeconfig,
or a command marked `agent-triggerable: true`. That is the last of the three injection points
[C-297] identified and never filed.

The same `DiscoveryEnv` that reaches `discover_commands`/`discover_skills` reaches both checks, so
one parameter closes the whole tranche.
[C-332](C-332-home-reading-tests-need-an-injection-seam.md) established the shape to copy —
`load_config_in(cwd, &DiscoveryEnv)` beside `load_config(cwd)`, exactly as C-297 did for
`discover_skills_in` / `discover_commands_in`.

**This is a small tranche — 11 tests — and it is ranked accordingly.** An earlier revision of
C-332's census put it at 30 and billed it as the widest blast radius. That was wrong; see Notes.

## Acceptance

- [ ] **Failing-first**: with a fixture `HOME` containing an agent-triggerable
      `~/.claude/commands/<name>.md`, a named test's verdict differs from the same test under an
      empty `HOME` at the merge base, and is identical under both afterwards. C-297 already recorded
      the sharp instance of this: a test asserting a name is *absent* inverts when the operator
      happens to have a command of that name.
- [ ] `detect_signals_in(cwd, &DiscoveryEnv)` exists beside `detect_signals(cwd)`, which delegates
      with `DiscoveryEnv::from_process()`. No third injection idiom. `kubeconfig_present` and
      `agent_triggerable_target_present` both take the env — closing only the discovery half leaves
      `~/.kube/config` ambient.
- [ ] The 11 tests pin an env. The re-derived breakdown (C-332, 2026-08-01, verified per call site):
      | crate / file | tests | reaches it via |
      |---|---|---|
      | `flux-flow/src/engine.rs` | 4 | `surfaced_op_names`, **only when `groups` is non-empty** |
      | `flux-runtime/src/lib.rs` | 3 | `detect_signals` directly (`:7031`, `:7066`, `:7318`) |
      | `flux-agent/src/lib.rs` | 2 | `discover_skills` via the spec builders (`:255`, `:274`) |
      | `flux-app/src/app.rs` | 1 | `detect_signals` (`:3041`) + `discover_skills` (`:3062`) |
      | `flux-sdk/src/lib.rs` | 1 | `try_with_default_skills` (`:1249`) |
- [ ] ⚠ The four `flux-flow` tests are named, because they are *not* discoverable by grepping for
      the reader — `surfaced_op_names` (`crates/flux-flow/src/engine.rs:1848`) returns at `:1868`
      before reaching `detect_signals` at `:1870` whenever `groups.is_empty()`, and 37 of the 41
      test fns in `engine.rs` pass exactly that:
      | test | line | how it supplies groups |
      |---|---|---|
      | `surfaced_groups_do_not_leak_between_sessions_on_a_shared_engine` | `:3566` | direct call, groups at `:3569` |
      | `disabled_ops_win_over_an_active_force_on_group` | `:3641` | direct call, groups at `:3644` |
      | `per_turn_surfacing_probe_follows_the_transitioned_root` | `:3874` | `assemble_with_loop` at `:3915` |
      | `one_raw_engine_serializes_turns_without_cross_wiring_sinks_or_audit` | `:4297` | `engine.groups =` at `:4303` |
      Do **not** pin a shared engine-building fixture to cover "most of them" — there is nothing to
      cover. `disabled_ops_never_reach_the_surfaced_set_with_no_groups` (`:3612`) calls
      `surfaced_op_names` directly with `&[]`; it is the short-circuit's own test and must keep
      exercising the ambient-free path. `crates/flux-flow/tests/` does not exist.
- [ ] ⚠ `surfaced_op_names` is `pub(crate)` but `detect_signals` is **public** and re-exported
      through the `flux-flow` facade. Threading a parameter reaches `flux-app`, `flux-flow` and
      `flux-cli`, so the new entry point must be additive — the existing signature keeps working.
      This is the reason the story is worth doing at 11 tests: the tranche is small, the API
      obligation is not.
- [ ] Full gate green in both workspaces.

## Notes

- Parent census: [C-332](C-332-home-reading-tests-need-an-injection-seam.md).
- ⚠ **A correction worth reading before trusting any count here.** An earlier revision of C-332
  sized this tranche at **30**, on the reasoning that `flux_flow::engine::surfaced_op_names` runs
  `detect_signals` on every turn and therefore every engine test is exposed. The call graph says
  that; the guard at `engine.rs:1858` does not. Reading the function body drops the `flux-flow` row
  from 26 to **4** and the tranche from 30 to **11** — which is, within one, exactly what C-297
  estimated (7) without any re-derivation at all. Two lessons, and the second is the load-bearing
  one: a census must execute reachability rather than infer it from call sites; and *a
  re-derivation that contradicts a prior estimate is not thereby more correct* — it needs its own
  evidence, which is why every row above now cites the line that proves it.
- C-297's estimate for this tranche stands. The tranche C-297 was genuinely wrong about is the
  `flux-server` router (25 estimated, 43 actual), filed separately.
- The seam to reuse: `DiscoveryEnv` (`crates/flux-runtime/src/metadata.rs`, C-297) and the
  `*_in(cwd, env)` pairs C-332 added beside `load_config` / `config_layers` / `load_groups`.
- `crates/flux-tools/src/groups.rs:34` and `command_invoke.rs:10,34,143` document the invariant that
  the `agent_triggerable` gate and `detect_signals` can never disagree about what is triggerable —
  keep that true, and check whether `command_invoke`'s own tests need the same pin.
- The general guard for this class is C-333 (a codegate lint banning ambient reads in test code).
