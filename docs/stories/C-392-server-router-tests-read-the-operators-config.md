---
id: C-392
title: "43 `flux-server` tests build a router that reads the operator's `~/.flux/config.toml`"
pillar: Core
epic: road-to-stable
status: done
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

- [x] **Failing-first**: with a fixture `HOME` whose `~/.flux/config.toml` sets
      `[server] a2a_session_ttl_secs` (or `requests_per_minute`), a named `flux-server` test fails at
      the merge base and passes with an empty `HOME`; after the change its verdict is identical under
      both. Build the test binary in your own target dir — a shared `CARGO_TARGET_DIR` re-runs a
      stale binary and the proof is worthless.
- [x] The seam is the **same idiom**, not a third one: an additive `router_in(engine, auth, card,
      bind, &DiscoveryEnv)` / `router_multi_in(..)` alongside the existing entry points, which
      delegate with `DiscoveryEnv::from_process()`. `router`/`router_multi` keep their signatures —
      `codewandler-flux-server` is published, and this must stay additive.
- [x] All 43 tests pin an env — **the measured figure is 45 declared / 44 executing**; see Progress.
      The re-derived breakdown (C-332, 2026-08-01):
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
- [x] ⚠ **Do not weaken the safety invariant while threading the parameter.** `router()` refuses
      `ServerAuth::Open` on a non-loopback bind at construction (`guard_open_bind`, `:862`) — that
      refusal is why it returns a `Result`. `router_in` must refuse identically, and
      `empty_shared_secret_bind.rs`'s two tests are the ones that prove it.
- [x] Full gate green in both workspaces.

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

## Progress

- **The seam** (`crates/flux-server/src/lib.rs`): four additive entry points, one idiom, the same
  value-held env as C-297/C-213/C-332 — `pub fn router_in(engine, auth, card, bind, &DiscoveryEnv)`
  and `pub fn router_multi_in(resolver, auth, bind, &DiscoveryEnv)`, with the private
  `router_with_ttl_in` / `router_multi_with_ttl_in` and `ServerLimits::from_env_in` /
  `a2a_ttl_from_config_in` beneath them. `router`/`router_multi` keep their signatures and now
  **delegate** with `DiscoveryEnv::from_process()`; nothing about production behaviour changed and
  `codewandler-flux-server`'s API is purely additive. `DiscoveryEnv` is re-exported from
  `flux_server` so a consumer of `router_in` need not name `flux-runtime`.
- **The safety invariant did not move.** `guard_open_bind` runs in `router_in`/`router_multi_in`,
  and `router`/`router_multi` reach it *by delegation* — so there is still exactly one enforcement
  point, and the pair cannot diverge. `empty_shared_secret_bind.rs`'s two tests drive `router_in`
  and both still pass; `unauthenticated_non_loopback_router_is_refused_at_construction` likewise.
- **Failing-first, at the merge base** (`87ec76e2`, own `CARGO_TARGET_DIR`): with a fixture `HOME`
  whose `~/.flux/config.toml` sets `[server] requests_per_minute = 1` (+ `a2a_session_ttl_secs`,
  `max_inflight_per_principal`), `a2a_conformance::task_history_is_populated_and_bounded` fails —
  `response body was not valid JSON … body: "resource limit exceeded"` — and passes with an empty
  `HOME`. After the change its verdict is identical under both.
- **Whole-suite verdict census, mechanically.** All 88 `flux-server` tests were run under both homes
  before and after. At the merge base **17** flipped `ok` → `FAILED` under the fixture home; after
  the change the two runs are **byte-identical, 0 failures**. (17 < 45 because a test can *read* the
  operator's config without this particular fixture value changing its outcome — the hazard is the
  read, the flip is one draw from it.)
- **The count, re-derived: 45 tests declared, 44 executing — not 43.** Two corrections to C-332's
  table, in opposite directions of confidence:
  - `src/lib.rs` (in-crate) is **3, not 2**. The missed one is
    `a2a_ttl_prunes_only_expired_a2a_sessions`, which injects an explicit `A2aTtl` through the
    white-box `router_with_ttl` seam and *looks* pinned — but that seam still called
    `ServerLimits::from_env()`, so it read the operator's config for its **limits** while pinning
    only its TTL. A census that follows the TTL knob stops one call short of it.
  - `tests/malformed_json_rpc.rs` is **4 declared, 3 executing**. The fourth,
    `garbage_body_yields_a_json_rpc_parse_error_envelope`, is `#[ignore]`d (a known C-41 gap) and so
    never runs — but it builds a router in source and is migrated with the rest.
  - Every other row matched exactly. The **call-site** figure was also low: 27 construction sites
    (26 `router`/`router_multi` + 1 `router_with_ttl`), not 21.
- **Proved by a check, not by inspection**: `crates/flux-server/tests/router_env_is_pinned.rs`
  scans this crate's whole test corpus (every `tests/**.rs` plus each `src/` module's inline
  `mod tests` tail) and fails on any `router(`/`router_multi(`/`router_with_ttl(`/`from_env(` call,
  naming file and line. It was verified to fire by reintroducing one ambient call, and it carries a
  unit test of its own scanner plus two vacuity floors (corpus size, pinned-site count) so it cannot
  pass by measuring nothing. C-333 should replace it with the syntax-aware workspace-wide form.
- ⚠ **The `flux-config` tilde-expansion seam is NOT folded in, and the `HOME_LOCK` trap C-332 left
  stands.** The router resolves `[server]` scalars out of the merged `Config`; it never reaches
  `skill_dir_paths()` / `workspace_add_dirs()` / `skill_dirs_with_origin()` / `sandbox_writable()`,
  so this story does not touch that call chain and the three `flux-config` tests that hold
  `HOME_LOCK` still genuinely need it. It remains the standalone follow-up C-332's Notes describe.
