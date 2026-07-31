---
id: C-344
title: "43 `flux-server` tests build a router that reads the operator's `~/.flux/config.toml`"
pillar: Core
epic: road-to-stable
status: ready
priority: 4
areas: [flux-server, flux-runtime]
note: "C-332's census, tranche A-remainder. `router()`/`router_multi()` resolve their TTL and resource limits from `load_config(current_dir())`, so an operator `~/.flux/config.toml` with `[server] a2a_session_ttl_secs` / `requests_per_minute` / `max_inflight_per_principal` silently changes what 43 tests assert against. C-332 built `load_config_in`; this story threads it through the router"
---

# The server router resolves its limits from the developer's home

## Goal

`flux_server::router()` and `router_multi()` resolve two things at build time from the layered flux
config: the A2A session TTL (`a2a_ttl_from_config`, `crates/flux-server/src/lib.rs:866`) and the
resource limits (`ServerLimits::from_env`, `:731`). Both do
`std::env::current_dir()` → `flux_runtime::metadata::load_config(&cwd)`, and `load_config`'s **user
layer is `$HOME/.flux/config.toml`**.

So every `flux-server` test that builds a router inherits whatever the developer happens to have in
their own config. An operator who sets `[server] a2a_session_ttl_secs`, `requests_per_minute` or
`max_inflight_per_principal` changes what 43 tests assert against — and the resulting failure looks
exactly like a real regression in whatever diff is in flight.

[C-332](C-332-home-reading-tests-need-an-injection-seam.md) built the seam this needs
(`flux_runtime::metadata::load_config_in(cwd, &DiscoveryEnv)`) and closed the `flux-runtime` and
`flux-config` halves of the tranche. This is the remainder, and it is the largest single block of
`HOME`-reading tests in the workspace.

## Acceptance

- [ ] **Failing-first**: with a fixture `HOME` whose `~/.flux/config.toml` sets
      `[server] a2a_session_ttl_secs` (or `requests_per_minute`), a named `flux-server` test fails at
      the merge base and passes with an empty `HOME`; after the change its verdict is identical under
      both. Build the test binary in your own target dir — a shared `CARGO_TARGET_DIR` re-runs a
      stale binary and the proof is worthless.
- [ ] The seam is the **same idiom**, not a third one: an additive `router_in(engine, auth, card,
      bind, &DiscoveryEnv)` / `router_multi_in(..)` alongside the existing entry points, which
      delegate with `DiscoveryEnv::from_process()`. `router`/`router_multi` keep their signatures —
      `codewandler-flux-server` is published, and this must stay additive.
- [ ] All 43 tests pin an env. The re-derived breakdown (C-332, 2026-08-01):
      | file | tests |
      |---|---|
      | `tests/a2a_conformance.rs` | 11 |
      | `tests/principal_auth.rs` | 12 |
      | `tests/multi_agent_mount.rs` | 5 |
      | `tests/malformed_json_rpc.rs` | 3 |
      | `tests/a2a_ttl_pruning.rs` | 2 |
      | `tests/a2a_context_continuity.rs` | 2 |
      | `tests/a2a_message_send.rs` | 2 |
      | `tests/empty_shared_secret_bind.rs` | 2 |
      | `tests/a2a_message_stream.rs` | 1 |
      | `tests/discovery_card_auth_exempt.rs` | 1 |
      | `src/lib.rs` (in-crate) | 2 |
      21 of these are direct `router(..)`/`router_multi(..)` call sites; the rest reach one through a
      per-file helper.
- [ ] ⚠ **Do not weaken the safety invariant while threading the parameter.** `router()` refuses
      `ServerAuth::Open` on a non-loopback bind at construction (`guard_open_bind`, `:862`) — that
      refusal is why it returns a `Result`. `router_in` must refuse identically, and
      `empty_shared_secret_bind.rs`'s two tests are the ones that prove it.
- [ ] Full gate green in both workspaces.

## Notes

- Parent census: [C-332](C-332-home-reading-tests-need-an-injection-seam.md). C-297 estimated 25
  here; the re-derived count is **43**, because the suite has grown and because the estimate missed
  the tests that reach a router through a file-local helper.
- The seam to reuse: `flux_runtime::metadata::load_config_in` /
  `config_layers_in` / `load_groups_in` (`crates/flux-runtime/src/metadata.rs`, C-332) and
  `DiscoveryEnv` (same file, C-297). `flux-server` already depends on `flux-runtime`, so no layering
  change is needed.
- **A related but distinct seam, found while measuring this one and deliberately not filed
  separately:** `Config::skill_dir_paths()`, `workspace_add_dirs()`, `skill_dirs_with_origin()` and
  `sandbox_writable()` (`crates/flux-config/src/lib.rs:947,963,992,1028`) each expand a leading `~/`
  by reading process `HOME` in **production** code. C-332 pinned the config *layer* those tests read
  but left the expansion alone — `skill_dirs_merge_project_before_user_and_expand_tilde`,
  `workspace_add_dirs_merge_and_allow_all` and `sandbox_config_parses_and_merges_security_directional`
  still hold `HOME_LOCK` and `set_var("HOME", ..)` for that reason alone. Fold it in here if the
  router work touches the same call chain; otherwise it is a small standalone follow-up.
- The general guard for this whole class is C-333 (a codegate lint banning ambient reads in test
  code); these 43 are the bulk of its initial waiver set.
