---
id: D-22
title: One guarded process-spawn path + the plugin authoring guide
pillar: Core
status: done
note: "funneled all OS-process creation through one `flux_system` `build_command` (+ new `spawn_interactive`); `PluginHost::spawn` routed through it so the **plugin process is env-cleared** — a plugin can no longer read the host's secrets via `std::env` (gated `secret` is the only path), closing a bypass of the deny-by-default model; flux-runtime git-context also via `System::run`; new `plugins/AUTHORING.md` (linked from AGENTS.md + README); env-isolation regression test; full root gate green"
---

# One guarded process-spawn path + the plugin authoring guide

## Goal
Make flux's "deny-by-default, manifest-scoped plugin capabilities; no bypass paths" promise **true**, and
document how plugins work where agents will find it. The plugin process was launched by a raw
`tokio::process::Command` in `flux-plugin` with **no `env_clear`**, so it inherited flux's full
environment — a plugin could read any secret with `std::env::var`, side-stepping the gated
`host.secret(purpose)` path. Per the mandate *there must be exactly one path that starts an OS process,
launching a plugin must go through it, and the safety lives in that one path.* Surfaced while writing
the authoring guide requested under the fluxplane-plugins parity epic (prereq for D-15/16/17).

## Acceptance
- [x] **One spawn constructor.** `flux-system` builds every OS process through a single
      `build_command(argv, env)` (argv-only, workspace-pinned cwd, `apply_safe_env` clear+allow-list) —
      `run_with_env`, `run_with_env_streamed`, `spawn_background`, and the new `spawn_interactive` all
      layer only their stdio on top. One `Command::new`, one envelope site.
- [x] **Plugins launch through it.** `PluginHost::spawn` delegates to `System::spawn_interactive`
      (piped stdin+stdout, inherited stderr, `kill_on_drop`); a `&System` is threaded from
      `load_plugin_tools` + the `flux plugin call`/`skill` CLI paths. The plugin process is env-cleared.
- [x] **No second production spawn path.** Audited all `Command::new` sites: `flux-runtime`'s
      git-context call routed through `System::run`; `flux-tools` `.exec()` (re-exec of the flux binary,
      process *replacement*) and `flux-eval`'s `#[cfg(test)]` git helper left as-is (documented).
- [x] **Invariant test.** `crates/flux-plugin/tests/host.rs::plugin_cannot_read_host_env`: a host-set
      non-allow-listed var is **not** visible to a spawned plugin (`readenv` probe → null), while an
      allow-listed var (`PATH`) is — proving isolation, not a broken probe.
- [x] **Authoring guide.** `plugins/AUTHORING.md` (lifecycle, the host-does-all-IO invariant, the full
      capability set, the rules), linked from `AGENTS.md` ("Where to make a change" + the safety
      invariant on env-cleared subprocesses) and `plugins/README.md` (+ refreshed its stale capability
      list and jira/confluence/loki auth columns).
- [x] **Gate green:** full root workspace — `cargo test --workspace`, `clippy -D warnings`, `fmt`,
      `flux-codegate` layering. No new crate, no new cross-layer edge (flux-plugin already deps
      flux-system).

## Progress
- **Done.** `flux-system` consolidated onto `build_command` + added `spawn_interactive`/`InteractiveChild`
  (RUST_LOG/RUST_BACKTRACE added to the safe allow-list); `PluginHost::spawn` + `load_plugin_tools` take
  a `&System`; the four CLI call sites + the integration-test fixtures updated; `flux-runtime`'s `git()`
  routed through `System::run` (gains a 10s timeout). `caps_plugin` gained a `readenv` isolation probe.
  Guide written and wired in.

## Notes
- The existing `flux-system` env-leak tests (`run_does_not_leak_parent_secrets`,
  `spawn_background_clears_parent_env_and_applies_overrides`) already cover `apply_safe_env`; D-22 adds
  the plugin-level end-to-end guard so nobody reverts `PluginHost::spawn` to a raw `Command`.
- Sibling safety story: [D-20](D-20-scoped-private-net-egress.md) scopes *network* egress; this scopes
  *process/env*. Prereq for the parity epic's remaining packs
  ([fluxplane-plugins-parity](../designs/fluxplane-plugins-parity.md), D-15/16/17).
