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
_(none ready — the Slack-channel assistant integration stack D-07/D-10/D-08/D-09 all shipped; see Done. Promote one
from Backlog below.)_

## Blocked
_(none)_

## Backlog (unranked — promote to **Next** with a `priority` when ready)
- [L-02 — flux-markdown engine + progressive-disclosure skills](L-02-flux-markdown-engine.md) · Language · AST parser, body-on-demand activation

### Downstream enablement (managed-agents) — queued behind the active Slack-channel assistant stack above
These support the multi-tenant **managed-agents** service (path-dep consumer). The Slack-channel assistant's integration stack
(D-07/D-10/D-08/D-09) was promoted to **Next** above and now leads; these are next in line. See
[roadmap → Downstream enablement](../roadmap.md#downstream-enablement-managed-agents-Slack-channel assistant).
- **1.** [D-02 — Tenant/context-taggable event substrate](D-02-tenant-event-substrate.md) · Core · **high** · account/agent tag + account-scoped projections on `flux-events`; decide early so managed-agents R-04 is a projection, not a retrofit
- **2.** [D-03 — Reusable A2A server helpers (current spec)](D-03-a2a-server-helpers.md) · Agent · **medium** · lift flux-server's A2A routes into a helper; unblocks managed-agents E-02 + fixes the `tasks/send` drift

## Done
- [D-08 — Integration plugin pack](D-08-integration-plugin-pack.md) · Agent · 8 native plugins in the in-repo `plugins/` workspace (websearch/gitlab/jira/confluence/kubernetes/loki/prometheus/slack) on a shared `host-kit`; reach vendors only via host caps; contribute `flux-datasource` records through the L5 `DatasourceHostCaps` bridge (commits `0e9b93e`/`deafe68`/`6b20c41`)
- [D-09 — Agentic channel target](D-09-agentic-channel-target.md) · Agent · `trigger.agent` wakes an `AgentSpec` turn (per-thread session memory + declared grants, `0d8ac58`) + the registry wiring (`App::with_tools` loads datasource + plugin tools on the `flux app run` path, `e4710ad`)
- [D-10 — Process-plugin protocol redesign](D-10-process-plugin-protocol.md) · Core · enriched the plugin manifest (auth-by-purpose, datasource declarations, endpoints) + host capabilities (HTTP method/headers/body + bearer injection, secret-by-purpose, endpoint, datasource-record contribution) over the existing unified frame; `DatasourceHostCaps` L5 bridge (commits `f389bc7`/`7db537a`)
- [D-07 — Knowledge datasource (a real RAG layer)](D-07-knowledge-datasource-rag.md) · Core · new L0 `flux-datasource` schema crate + a `DatasourceBackend` trait with in-memory + **SQLite-FTS5** backends, the five retrieval ops (`search`/`get`/`list`/`relation`/`batch_get`), markdown + OpenAPI ingesters, reindex/freshness, and an unwired embeddings seam (commits `2642479`/`e6d7279`/`5241c97`)
- [D-01 — Parameterized flow execution (the behaviour-runner seam)](D-01-flow-input-seeding.md) · Agent · deterministic `FlowClient::parse` (no model round-trip) + a per-run input-seeding seam (`FlowStore::seed` + `FlowClient::execute_with`/`run_flow`) so a stored flow runs per invocation with injected `$var` settings — fresh-store isolation, flow-local binds shadow seeds, envelope unchanged; modules, zero new crates; serves managed-agents R-01/A-03 (see [CHANGELOG](../../CHANGELOG.md))
- [D-06 — Realtime voice-to-voice as a first-class flux provider](D-06-realtime-voice-provider.md) · Agent · sibling `RealtimeProvider`/`RealtimeSession` seam (modules in flux-provider/flux-providers/flux-flow — zero new crates) + OpenAI-Realtime impl lifted from managed-agents; realtime tool calls run through `Executor` declared once; SDK `FlowClient::run_voice_session` + a Phase-2 engine-owned-turns spike (see [CHANGELOG](../../CHANGELOG.md))
- [D-04 — Event-trigger channels (cron/webhook/Slack)](D-04-event-trigger-channels.md) · Agent · new `flux-channels` L6 crate; channels declared in the Program + run by `flux app run` (each fires a bus event → trigger → journey via `App::deliver`); schedule/webhook/Slack adapters (see [CHANGELOG](../../CHANGELOG.md))
- [D-05 — Harden the sub-agent primitive for multi-tenant production](D-05-sub-agent-hardening.md) · Agent · SDK seam (`FlowClient::with_sub_agents`) + lifecycle limits (cancel/wall-clock) + pluggable approver + tested isolation + child audit; the primitive managed-agents R-03/A-05 consume (see [CHANGELOG](../../CHANGELOG.md))
- [C-01 — Crate consolidation, phases 2–4](C-01-crate-consolidation.md) · Core · hooks→plugin, browser+datasource→capabilities, context→runtime; removed dead integrations (35 → 31 crates)
- [A-02 — A2A client (`flux a2a <URL>`)](A-02-a2a-client.md) · Agent · consume a remote A2A agent like a local one; server clean-cutover to the current spec (see [CHANGELOG](../../CHANGELOG.md))
- [A-01 — Unify on FlowEngine, retire the classic Agent loop](A-01-unify-flowengine.md) · Agent · one loop everywhere; `flux-agent` repurposed as the `AgentSpec` home (see [CHANGELOG](../../CHANGELOG.md))
- [L-01 — Global, multi-format skill loading](L-01-global-skills.md) · Language · multi-dir + Agent-Skills/Claude format + `flux-markdown` (see [CHANGELOG](../../CHANGELOG.md))

## Done
Completed stories roll into [CHANGELOG.md](../../CHANGELOG.md): set `status: done` in the file and
remove its row here.
