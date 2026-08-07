---
id: C-688
title: "A start-path census pins the resolved loop binding"
pillar: "Core"
status: backlog
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-flow, flux-agent, flux-cli]
note: "C-569 shipped the contract but no test fails when a new start path skips the funnel; a re-pointed census exists on fleet/wave-299/flux/story/C-569"
---

# A start-path census pins the resolved loop binding

## Goal

C-569 landed the resolved loop-binding contract — `AgentLoopBindingMetadata` in
`crates/flux-core/src/agent_loop.rs`, `assemble_with_binding` and `pub agent_loop_binding:
AgentLoopBinding` in `crates/flux-flow/src/engine.rs`, `validate_runtime` refusing unsupported
features before the first model call, and per-turn reconstruction via `turn.loop_binding` /
`equivalent_to`. What it did not land is anything that goes **red when a new agent-start path
skips that funnel**. The contract holds today because the funnel is the only way in; nothing
enforces that it stays the only way in.

A census for exactly this exists and is recoverable, written by wave-299's worker before that
wave was found redundant and cancelled:

    git show fleet/wave-299/flux/story/C-569:crates/flux-flow/tests/loop_binding_census.rs

It is 203 lines, has the same shape and role as `flux-tools`' `worktree_base_is_pinned.rs` and
`flux-server`'s `router_env_is_pinned.rs`, and is honest in its own doc comment about scanning
text rather than a syntax tree. **All six of its `START_PATHS` still hold against `main`** —
`flux-cli/src/execution.rs`, `flux-sdk/src/assembly.rs`, `flux-orchestrate/src/lib.rs`,
`flux-orchestrate/src/worker.rs`, `flux-app/src/app.rs`, `flux-cli/src/app_cmd.rs`.

Its assertions do not. They were written against wave-299's own architecture, which put the
binding in a separate `crates/flux-flow/src/loop_binding.rs`; `main` puts it in `engine.rs`
under different names. Re-pointing is the work:

| census expects | `main` has |
| --- | --- |
| `crates/flux-flow/src/loop_binding.rs` | the binding in `crates/flux-flow/src/engine.rs` |
| `pub loop_binding:` on `FlowEngine` | `pub agent_loop_binding:` (engine.rs:559) |
| `pub source_reference:` / `pub sha256:` / `pub entry:` / `pub required_features:` | `source_ref` / `source_sha256` / `entry_point` / `required_runtime_features` |
| `assemble_with_loop` resolving an `AgentLoopSelection` | `assemble_with_binding` (engine.rs:780) |
| a `"loop.binding"` receipt string in the funnel | no such literal — find where identity is actually recorded, or drop this assertion |

## Acceptance

- [ ] A census test fails when a start path can reach a runnable engine without a resolved binding, and passes on `main` as it stands. Its assertions name `main`'s real spellings, not wave-299's.
- [ ] Every start path it claims is verified to still exist, so the census cannot go vacuous — an entry whose call has moved turns the test red with a message saying to re-point it, rather than silently measuring nothing.
- [ ] The scanner's own pin is kept: a call named in a `//` comment tail is not a call.
- [ ] Any assertion that cannot be re-pointed truthfully is dropped rather than weakened, and the story records which and why. A census that asserts less than it appears to is worse than a smaller one that is exact.
- [ ] The full gate passes.

## Notes

Filed rather than salvaged inline while closing wave-299: rewriting half a test against a
codebase under concurrent change, without a gate run, is how a red test ships. The branch is
retained indefinitely, so nothing is lost by deferring it.
