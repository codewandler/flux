---
id: C-457
title: "Could flux be the environment worker for someone else's brain? — the one place our envelope plugs into another harness"
pillar: Core
status: ready
priority: 7
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [flux-cli, flux-runtime]
note: "⚠ an INVESTIGATION with a real product idea inside it. Anthropic's self-hosted sandbox is a polled work queue — outbound HTTPS only, no inbound. A worker claims an item, downloads skills, RUNS THE TOOL CALLS, posts results back. flux is exactly `infrastructure you control` with a policy envelope"
---

# Their brain, our hands — with an envelope around them

## Goal

Establish whether flux can serve as an **environment worker** for Anthropic's Managed Agents, and what
it would be worth.

## Why this is interesting rather than merely possible

The self-hosted sandbox protocol is unusually easy to meet:

- The environment *"acts as a work queue"*. A worker **polls** it — *"needs only outbound HTTPS"* — or
  wakes on a `session.status_run_started` webhook.
- On claiming a work item the worker *"spawns an execution context…, downloads the agent's skills, runs
  the tool calls, and posts the results back."*
- The poller injects `ANTHROPIC_SESSION_ID`, `ANTHROPIC_WORK_ID`, `ANTHROPIC_ENVIRONMENT_ID`,
  `ANTHROPIC_ENVIRONMENT_KEY` into a spawn script; deliverables land in the working directory.

⚠ **The interesting part is what flux would add**: Managed Agents has no per-effect approval, no
authorization envelope and no evidence chain. flux has all three, and its whole thesis is that the
runtime stays the authority after the model has spoken. **flux as the environment worker is that thesis
applied to somebody else's brain** — the one integration where flux's differentiator is the product
rather than a precondition.

## Acceptance

- [ ] A **decision with evidence**, not a build: can flux's `Executor::dispatch` envelope wrap tool calls
      that arrive from an external orchestrator, and what breaks if it refuses one?
- [ ] ⚠ **The refusal case is the whole investigation.** If flux denies an effect the remote model asked
      for, does the protocol carry a "refused, here is why" result the model can reason about — or does
      it read as a tool error and get retried in a loop? An envelope whose refusals look like failures
      makes the agent fight it.
- [ ] ⚠ **State the data-residency limit honestly**: execution stays on your host, but *"tool inputs and
      outputs still flow to Anthropic's control plane."* flux cannot change that, and a page implying
      otherwise would be worse than not building this.
- [ ] Where the approval prompt appears, given the operator is not watching a terminal. Overlaps
      [C-453](C-453-a-remote-approval-channel.md) — likely the same answer.
- [ ] ⚠ **A recommendation, including "no".** *"flux's envelope does not usefully compose with an
      orchestrator that expects tools to succeed"* would be a completely acceptable and valuable outcome.
- [ ] The protocol is **beta** and behind `managed-agents-2026-04-01`. Pin what was read, and when.

## Notes

- ⚠ Do not build a second dispatch path. If this happens, tool calls arrive through the same
  `Executor::dispatch` chokepoint as everything else — `AGENTS.md` names it, and an external orchestrator
  is not a reason to widen it.
- The alternative, cheaper integration is **flux as an MCP server** to a managed agent — Managed Agents
  takes MCP servers as tool providers. That exposes tools without the envelope wrapping their execution,
  so it is a different and weaker proposition. Worth naming so the two are not confused.
- Related: [C-456](C-456-the-managed-agent-topologies.md) documents the topologies; this asks whether we
  should occupy one.

## Progress
- Filed 2026-08-02.
