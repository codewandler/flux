---
id: C-162
title: "[tools] disable — a plain blocklist for turning ops off"
pillar: Core
status: done
priority:
epic:
design:
note: "tool groups (flux-evidence lib.rs:185-193, .flux/groups.toml at flux-config lib.rs:1028) only ever ADD surface when evidence fires — there is no subtractive knob, so 'I never want browser.* in this repo' means hand-writing authorization policy for a prompt-size/attack-surface concern that isn't an authorization question"
---

# `[tools] disable` — a plain blocklist for turning ops off

## Goal
Give operators one obvious way to say *"this repo never uses these ops"* — `[tools] disable =
["browser.*", "web.*"]` in `.flux/config.toml`. Today the only subtractive control is the
authorization policy, which is the right instrument for *authority* and the wrong one for *surface*:
an op the agent should never see still costs prompt tokens, still invites the model to try it, and
still widens the prompt-injection target. Tool groups (`flux-evidence/src/lib.rs:185-193`) are
purely additive — they surface ops when evidence fires, never hide them.

## Acceptance
- [ ] `[tools] disable = [...]` in `.flux/config.toml` accepts exact op names and `family.*` globs;
      a disabled op is absent from the surfaced tool set — failing-first test asserting it never
      reaches the model.
- [ ] Disabling is **surface-only and defense-in-depth, not a security boundary** — a disabled op
      is also refused at dispatch (so a cached plan or a resumed session can't call it), and the
      docs say plainly that the authorization policy remains the security control. Test covers the
      dispatch refusal path.
- [ ] Layering follows the existing config precedence (user → project), and an entry matching no
      known op is a startup warning naming the entry, not a silent no-op.
- [ ] `flux` surfaces what was disabled somewhere discoverable (the natural home is the C-128
      `flux doctor` diagnostics, if that lands first) so a mysteriously-missing op is one command
      from an explanation.
- [ ] Disabling is stable across a turn, so it cannot churn the prompt prefix mid-session (the A-95
      lesson).

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass)

### 2026-07-28 — implementation

**Design.** `[tools] disable` lives in a new `flux_config::ToolsConfig` (`disable: Vec<String>`),
layered like the permission lists (user entries first, then project, concatenated + deduped —
`crates/flux-config/src/lib.rs`). A pure `flux_config::tool_disable_matches(pattern, op_name)`
decides membership: an exact name, or — only when the pattern ends `.*` — every op under that
dotted family (`"browser.*"` matches `browser.navigate`; a bare `"browser"` is an exact-name match
only, never an implicit glob).

Resolution against the *live* registry (so a pattern with no matching op is detectable) is
`flux_runtime::ToolRegistry::resolve_disabled(patterns) -> ResolvedDisabledOps { disabled,
unmatched }`, called once in `flux-cli`'s `build_agent_with` right after the registry is fully
assembled and before any turn runs — so the resolved set is fixed for the life of the session/turn
(the A-95 stability lesson: no per-turn recomputation, so the prompt prefix cannot churn).

- **Surfacing (never reaches the model).** `Executor::disabled_ops()` exposes the resolved set;
  `flux_flow::engine::surfaced_op_names` (the one function both the model's tool catalog and the
  model-stage catalogs read from) subtracts it from the advertised set, in BOTH the gated and
  ungated branches — proven to win even over a force-on/active tool group (tool groups are
  additive-only; disable is the one subtractive knob, and it takes precedence).
- **Dispatch defense-in-depth.** `Executor::gate()` (the one gate shared by `authorize()` and
  `dispatch_outcome()`) checks `disabled_ops` first, unconditionally, before the capability-scope
  floor/hooks/policy/permissions, and refuses with `` `{name}` disabled by config ([tools] disable)
  ``. The op stays fully *registered* (dispatch must recognize the name to give this specific
  refusal rather than "unknown tool"), so a cached plan or resumed session naming it is refused
  the same way. Docs (Rust doc-comments + the website page) say plainly that the authorization
  policy remains the actual security control and wins if the two disagree — this is not a second
  permission system.
