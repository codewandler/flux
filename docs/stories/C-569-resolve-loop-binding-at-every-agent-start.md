---
id: C-569
title: "Every agent start resolves and snapshots an explicit loop binding"
pillar: Core
status: done
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-agent, flux-flow, flux-runtime, flux-orchestrate, flux-cli]
note: "general omission resolves to builtin adaptive; task/Fleet/backend starts carry a versioned binding and never inherit a parent's loop implicitly"
---

# Resolve behavior before starting the agent

## Goal

Make loop selection a required resolved field of the common agent-start contract so every top-level,
sub-agent, Fleet and served start says exactly which behavior harness it is running.

## Acceptance

- [x] Failing first, a start-path census proves role/task and Fleet child constructors can currently
      reach a running engine with an independently defaulted loop and no durable loop identity. The
      fixed census covers CLI/SDK, roles through `task`, nested children, Fleet writers/reviewers/
      decision agents, app agents and served/A2A task starts.
- [x] A resolved `AgentLoopBinding` carries logical profile/revision, runner kind, immutable source
      reference/digest, entry point and required runtime features. Start/status/terminal receipts
      expose bounded identity and digest metadata, never loop source or prompts.
- [x] An ordinary omitted selector resolves to the explicit versioned adaptive preset before start.
      A sub-agent resolves its role/request policy and never implicitly copies the parent's loop or
      context. Fleet task roles require an explicit policy-selected binding.
- [x] Missing profiles, changed digests, invalid source, missing operations and unsupported runtime
      features refuse before the first model call with the exact mismatch.
- [x] Message, restart, resume, rework and recovery reconstruct the admitted binding. File/config/
      role changes affect new starts only; switching a live worker requires an explicit new
      admission/session transition.
- [x] Capability and budget inheritance remain narrow-only and are not represented as context or
      loop inheritance. Existing role-specific authored loops and top-level `--loop` behavior remain
      compatible after resolution.
- [x] Focused unit/conformance tests and the full gate pass.

## Progress

- 2026-08-06 — live Fleet-main dogfood proved that an explicit attachment still defaults to the
  general adaptive loop and that role/task starts do not expose a durable resolved identity. C-556
  owns the urgent coordinator-only surface; this story remains the common start-contract repair so
  configured sub-agents preserve their own authored binding on start, message, resume and recovery.
- 2026-08-06 — implemented the common `AgentLoopBinding` contract and resolution boundary. Omitted
  general starts now resolve `adaptive@1`; CLI sessions persist digest-addressed source and reject a
  live binding switch; roles, nested `task` children and SDK callers carry explicit bindings; start
  and terminal events plus streaming receipts expose bounded metadata.
- 2026-08-06 — implemented Fleet task-kind policy, admission-time capability validation, exact
  source/metadata snapshots, reconstruction for message/resume/rework and bounded identity in
  worker/run/terminal receipts. `fleet.run --prepare-only` now returns the admitted worker ids and
  wave linkage needed by the coordinator. Focused engine, role, orchestration, SDK, CLI stream and
  Board/Fleet suites pass; install/full-gate/live-TUI evidence remains before transition to `done`.
- 2026-08-06 — installed TUI dogfood admitted `wave-234-worker-1` and `wave-242-worker-1` with the
  policy-selected `implementation` loop, bounded profile/digest identity and explicit task kind;
  `fleet.agents` listed each durable worker and the coordinator cancelled both waves through
  acknowledged Fleet operations. Restarting the durable `s_1` coordinator also proved that
  semantically set-valued binding metadata survives legacy ordering without permitting a real
  binding change. The mandatory release gate is green. Board transition remains pending until the
  implementation is committed on the Board's pinned canonical ref.
- 2026-08-06 — a later full-gate rerun over the enlarged dirty diff exposed a shared runtime defect:
  plain-text redaction could consume a JSON escape inside the adaptive loop's serialized state and
  make `$intent.kind` unreadable. Tool-result redaction now walks valid JSON structurally whenever a
  secret match fires, retains byte-exact output otherwise, and the strict-review journey regression
  passes with the credential-shaped fixture in the live diff.

## Start-path census

| Start path | Pre-fix failure | Fixed boundary / evidence |
|---|---|---|
| CLI and SDK | Each engine could independently default without durable identity. | CLI resolves and caches a binding per session; `ClientBuilder` accepts an explicit binding and common `AgentSpec` assembly resolves omission. |
| Role and `task` child | A role loop was only source selection; nested children could acquire a host default. | Role conversion resolves its owned binding; `LocalSpawner` copies the admitted child binding through nested spawners, not the parent conversation. |
| Fleet writer/reviewer/decision and ad-hoc starts | Templates selected model/capabilities but no loop identity; message/rework reread mutable configuration. | Required task-kind policy resolves before worktree creation, snapshots exact source and metadata, and every later start reconstructs and digest-checks that snapshot. |
| App and served/A2A starts | These paths reached the same independently defaulting `AgentSpec` assembly. | The common `AgentSpec::into_engine` boundary now resolves or validates a binding before provider access, so app/server callers cannot construct a runnable unbound engine. |
| Restart/recovery | No durable loop field existed to compare or reconstruct. | Turn projections preserve binding metadata; engine lifecycle refuses a different binding for a live session and CLI/Fleet recover exact digest-addressed source. |

## Notes

- This supplies the binding operators need to run their own authored sub-agent loops and the stable
  identity C-543 displays and switches. Postponed C-567 is optional policy/convenience work.


## Comments

- Rescued from wave-299's dead worker and committed on its story branch as 5edcb8ed (940 new lines plus 204 changed). The repository gate then refused the candidate: role::tests::a_role_resolves_its_own_loop_binding_and_never_the_parents panics with `role 'triage' has an invalid agent loop: invalid explicit agent loop: parse error: line 1`. The test's inline authored loop is `return "done"`, which is not a parseable Flux-Lang program on its own — it has no flow declaration. Either the fixture is wrong or an authored role loop is meant to accept a bare expression; deciding that by inference would produce a green test asserting the wrong thing, so the story stays open. The work is preserved on fleet/wave-299/flux/story/C-569 and is NOT in 0.57.0. Note also that the handoff gate accepted this commit because the cited test (loop_binding_census) passes — a handoff verifies the argv it is given, so a commit can break a different test in the same story and only the repository gate will say so.


## Evidence

- Closed against work already in main, not a new implementation. crates/flux-flow/src/engine.rs carries the resolved binding on the engine (`pub agent_loop_binding: AgentLoopBinding`, :559), assembles from it (`assemble_with_binding`, :780), refuses unsupported runtime features before the first model call (`validate_runtime`, :836) and reconstructs the admitted binding per turn (`turn.loop_binding` / `equivalent_to`, :1004-1011). crates/flux-core/src/agent_loop.rs defines `AgentLoopBindingMetadata` carrying profile, revision, runner, source_ref, source_sha256, entry_point, required_operations and required_runtime_features. crates/flux-agent/src/role.rs gives roles their own binding via `AgentLoopBinding::native_flux`. Landed by a9bfb475. wave-299 independently re-implemented the same contract as crates/flux-flow/src/loop_binding.rs and was never merged; that branch is retained, not deleted.
