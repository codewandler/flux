---
id: L-72
title: flow_list/flow_run tools + unify flows and ops under ~/.flux/flows
pillar: Language
status: done
priority:
epic:
design: docs/designs/composite-ops.md
note: "flow_list/flow_run agent tools; ~/.flux/flows (@global_flows) is the unified home for reusable flows/ops (auto-load); fixed base_for bare-@name resolution"
---

# flow_list/flow_run tools + unify flows and ops under ~/.flux/flows

## Goal
Let the agent discover and run authored Flux-Lang flows, and make `~/.flux/flows` (+ `.flux/flows`) the
single home for reusable `.flux` definitions — flows, ops, or mixed modules — with composite ops there
auto-loading as callable ops.

## Acceptance
- [x] `flow_list` lists every flow and composite op under `.flux/flows` / `~/.flux/flows` (and the legacy
  `.flux/ops` / `@global_ops`) with description + params. (Manual: `flux flow run` a flow calling
  `flow_list()` → shows the flow + op.)
- [x] `flow_run(name, inputs?)` resolves a flow by name, seeds `inputs` as literal binds, and runs it in the
  current session via the depth-guarded `run_plan` reentry. (Manual: `flow_run({name:"ping", inputs:{who:"flux"}})` → `"pong: flux"`.)
- [x] A composite `op` placed in `~/.flux/flows` auto-loads and is directly callable. (Manual: `greet("world")` → `"hi world"`.)
- [x] `Workspace::base_for` resolves a bare `@name` (no subpath) to its named root, so directory reads of
  `@global_ops`/`@global_flows` work — they silently returned nothing before.
- [x] Docs updated: `composite-ops.md`, `flux-flow.md`, `ops-reference.md`, `SKILL.md`, and the public
  website (`ops`/`tooling`/`storage`/`troubleshooting`/`modules-and-programs`).

## Progress
- 2026-07-09: Implemented `flow_list`/`flow_run` (`crates/flux-tools/src/flows.rs`), the `@global_flows`
  root + `register_flows` (`flux-cli/src/main.rs`), lenient flows-dir composite loading
  (`flux-flow/src/composites.rs` `load_flows_dir`), and the `base_for` fix (`flux-system/src/lib.rs`).
  Built, clippy-clean, installed, verified end-to-end. CHANGELOG entries landed under `[0.11.6]`; docs swept.

## Notes
- Key files: `crates/flux-tools/src/flows.rs`, `crates/flux-flow/src/composites.rs` (`load_flows_dir`),
  `crates/flux-cli/src/main.rs` (`@global_flows`, `register_flows`), `crates/flux-system/src/lib.rs` (`base_for`).
- Design: [composite-ops.md](../designs/composite-ops.md) (§ "The `~/.flux/flows` home"). CHANGELOG: `[0.11.6]`.
- Search precedence: `.flux/flows` → `@global_flows` → `.flux/ops` → `@global_ops`. `flow_run` needs a `LoopHost`.
- Supersedes the `~/.flux/ops`-only notes in [[L-06]] / [[C-21]].
