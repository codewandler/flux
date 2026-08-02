---
id: A-147
title: Separate harness protocol, agent profile, and repository context
pillar: Agent
status: done
priority: 1
epic: layered-agent-context
design: docs/designs/layered-agent-context.md
note: "The shipped Flux contract becomes an embedded prefix; coding behavior, personas, repository policy, and workspace data are distinct layers"
---

# Separate harness protocol, agent profile, and repository context

## Goal
Make every Flux-backed model call retain a small harness-owned protocol while keeping coding behavior,
agent personas, repository policy, and workspace evidence independently owned and inspectable.

## Acceptance
- [x] Failing-first: `general_agent_keeps_harness_core_without_coding_profile` proves authored
      instructions cannot replace the Flux protocol and do not implicitly select the coding profile.
- [x] Failing-first: `role_body_is_instructions_after_the_harness_core` proves a role body specializes
      rather than replaces the harness-owned prefix.
- [x] The CLI and default SDK builder select the coding profile; app agents and roles default to the
      general profile unless explicitly configured otherwise.
- [x] Project context is assembled as typed, provenance-carrying layers and rendered in deterministic
      order after the harness/profile/persona layers.
- [x] Generic Flux harness instructions live in embedded assets, while root `AGENTS.md` is a compact,
      agent-agnostic repository contract.
- [x] Public migration docs, engineering/customer changelogs, and harness status reflect the breaking
      `system_prompt` to `profile` + `instructions` split.
- [x] Full workspace build, test, clippy, format check, and `flux-codegate` pass.

## Progress
- 2026-08-03: plan grounded against the context-package review and current prompt assembly paths.
- 2026-08-03: captured failing-first compile evidence for the two profile/role tests, then embedded
  the harness, coding profile, tool guidance, built-in roles, and strict-review protocol as package
  assets. Root `AGENTS.md` fell from 19,600 to 4,920 bytes without becoming harness input.
- 2026-08-03: shipped typed prompt/project layers with trust, source, capture time, size, and digest;
  added body-free-by-default `flux context show`; and migrated SDK, role, app, CLI, and docs surfaces
  from replaceable `system_prompt` state to explicit profile plus instructions.
- 2026-08-03: `cargo build --workspace`, `cargo test --workspace`, clippy with warnings denied,
  format check, `flux-codegate`, embedded-doc sync, and diff check all pass. No terminal-bench score is
  claimed: its self-improvement runner requires a clean tree, while this shared tree contains
  unrelated user work that this story deliberately preserved.

## Notes
- Review source: `docs/reviews/single/2026-08-02-agent-context-package-review.md`.
- This changes prompt composition only. Authorization, approval, dispatch, and guarded IO remain
  unchanged and continue to be the enforcement boundary.
