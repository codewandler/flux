---
id: A-127
title: Roles carry an address — one delegation vocabulary, local or remote
pillar: Agent
status: backlog
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-agent, flux-orchestrate, flux-capabilities]
note: "⚠ cap_scope is enforced by constructing the child registry IN-PROCESS; across the wire it becomes a request, not an enforcement — that divergence must be surfaced, never silently trusted"
---

# Roles carry an address — one delegation vocabulary, local or remote

## Goal
Sub-agent roles and fleet agents are today **disjoint namespaces**: `Role`
(`crates/flux-agent/src/role.rs:16`) has no address, `AgentDecl` cannot be remote, and no code
bridges them. Give `Role` one optional typed fleet target so `task(role)` routes to `LocalSpawner`
when absent and `A2aSpawner` when present — the same delegation the agent already knows, now able to
land on another machine.

## Acceptance
- [ ] `Role` gains an optional typed target based on A-126's endpoint/worker reference; absent means
      in-process, exactly as today. The strict frontmatter parser (`try_parse_role`,
      `deny_unknown_fields`) accepts it and still rejects unknown keys. Do not reintroduce A-120's
      runtime-selecting `AgentAddress`.
- [ ] `task(role)` routes on the address: `LocalSpawner`
      (`crates/flux-orchestrate/src/lib.rs:276`) when absent, `A2aSpawner` (A-116) when present.
      Failing-first test: the same role file with and without an address produces an in-process
      child and a remote dispatch respectively, with identical result shape to the caller.
- [ ] **Failing-first test for the safety divergence** — the one that matters: `cap_scope` is
      enforced today by constructing the child's narrowed `ToolRegistry` in-process
      (`lib.rs:290`, `:310`, `task` stripped at leaf depth `:325`). A remote worker constructs its
      own registry in a different trust domain, so the requested scope is a **declared intent**. The
      test pins that a remote role whose worker reports a wider scope than requested is surfaced as
      a divergence — not silently accepted, and not silently narrowed in a way that lies about what
      ran.
- [ ] The depth/nesting bounds (A-25) still hold across a remote hop — a remote child cannot escape
      the delegation cap by being on another machine.
- [ ] Documented in the role reference: an address makes the role's `tools` list a request, and the
      worker's own policy is authoritative.

## Progress
- (not started)

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md) — "The roles/fleet
  unification".
- Depends on A-116 (`A2aSpawner`) and A-126's fleet target vocabulary.
- Related, unfixed: the `task` op's schema still does not expose the role list to the model
  (`crates/flux-orchestrate/src/lib.rs:1070` hardcodes examples), so the model guesses role names.
  Out of scope here — worth its own story.
