---
id: C-297
title: "A skill-discovery test reads the operator's real `~/.claude/skills`, so a concurrent session reds the gate"
pillar: Core
status: done
priority: 6
areas: [flux-runtime]
note: "found by C-213 and hit by the coordinator on the very next merge — `discover_skills` walks the machine's real home, so any agent session writing there fails an unrelated test and the log looks like a code regression"
---

# A skill-discovery test reads the operator's real `~/.claude/skills`

## Goal

`crates/flux-runtime/src/metadata.rs`'s
`skill_directory_with_no_frontmatter_name_takes_directory_name` calls `discover_skills`, which reads
the machine's **real** `~/.flux/skills`, `~/.agents/skills` and `~/.claude/skills` rather than a
pinned fixture root. On a machine where those directories are live — a developer box, and especially
one running several agent sessions — the test's result depends on what another process happened to
write a second earlier.

It is a gate flake with a bad failure signature: the run aborts, the log truncates, and it reads as a
compile or infrastructure failure in whatever diff is being merged.

## Acceptance

- [x] The test resolves its skill roots from an injected/overridden home, not from the process's real
      one. `HarnessEnv` in `flux-capabilities` (C-213) is the shape that already exists in this tree
      for exactly this problem — an injected environment rather than per-site `std::env` reads — and
      is worth copying rather than reinventing.
- [x] A failing-first demonstration: with a stray directory planted in the real `~/.claude/skills`,
      the test fails at the merge base and passes after. That is reproducible without waiting for a
      concurrent session to collide.
- [x] Any *other* test in the workspace that reads a real home directory is enumerated in this story.
      Fixing one instance of a class and leaving its siblings is how this recurs. `discover_skills`
      is the known one; say plainly whether the scan found others and how you scanned.
- [x] Full gate green, twice in a row, while something concurrently writes to `~/.claude/skills`.

## Notes

- **Provenance, and why it is filed rather than shrugged off.** C-213's implementor hit it once in
  ten full-workspace runs, investigated properly rather than re-running until green, and proved it
  pre-existing: the test lives in a crate its diff never touched, and
  `cargo tree -p codewandler-flux-runtime -e normal,dev` contains neither `flux-capabilities` nor
  `flux-cli`, so its code was not in that binary's closure at all. It then failed to reproduce in six
  runs at the merge base.
- **The coordinator then hit it immediately**, on the very next merge, while a review agent was
  running concurrently: `cargo test --workspace` exited 101 having run 46 tests, with a truncated log
  and no error line. A clean re-run gave 3392 passed / 0 failed. Two independent hits in one session
  is not a curiosity; it is a tax on every merge in a multi-session repo.
- ⚠ This is the same family as the `/tmp/.git` sticky-test flake — a test whose result depends on
  machine state outside the repository. (That one is an operator-machine note, not a repo artifact;
  this repo has no root `CLAUDE.md`, so the link that used to point at one is gone — C-332.) The
  general rule worth stating in the fix: a unit test may not read the operator's home.
- Related: [C-213](C-213-extract-harness-adapters.md) found it and named the fix shape.

## The scan (Acceptance 3) — plainly: yes, there are many others

**How I scanned.** Three passes over `crates/**`, then a read of every hit:

```bash
# 1. production readers of the real home
grep -rnE 'var_os\("HOME"\)|var\("HOME"\)|var_os\("USERPROFILE"\)|dirs::home_dir|home::home_dir|
           directories::|BaseDirs|UserDirs|CLAUDE_CONFIG_DIR|CODEX_HOME|FLUX_HOME|XDG_' --include='*.rs' crates/
# 2. which tests override HOME / take a lock
grep -rnE 'set_var\("HOME"|remove_var\("HOME"|HOME_LOCK|ENV_LOCK|serial_test' --include='*.rs' crates/
# 3. test callers of each reader, incl. the transitive ones
grep -rn 'discover_skills|discover_commands|load_config|load_groups|config_layers|detect_signals|
          surfaced_op_names|allocate_worktree_dir|router\(|router_multi\(' --include='*.rs' crates/
```

A test is a hazard when it reaches one of those readers with the process's real `HOME` intact.
**73 hazardous tests across 11 crates**, not one. `flux-capabilities` is the only crate that is clean
*by construction* — `HarnessEnv` is a value, and all six of its tests build
`HarnessEnv::empty().with("HOME", <temp>)`. That is the shape this story copies.

