# flux — backlog & status board

The single screen for **"what to work on next and where we are."** One file per story lives in this
directory (`<ID>-<slug>.md`, frontmatter carries `pillar`/`status`/`priority`); this board indexes
them by status. New work? Copy [`_TEMPLATE.md`](_TEMPLATE.md). For the bigger picture see the
[docs map](../README.md); for the working loop see [AGENTS.md](../../AGENTS.md) → **"Start here"**.

> Keep this board in sync when a story's `status` changes. (A small generator that rebuilds it from
> frontmatter may automate this later — see the docs map.)

## Status
- **Released:** v0.2.4 (2026-06-25). **In flight (`[Unreleased]`):** one agent loop everywhere
  (classic `flux-agent::Agent` retired) with per-turn token usage flowing through it; global
  multi-format skills; A2A client; provider wire robustness (OpenRouter/Ollama via the Anthropic
  Messages protocol); and the self-improvement offline half — partial-credit tag scalar, durable token
  capture, and a stable-baseline synthetic loop. See [CHANGELOG](../../CHANGELOG.md).
- **Gate:** green — `cargo test` · `clippy -D warnings` · `fmt` · the `flux-codegate` layering lint.

## Now (in progress)
- [I-01 — Statistically clean headline gain](I-01-headline-gain.md) · Improve · offline half done
  (partial-credit scalar + durable token capture + synthetic `trials = 5` loop); the trials ≥ 5
  grader-confirmed run is **staged** on a funded provider key

## Next (ready — take the top one unless the user named a story)
_(none ready — promote one from Backlog below)_

## Blocked
_(none)_

## Backlog (unranked — promote to **Next** with a `priority` when ready)
- [L-02 — flux-markdown engine + progressive-disclosure skills](L-02-flux-markdown-engine.md) · Language · AST parser, body-on-demand activation

### Downstream enablement (managed-agents) — ranked by leverage on the managed-agents critical path
These exist to support the downstream multi-tenant agents service (path-dep consumer of flux). See
[roadmap → Downstream enablement](../roadmap.md#downstream-enablement-managed-agents).
- **1.** [D-01 — Parameterized flow execution (behaviour-runner seam)](D-01-flow-input-seeding.md) · Agent · **highest** · `parse` + per-run input seeding into `FlowClient`; serves managed-agents R-01/A-03 ([design](../designs/flow-input-seeding.md))
- **2.** [D-02 — Tenant/context-taggable event substrate](D-02-tenant-event-substrate.md) · Core · **high** · account/agent tag + account-scoped projections on `flux-events`; decide early so managed-agents R-04 is a projection, not a retrofit
- **3.** [D-03 — Reusable A2A server helpers (current spec)](D-03-a2a-server-helpers.md) · Agent · **medium** · lift flux-server's A2A routes into a helper; unblocks managed-agents E-02 + fixes the `tasks/send` drift
- **4.** [D-04 — Event-trigger channels (cron/webhook/Slack)](D-04-event-trigger-channels.md) · Agent · **medium** (epic) · background agents waking on events; schedule adapter first (fluxplane prior art)
- **5.** [D-05 — Harden the sub-agent primitive for multi-tenant production](D-05-sub-agent-hardening.md) · Agent · **medium** · SDK seam + lifecycle limits + pluggable approver/tested isolation + child audit; the primitive managed-agents R-03/A-05 consume ([design](../designs/sub-agent-hardening.md))

## Done
- [C-01 — Crate consolidation, phases 2–4](C-01-crate-consolidation.md) · Core · hooks→plugin, browser+datasource→capabilities, context→runtime; removed dead integrations (35 → 31 crates)
- [A-02 — A2A client (`flux a2a <URL>`)](A-02-a2a-client.md) · Agent · consume a remote A2A agent like a local one; server clean-cutover to the current spec (see [CHANGELOG](../../CHANGELOG.md))
- [A-01 — Unify on FlowEngine, retire the classic Agent loop](A-01-unify-flowengine.md) · Agent · one loop everywhere; `flux-agent` repurposed as the `AgentSpec` home (see [CHANGELOG](../../CHANGELOG.md))
- [L-01 — Global, multi-format skill loading](L-01-global-skills.md) · Language · multi-dir + Agent-Skills/Claude format + `flux-markdown` (see [CHANGELOG](../../CHANGELOG.md))

## Done
Completed stories roll into [CHANGELOG.md](../../CHANGELOG.md): set `status: done` in the file and
remove its row here.
