# Layered agent context

## Decision

Flux no longer treats the system prompt as one caller-replaceable string. A model-backed Flux agent
receives an ordered context package:

1. **Harness protocol** — a small embedded Flux contract present on every agent-backed model call.
2. **Agent profile** — `coding` for the coding harness, `general` for authored roles and applications.
3. **Instructions** — the role/persona authored by an SDK, app, or role file.
4. **Repository policy** — `AGENTS.md`, `CLAUDE.md`, `.flux/context.md`, and applicable fragments.
5. **Workspace data** — environment, Git, repository shape, knowledge, skills, and runtime notes.

The order describes ownership and precedence, not authorization. All effects still traverse the
runtime's authorization → approval → guarded-IO envelope.

## Ownership

Generic Flux behavior is compiled from Markdown assets in its owning crate (`flux-agent` for the
harness/profile/built-in roles, `flux-app` for the strict-review protocol); it does not live in a
workspace's `AGENTS.md`, `.agents`, `.claude`, or authored role files. Cross-harness repository
playbooks may remain in `.agents`/`.claude` when their procedure is agent-agnostic. Project-authored
Flux roles remain optional resources in `.flux/agents`, never the source of shipped defaults.

Root `AGENTS.md` is the small always-loaded repository contract and routes specialized work to its
authoritative runbook. Historical rationale and subsystem-only procedures do not occupy the default
prompt.

## API and defaults

`AgentSpec` exposes `profile: AgentProfile` and `instructions: String`; the ambiguous public
`system_prompt` field is removed. `AgentSpec::coding(model)` selects the coding profile, while
`AgentSpec::general(model, instructions)` selects only the universal protocol plus authored persona.

The ordinary CLI and SDK builders default to `coding`. Role bodies and app-agent descriptions are
instructions under `general` unless they explicitly request `profile: coding`. The universal harness
protocol is not replaceable through these high-level APIs.

## Context representation

Prompt contributions carry a stable id, kind, trust class, cache class, optional source and capture
time, plus their body. Rendering is deterministic. Repository-controlled instructions and workspace
snapshots are visibly tagged and cannot impersonate the harness-owned prefix. A manifest exposes
metadata, sizes, and hashes without printing bodies by default.

`flux context show` renders that body-free startup manifest without resolving a provider or loading
plugins; `--body` is an explicit disclosure, while `--profile` and repeated `--tool` values make
conditional embedded layers reproducible. A real CLI agent additionally records the exact
tool-aware manifest as the `agent.context_manifest` startup observation.

Static harness/profile assets remain the first cache-stable prefix. Session-start repository context
is session-stable; turn notes remain dynamic. Existing safety-sensitive context read failures stay
fail-closed.

## Compatibility

This is a pre-1.0 breaking change. SDK callers migrate from `system_prompt(text)` to
`profile(AgentProfile::General).instructions(text)` for a custom non-coding agent, or simply
`instructions(text)` to specialize the coding builder. Role and app bodies no longer erase the Flux
harness protocol.

## Verification

Tests cover the CLI, SDK, app-agent, and sub-agent composition matrix; exact layer ordering and
metadata; conditional profile inclusion; and cache stability. Harness evaluation is a matched
before/after no-regression check, never a reason to weaken a runtime guard.