- **Discoverability (C-128 `flux doctor` hasn't landed).** Three existing surfaces cover it: (1)
  an unmatched pattern prints a startup warning naming the entry (`(warning: [tools] disable entry
  \`X\` matches no known op)`); (2) a `tools.disabled` Startup evidence observation lists what's
  disabled (visible via the REPL's `/evidence`); (3) the REPL's existing `/tools` command now
  marks each disabled op inline (`name (disabled by config)`) instead of hiding it.

**Files touched.**
- `crates/flux-config/src/lib.rs` — `ToolsConfig`, `tool_disable_matches`, `Config.tools` field,
  `merge()` wiring, TOML deny-unknown-fields coverage.
- `crates/flux-runtime/src/lib.rs` — `ResolvedDisabledOps`, `ToolRegistry::resolve_disabled`,
  `Executor.disabled_ops` field + `with_disabled_ops`/`disabled_ops()`, the `gate()` refusal,
  `DispatchOutcome::denied` doc update.
- `crates/flux-flow/src/engine.rs` — `surfaced_op_names` gained a `disabled` parameter (internal
  `pub(crate)` fn; all call sites, including tests, updated); `surfaced_for_turn` passes
  `self.executor.disabled_ops()`.
- `crates/flux-cli/src/execution.rs` — resolves + installs the disabled set on the executor in
  `build_agent_with`; prints the unmatched-pattern warning; records the `tools.disabled`
  observation.
- `crates/flux-cli/src/session.rs` — `/tools` marks disabled ops.
- `crates/flux-cli/tests/mock_smoke.rs` — new end-to-end test.
- `website/docs/reference/config.md` — new "Tool surface (`[tools] disable`)" section + example.

**New tests** (all failing-first verified: temporarily reverted the fix in place, confirmed the
test failed for the intended reason, restored):
- `flux-config`: `tool_disable_matches_exact_names_and_family_globs`,
  `tools_disable_layers_user_and_project_with_precedence_and_dedup`,
  `tools_disable_parses_from_toml`.
- `flux-runtime`: `resolve_disabled_matches_exact_names_and_family_globs_and_reports_unmatched`,
  `resolve_disabled_family_glob_matches_every_op_in_the_family`,
  `disabled_op_is_refused_at_dispatch_even_though_still_registered_and_allowed` (failing-first
  verified).
- `flux-flow`: `disabled_ops_never_reach_the_surfaced_set_with_no_groups`,
  `disabled_ops_win_over_an_active_force_on_group` (both failing-first verified).
- `flux-cli`: `tools_disable_unmatched_entry_warns_at_startup` (real binary, real
  `.flux/config.toml`; failing-first verified).

**Gate (crate-scoped, run individually — see final report for exact commands/output):**
`codewandler-flux-config`, `codewandler-flux-runtime`, `codewandler-flux-flow` (added
`#[allow(clippy::too_many_arguments)]` on `surfaced_op_names`, matching the file's existing
convention for similarly-shaped functions), and `flux-cli` (build/test/clippy all green;
`website_contract` doc-sync test green with the new `[tools]` example block). `cargo fmt --check`
is clean for every file this story touched; one pre-existing formatting violation remains in
`crates/flux-cli/src/execution.rs` from a concurrently-edited, not-yet-formatted line belonging to
the A-96 (`consult` op) story — left untouched per the no-cross-story-reformatting rule.

**Public API.** Additive only: new public `flux_config::ToolsConfig`/`tool_disable_matches`, new
public `flux_runtime::ResolvedDisabledOps`/`ToolRegistry::resolve_disabled`/
`Executor::with_disabled_ops`/`Executor::disabled_ops`. No existing signature changed except the
crate-internal (`pub(crate)`) `flux_flow::engine::surfaced_op_names`, which is not part of the
published API surface. No `#[non_exhaustive]` needed (all new types are either plain data structs
callers construct via `Default`/fields, not enums expected to grow variants).

**Scope note.** Only the primary interactive agent path (`flux run` / REPL / TUI, all funneling
through `flux-cli::execution::build_agent_with`) wires `[tools] disable`. Two other executor-
assembly points were left untouched as likely follow-up stories: `flux-server`'s HTTP/A2A surface
and `flux app run`'s multi-agent program host (`flux-cli::app_cmd`) — both build their own
`ExecutionEnvironment`/registry and do not currently read `cfg.tools.disable`.

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's `amp.tools.disable`, a glob-supporting
  tool-name blocklist.
- Evidence the gap is real: `crates/flux-evidence/src/lib.rs:185-193` (`ToolGroup` — `tools` +
  `surface_when`, additive only) and `crates/flux-config/src/lib.rs:1028` (the `.flux/groups.toml`
  manifest that carries them).
- Small by design. The value is that it is *obvious*: today a user who wants less surface has to
  learn the policy language to express a preference that isn't about permission at all.
- Do not let this become a second permission system. If the two ever disagree, the policy wins and
  the docs must say so.
