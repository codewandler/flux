---
id: C-456
title: "Two more topologies exist and we do not list them — Anthropic's cloud sandbox, and its self-hosted worker"
pillar: Core
status: ready
priority: 6
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [website, docs]
note: "⚠ the second one is C-436 INVERTED: Anthropic keeps orchestration and moves tool execution to infrastructure you control. Same seam as flux-runtime/flux-system, opposite side — which is outside validation that the seam is real, and a row the topologies page cannot honestly omit"
---

# The operational topologies someone else already shipped

## Goal

Add the two Managed Agents shapes to the topologies page, and describe how they differ from flux's —
so a reader comparing options meets an honest map rather than only flux's half of it.

## The two rows

| topology | brain | hands | approval |
|---|---|---|---|
| **Managed Agents, cloud sandbox** | Anthropic | Anthropic | ⚠ none per-effect — steer or interrupt |
| **Managed Agents, self-hosted sandbox** | Anthropic | **your infrastructure** | ⚠ none per-effect |

Their architecture is *"decoupling the brain from the hands"*: a **stateless** harness that restarts
without data loss, execution provisioned only when a tool is called, and the **session as a durable log
stored outside both**. Stated payoff: time-to-first-token down ~60% at p50 and >90% at p95, because
inference starts before a container exists.

⚠ **The self-hosted variant is exactly C-436 inverted.** *"Self-hosted sandboxes keep the orchestration
on Anthropic's side but move tool execution into infrastructure you control."* flux keeps orchestration
with **you** and moves execution away. Same joint, opposite sides — which is the strongest outside
evidence that `execution-substrate.md`'s split is the real one, and worth saying on the page.

## Acceptance

- [ ] Both rows added, answering the page's own two questions: **where are my files** and **where does
      the approval prompt appear**.
- [ ] ⚠ **The approval row is the honest differentiator and must not be softened in either direction.**
      Managed Agents deliberately has no per-effect approval — you steer or interrupt. That is coherent
      for long-running async work, not a defect, and the page must not imply otherwise. ⚠ Equally: flux's
      own served-agent row is *allow-all or deny-all* until [C-453](C-453-a-remote-approval-channel.md)
      lands, so on that axis flux is currently **no better**, and the page must say so.
- [ ] Data residency stated for the self-hosted row: tool execution stays on your host, but *"tool inputs
      and outputs still flow to Anthropic's control plane"*. That is the whole reason someone would ask.
- [ ] The status column stays honest — these are **someone else's shipped product**, not flux roadmap.
      Distinguish "exists, not ours" from "ours, proposed".
- [ ] ⚠ No competitor-bashing and no comparison-table framing. [C-429](C-429-the-recipes-surface-and-positioning.md)
      already decided public positioning argues from architecture. This is a map, and a map that flatters
      its author is not a map.
- [ ] Full gate green including the website checks.

## Notes

- Follow-up to [C-440](C-440-the-topologies-page.md) rather than part of it — that page was landing when
  these surfaced, and adding rows mid-gate would have been scope creep on a finished diff.
- [C-457](C-457-flux-as-an-environment-worker.md) is the capability question these rows raise.

## Progress
- Filed 2026-08-02.
