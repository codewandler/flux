---
id: C-567
title: "Fleet workers use assignment-selected operator-authored loops"
pillar: Core
status: backlog
epic: agent-loop-harnesses
design: docs/designs/agent-loop-harnesses.md
areas: [flux-agent, flux-flow, flux-cli, flux-runtime]
depends_on: [C-566, C-569]
note: "postponed convenience policy — configured sub-agents can already select operator-authored loops through C-569"
---

# Give Fleet workers the loop their assignment needs

## Goal

Let Fleet select an operator-authored native loop from explicit template/task-kind policy so an
implementation worker executes one assigned contract instead of routing every job through Flux's
general adaptive explorer. Flux supplies the binding and lifecycle contract, not a product-selected
implementation strategy.

## Acceptance

- [ ] Failing first, a hermetic five-writer-shaped fixture proves the existing Fleet launcher sends
      every story worker through the adaptive `detect_intent`/`explore` loop and cannot distinguish a
      useful partial tree from the observed 50-call/history-budget terminal failure.
- [ ] Fleet config declares versioned loop profiles and maps an explicit task kind/template default
      to one profile. Selection never asks a model to infer kind from issue prose; a future Jira,
      Trello or other Board backend may map its typed metadata without becoming a loop runner.
- [ ] The operator-authored implementation profile used by the fixture reads the exact repository
      instructions, story and linked design, establishes validation evidence, implements only that
      contract, runs targeted checks, invokes the profile's required terminal signal and returns
      C-244's typed handoff. It does not select work, coordinate Fleet, explore unrelated
      repositories or review itself. Flux does not ship a dedicated implementation profile in this
      tranche.
- [ ] Fleet writer/reviewer/decision roles have no implicit adaptive fallback. General non-Fleet
      agents retain C-569's explicit resolved adaptive default; research work may select a declared
      exploratory profile.
- [ ] Admission snapshots the resolved loop id/revision/source digest/entry point and required
      runtime features beside the model, mode, capability, worktree and fences. Message, restart,
      resume and rework preserve it; drift requires explicit re-admission.
- [ ] A model answer without the configured acknowledged terminal signal remains incomplete. The
      signal cannot mark Board work done, validate the worker's own commit or bypass host review,
      integration and repository gates.
- [ ] C-566's fresh assignment-only context and C-565's capability ceiling remain unchanged. Loop
      instructions cannot widen roots/tools, set Board status, satisfy host evidence or apply/push.
- [ ] Hermetic lifecycle coverage proves selection/default refusal, invalid/missing loop refusal,
      five concurrent native workers, exact continuation and bounded loop receipts without provider
      credentials or network.
- [ ] The roadmap dogfood launches five fresh writers on five different stories across Flux,
      Connectors and Exchange; all five retain their admitted ceiling, create exact story commits
      and produce handoff-ready bounded receipts.
- [ ] The public Fleet guide, loop docs, design, changelog, website mirror and embedded documentation
      explain profile selection, task-kind mapping, isolation and recovery without implying that
      `working` alone proves progress.

## Progress

- 2026-08-05 — the initial contract proposed a Codex-specific CLI runner. The full open-story census
  showed that `AgentSpec`, roles and Flux-Lang already provide the correct authored-loop seam, while
  C-552/C-553 separately own foreign task backends. Respecified before implementation as the urgent
  native workhorse-loop slice of C-568; the uncommitted fake-Codex experiment stays preserved.
- 2026-08-06 — Decision 0014 removed the shipped-workhorse requirement before implementation.
  Fleet now binds operator-authored profiles, snapshots their identity and requires an acknowledged
  terminal lifecycle signal; a dedicated built-in implementor remains a separate future decision.
- 2026-08-06 — postponed by operator direction and removed from the active Fleet wave and dependency
  graph. Operators can run their own authored sub-agent loops once C-569 resolves the common binding;
  this story is optional policy/convenience work, not a prerequisite.

## Notes

- Depends on C-569's common resolved binding. C-572 composes the separate fresh reviewer/repair
  loops; review is deliberately not a writer phase.
