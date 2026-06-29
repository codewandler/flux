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
- The **fluxplane-plugins parity epic** continues: **D-14 shipped** (all 8 plugins at op + behavioural
  parity; see Done). **D-15** (observability/AI), **D-16** (datastore/infra), **D-17** (telephony) are the
  ready follow-ons — all unblocked now D-12 (+ the D-14 host extensions: managed processes, binary body)
  shipped. See the epic in Backlog below.
- [D-11 — App-runner ergonomics](D-11-app-runner-ergonomics.md) · Agent · the alternate ready pick (makes
  `flux app run` a viable host for a declarative bot; unblocks Slack-channel assistant S-01/S-03/S-06).

## Blocked
_(none)_

## Backlog (unranked — promote to **Next** with a `priority` when ready)
- [L-02 — flux-markdown engine + progressive-disclosure skills](L-02-flux-markdown-engine.md) · Language · AST parser, body-on-demand activation
- [D-11 — App-runner ergonomics for declarative bots](D-11-app-runner-ergonomics.md) · Agent · configurable `flux app run` knowledge ingest + OpenAPI + persona/event-context-from-file; blocks Slack-channel assistant S-03/S-06

### fluxplane-plugins parity (epic) — full native parity with the 26-plugin fluxplane pack
The integration breadth+depth push: every *portable* fluxplane plugin rewritten natively at full op coverage,
gated by D-12. See [epic design](../designs/fluxplane-plugins-parity.md). **D-12/D-13 shipped** (see Done);
**D-14 leads** (in progress above). D-12 is landed, so D-15/D-16/D-17 are all unblocked.
- [D-15 — Observability & AI plugin pack](D-15-observability-ai-plugins.md) · Agent · alertmanager, grafana, opsgenie, huggingface (HTTP; D-12 Slice A auth ready)
- [D-16 — Datastore & infra plugin pack](D-16-datastore-infra-plugins.md) · Agent · sql, docker, aws (D-12 Slice B conn + blob ready)
- [D-17 — Telephony plugin pack](D-17-telephony-plugins.md) · Agent · asterisk, homer — serves the managed-agents voice surface (D-12 Slice B conn ready)

### Downstream enablement (managed-agents) — queued behind the active Slack-channel assistant stack above
These support the multi-tenant **managed-agents** service (path-dep consumer). **D-02 and D-03 both shipped**
(see Done); the remaining managed-agents items are not yet filed. See
[roadmap → Downstream enablement](../roadmap.md#downstream-enablement-managed-agents-Slack-channel assistant).

### Subscription providers & cross-provider cost (epic) — claude-code/codex hardening + usage/cost
Harden the two **subscription/passthrough** providers (reuse the desktop apps' tokens + refresh, no full
OAuth2 yet), make codex's **websocket** the default transport, and add **full usage + cost tracking across
all providers**. Most plumbing already exists (`flux-credentials` import/refresh, `claude`/`codex` providers,
`-m claude|codex/...` routing) — this is harden/verify/extend + the new cost layer. See
[epic design](../designs/subscription-providers-and-cost.md). Built in this order (C-03/C-04/C-05 parallel):
- **1.** [C-03 — Codex provider hardening](C-03-codex-provider-hardening.md) · Core · `account_id` from the
  `id_token` JWT, cache+reasoning token capture, reasoning continuity under `store:false`
- **1.** [C-04 — Claude verify + force-refresh-on-401](C-04-claude-401-refresh.md) · Core · refresh today is
  expiry-time-only; add a 401→refresh→retry path (shared by both subscription providers)
- **1.** [C-05 — Cross-provider pricing & cost model](C-05-pricing-cost-model.md) · Core · per-model per-tier
  rates + `cost(&Usage, model)`; built-in table + `~/.flux/pricing.toml` override; normalize codecs' cache fields
- **2.** [C-06 — Usage & cost accounting](C-06-usage-cost-accounting.md) · Core · model attribution +
  sub-agent rollup + a `cost_summary` projection + `flux usage` + a server endpoint + cache-aware surfacing (needs C-05)
- **2.** [C-07 — Codex WebSocket transport (default, HTTP fallback)](C-07-codex-websocket-transport.md) · Core ·
  WS primary with transparent HTTP-SSE fallback (needs C-03)
- **later.** [C-08 — Full OAuth2 login (codex PKCE)](C-08-full-oauth2-login.md) · Core · the explicit later
  stage; import + refresh cover the near term

## Done
- [D-14 — Deepen the 8 native plugins to full op-parity](D-14-deepen-native-plugins.md) · Agent · all 8 `plugins/` at fluxplane op + **behavioural** parity (+~160 ops): gitlab 6→64, slack 5→30, kubernetes 5→24, jira 3→21, confluence 3→15, prometheus 4→8, loki 3→5, websearch +`provider.list`. Added two **host protocol** capabilities (managed background processes `process.spawn/read/status/kill`; binary HTTP body `body_b64`/`response_binary`). jira/confluence auth re-ported to the reference (Bearer/`cloud_id` gateway + Basic fallback); k8s port-forward on managed processes; byte-exact attachments/files; jira ADF + transition scorer, slack mentions/unreads, gitlab `diff.lines` regex ported faithfully. One MockHost test per op; `plugins/` + host gate green
- [D-13 — Generated plugin skill (`flux plugin skill`)](D-13-plugin-skill-command.md) · Core · renders installed-plugin manifests into a trigger-activated `flux-plugins` SKILL.md + `references/` (the flux analogue of fluxplane's `fluxplane-plugin skill`); flux-markdown frontmatter writer (commit `7030261`)
- [D-12 — Plugin protocol parity extensions](D-12-plugin-protocol-parity.md) · Core · additive host caps for the missing fluxplane plugins — non-Bearer auth injection (A: Basic/header/query by purpose) + raw `conn.*` dialer (B) + `blob.*` store (C); clean extension of `flux.plugin.v1`, unblocks D-14..D-17 (commit `a21bc47`)
- [D-03 — Reusable A2A server helpers](D-03-a2a-server-helpers.md) · Agent · lifted flux-server's A2A routes into the reusable `flux_a2a::server` helper; unblocks managed-agents E-02 + fixed the `tasks/send` drift (commit `7dcc6b3`)
- [D-02 — Tenant/context-taggable event substrate](D-02-tenant-event-substrate.md) · Core · optional stream-level account/agent/correlation context envelope on `flux-events` runs + account-scoped reads (`list_for_account`/`account_streams`) (commit `c97c8a4`)
- [L-03 — Native-text module declarations (`.flux` does all of it)](L-03-native-text-program-grammar.md) · Language · the whole app — `agent`/`channel`/`datasource`/`trigger`/`journey` + flows — in native flux-lang text (settings inline, secrets as `secret "ENV"` refs); JSON-program path deleted (clean cutover); `flux app run`/`flux flow run` load native text; supersedes the JSON manifest (see [design](../designs/native-text-modules.md))
- [C-02 — Integration-stack hardening](C-02-integration-stack-hardening.md) · Core · `flux plugin call`/`install` + a `plugins/` CI job (`a8092dc`); feature-gated embeddings/semantic backend — `OpenAiEmbedder` + a `SemanticIndex` hybrid-rerank decorator, default build unchanged (`f912c24`); a live env-gated `scripts/smoke-plugins.sh` (`5fda8be`)
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