**Fixed here** (all of `flux-runtime`'s skill/command discovery — the class the Goal names):
`metadata.rs` `skill_directory_with_no_frontmatter_name_takes_directory_name` (the reported one),
`missing_optional_metadata_is_harmless_on_every_platform`,
`command_frontmatter_parses_known_fields_and_warns_on_unknown_ones`,
`command_agent_triggerable_flag_parses_silently_and_defaults_false`, plus the three symlink-escape
tests, and the seven that were *correct* but got there by mutating process-global `HOME`.

**Found and deliberately NOT fixed** — each needs a different injection point, and the two biggest
sit behind public signatures whose callers are in crates fenced off from this story (`flux-cli`,
`flux-app`, `flux-tui`). Filing rather than half-fixing:

| crate / file | count | reader | why not here |
|---|---|---|---|
| `flux-server/tests/*` (`principal_auth`, `a2a_conformance`, `a2a_context_continuity`, `a2a_message_stream`, `a2a_ttl_pruning`) | 25 | `router()`/`router_multi()` → `a2a_ttl_from_config()` + `ServerLimits::from_env()` → `load_config(current_dir())` (`flux-server/src/lib.rs:809,674`) | an operator `~/.flux/config.toml` with `[server] a2a_session_ttl_secs` / `requests_per_minute` / `max_inflight_per_principal` changes what all 25 assert against |
| `flux-config/src/lib.rs` | 10 | the `#[cfg(test)]` `load()` helper (`:1424`) merges the real `~/.flux/config.toml` under every fixture | exact-vector asserts (`allow == [...]`, `grants.len() == 1`); user layer *concatenates* |
| `flux-tools/src/lib.rs` | 6 | `allocate_worktree_dir` → **writes** `$HOME/.flux/worktrees` when `FLUX_WORKTREE_DIR` is unset | these create real state in the operator's home |
| `flux-tools/src/command_invoke.rs` | 5 | `invoke_command`/`invoke_skill` → `discover_commands`/`discover_skills` | `inaccessible_target_is_refused` asserts `"ghost"` is *absent* — a real `~/.claude/commands/ghost.md` inverts it |
| `flux-runtime/src/metadata.rs` (config half) | 5 | `trusted_flux_root()` (`:28`) → `load_config`/`load_groups`/`config_layers` read `~/.flux/{config,groups}.toml` | `load_config` is public and called from fenced crates; needs its own `load_config_in` |
| `flux-flow/src/engine.rs` | 4 | `surfaced_op_names` → `detect_signals` | same `detect_signals` injection point as below |
| `flux-system` | 4 | `worktree_base_dir`, `sandbox::home_dir`; two `live_smoke_*` **write** into the real home (gated by `FLUX_LIVE_SANDBOX_SMOKE`) | two are tautological (assert against the var they set) |
| `flux-runtime/src/lib.rs` | 2 | `detect_signals` → discovery + `~/.kube/config` | `detect_signals` is public; threading `DiscoveryEnv` through it reaches `flux-app`/`flux-flow`/`flux-cli` |
| `flux-agent/src/lib.rs` | 2 | `with_default_skills`/`try_with_model_invoked_skills` → `discover_skills` | one asserts a skill is *absent* |
| `flux-cli` | 3 | `plugin_cmd.rs:2009`; `tests/core_catalog.rs:14` and `tests/website_contract.rs:153` spawn the binary without pinning `HOME` | fenced (C-296, C-214) |
| `flux-app/src/app.rs` | 1 | `detect_signals` | fenced (shares the `detect_signals` seam) |
| `flux-plugin/src/pack.rs` | 1 | `generate_pack_keypair` **writes** `$HOME/.flux/minisign-pack.key` | `#[ignore]`d by default |

Also worth recording: **lock-less `HOME` mutators** are what turn the above from "environment-dependent"
into "racy" — `flux-cli/src/main.rs:1791` sets `HOME` and never restores it;
`flux-plugin/src/host.rs:3078` and `flux-providers/src/bedrock.rs:2020` `remove_var("HOME")`, as did
every correct test in `metadata.rs` before this change. That is why the reported test failed twice in
one day and then "failed to reproduce in six runs": whether it reads the real home at all depends on
whether it wins a scheduling race against a test that clears `HOME` process-wide.

**Recommended follow-up:** one story per injection point — `load_config_in(cwd, env)` (covers
`flux-server`'s 25 + `flux-config`'s 10 + `metadata.rs`'s 5), `detect_signals_in(cwd, env)` (covers
`flux-flow` 4 + `flux-runtime` 2 + `flux-app` 1), and a `FLUX_WORKTREE_DIR` pin in `flux-tools`'
worktree tests (6). Those three would clear 53 of the 73.

## Progress

- `DiscoveryEnv` added to `crates/flux-runtime/src/metadata.rs` — a value-held `HOME`, mirroring
  `flux-capabilities`' `HarnessEnv`. `discover_skills_in` / `discover_commands_in` take it; the
  existing `discover_skills` / `discover_skills_from` / `discover_commands` keep their signatures and
  delegate with `DiscoveryEnv::from_process()`, so no caller outside this crate changed.
- The user-global root list now lives in exactly one place each (`DiscoveryEnv::skill_roots` /
  `command_roots`) instead of being spelled inline at the `std::env` read sites.
- All 14 discovery tests in `metadata.rs` now pin an env; **no discovery test mutates process `HOME`
  any more**. `HOME_LOCK` survives for the two config tests that genuinely still repoint `HOME`.
- ⚠ One trap for a resuming agent: removing `HOME_LOCK` from the discovery tests un-serialized
  `namespaced_duplicate_skill_names_dedup_first_wins_and_warn` from
  `discover_skills_across_five_dirs_with_precedence_and_dedup`, and they race on `tracing`'s
  *process-global callsite-interest cache* (the recording subscriber then captures nothing —
  reproduced 1-in-6). Fixed with a `TRACING_CALLSITE_LOCK` named for what it protects, plus a
  deterministic assertion on the returned `SkillDiscovery::warnings`. 20/20 green after.
