---
id: C-393
title: "`detect_signals` has no injection seam, so 11 tests probe the operator's home"
pillar: Core
epic: road-to-stable
status: in-progress
priority: 7
areas: [flux-runtime, flux-flow, flux-app, flux-agent, flux-sdk, flux-cli, flux-tools, flux-codegate]
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

- [x] **Failing-first**: with a fixture `HOME` containing an agent-triggerable
      `~/.claude/commands/<name>.md`, a named test's verdict differs from the same test under an
      empty `HOME` at the merge base, and is identical under both afterwards. C-297 already recorded
      the sharp instance of this: a test asserting a name is *absent* inverts when the operator
      happens to have a command of that name.
      → `flux-cli`'s `execution::command_file_tests::no_command_dirs_yields_an_empty_list`
      (`crates/flux-cli/src/execution.rs:596`). At merge base `ad29900b` it FAILS under the fixture
      home and passes under an empty one; after the change it passes under both, and under a
      *hostile* home (every user-global command/skill root populated with agent-triggerable
      `ghost`/`greet`/`deploy`/`missing`/`automatic` entries plus `~/.kube/config`) the whole
      workspace is green.
- [x] `detect_signals_in(cwd, &DiscoveryEnv)` exists beside `detect_signals(cwd)`, which delegates
      with `DiscoveryEnv::from_process()`. No third injection idiom. `kubeconfig_present` and
      `agent_triggerable_target_present` both take the env — closing only the discovery half leaves
      `~/.kube/config` ambient.
      → `crates/flux-runtime/src/lib.rs:2081` (`detect_signals` delegating) and `:2100`
      (`detect_signals_in`); `agent_triggerable_target_present(cwd, env)` at `:2178` and
      `kubeconfig_present(env)` at `:2202`.
- [x] The 11 tests pin an env. The re-derived breakdown (C-332, 2026-08-01, verified per call site):
      | crate / file | tests | reaches it via |
      |---|---|---|
      | `flux-flow/src/engine.rs` | 4 | `surfaced_op_names`, **only when `groups` is non-empty** |
      | `flux-runtime/src/lib.rs` | 3 | `detect_signals` directly (`:7031`, `:7066`, `:7318`) |
      | `flux-agent/src/lib.rs` | 2 | `discover_skills` via the spec builders (`:255`, `:274`) |
      | `flux-app/src/app.rs` | 1 | `detect_signals` (`:3041`) + `discover_skills` (`:3062`) |
      | `flux-sdk/src/lib.rs` | 1 | `try_with_default_skills` (`:1249`) |
- [x] ⚠ The four `flux-flow` tests are named, because they are *not* discoverable by grepping for
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
      → the four pin `pinned_env()` (`crates/flux-flow/src/engine.rs:2271`); the short-circuit's own
      test additionally now asserts `surfaced.is_none()`, so the guard the census hinges on
      is itself pinned rather than assumed (`:3648`).
- [x] ⚠ `surfaced_op_names` is `pub(crate)` but `detect_signals` is **public** and re-exported
      through the `flux-flow` facade. Threading a parameter reaches `flux-app`, `flux-flow` and
      `flux-cli`, so the new entry point must be additive — the existing signature keeps working.
      This is the reason the story is worth doing at 11 tests: the tranche is small, the API
      obligation is not.
      → every new entry point is additive: `detect_signals_in`, `AgentSpec::try_with_default_skills_in`,
      `AgentSpec::try_with_model_invoked_skills_in`, `FlowEngine::with_discovery_env`. No published
      signature changed; `cargo build --workspace` needed no call-site edits outside the pinned tests.
- [x] Full gate green in both workspaces.

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
  **Checked — they did**, and the check was not academic: one of them inverted outright under a
  fixture home. See Progress.
- The general guard for this class is C-333 (a codegate lint banning ambient reads in test code).

## Progress

**2026-08-01 — implemented.** Branch `impl/C-393-clean`, based on `origin/main` at `ad29900b`.

The seam, exactly the C-332/C-392/C-391 shape (a value, never a `set_var`):

- `flux_runtime::detect_signals_in(cwd, &DiscoveryEnv)` beside `detect_signals(cwd)`; **both**
  home-rooted checks take the env, so `~/.kube/config` is closed along with discovery.
- `AgentSpec::try_with_default_skills_in` / `try_with_model_invoked_skills_in` (flux-agent).
- `FlowEngine::with_discovery_env` + a private `discovery_env` field threaded into
  `surfaced_op_names`, whose `env` parameter is **required** rather than defaulted — the tests that
  short-circuit pay nothing, and the four that reach the probe must state their fixture.
- `execution::load_skills_in` / `load_model_invoked_skill_catalog_in` / `load_command_files_in`
  (flux-cli, `pub(super)`).
- `flux-runtime`'s `metadata::HOME_LOCK` is **gone** — nothing in the crate repoints `HOME` any more.

**The census (`flux-codegate`).** `no_test_resolves_the_workspace_probe_from_the_operators_home`
walks every source in both workspaces and fails on any test-code call to a process-reading
discovery entry point. It is `syn`-structural, so prose and string literals are free, and `*_in`
is a different identifier rather than a substring exclusion. It additionally scans **macro token
streams** — without that it reports zero over a corpus whose dominant shape is
`assert!(load_command_files(..).is_empty())`. Anti-vacuity floors on both the file count and a
tally of *pinned* references. Verified to fire, twice: reverting `flux-sdk/src/lib.rs:1249` to
`try_with_default_skills()` and reverting the macro-embedded `flux-cli/src/execution.rs:598` call
each reds it with the exact `file:line`.

**One gap the story's own Notes predicted, found by running the suite under a fixture home.**
`flux-tools`' `command_invoke::tests::inaccessible_target_is_refused` asserts "no command named
`ghost` is discovered" — and passed only because the developer's `~/.claude/commands/ghost.md`
does not exist. Under the fixture home it returned the *user-global* command's body instead of
refusing. Worse, leaving it ambient would have made the invariant this module's header states
(`command_invoke.rs:8`) newly *falsifiable*: with `FlowEngine::with_discovery_env` a caller can now
pin the probe that surfaces `command.invoke` while the accessible gate still read the process
`HOME`, so the signal and the gate could disagree. `CommandInvokeTool` therefore carries a
`DiscoveryEnv` field, built `from_process()` at registration (production unchanged) and pinned by
the suite. Pinned in both directions by
`the_accessible_gate_resolves_the_pinned_home_and_never_the_process_home`.

**Verification beyond the gate.** The whole workspace is green under a *hostile* `HOME` in which
every user-global root flux consults (`~/.flux/commands`, `~/.claude/commands`, `~/.flux/skills`,
`~/.agents/skills`, `~/.claude/skills`) holds agent-triggerable `ghost`/`greet`/`deploy`/`missing`/
`automatic`/`pdf-extract`/`l02-cli-layering`/`flux-plugin` entries, plus `~/.kube/config`. At the
merge base that same home reds `no_command_dirs_yields_an_empty_list` and
`inaccessible_target_is_refused`.

**Known scanner blind spot, deliberately not closed.** The census sees *direct* calls from test
code. `command_invoke`'s gate was reached indirectly, through `Executor::dispatch` → the tool's
`execute`, and no name-frontier scanner can catch that without flagging every test that registers
builtins. C-333 (the general ambient-read lint) is where that belongs.
