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
- [D-11 — App-runner ergonomics](D-11-app-runner-ergonomics.md) · Agent · the ready pick (makes
  `flux app run` a viable host for a declarative bot; unblocks Slack-channel assistant flows).

## Blocked
_(none)_

## Backlog (unranked — promote to **Next** with a `priority` when ready)
- [L-02 — flux-markdown engine + progressive-disclosure skills](L-02-flux-markdown-engine.md) · Language · AST parser, body-on-demand activation
- [D-11 — App-runner ergonomics for declarative bots](D-11-app-runner-ergonomics.md) · Agent · configurable `flux app run` knowledge ingest + OpenAPI + persona/event-context-from-file; blocks declarative assistant flows

### fluxplane-plugins parity (epic) — full native parity with the 26-plugin fluxplane pack
The integration breadth+depth push: every *portable* fluxplane plugin rewritten natively at full op coverage,
gated by D-12. See [epic design](../designs/fluxplane-plugins-parity.md). **D-12 through D-17 shipped** (see
Done); **D-22** added the single guarded spawn path + plugin authoring guide.

### Endpoint discovery & brokerage (epic) — references-only plugin IO + cross-plugin endpoint discovery
The missing fluxplane essentials feature, and a **new top priority**: make a host-managed **reference** the
only currency a plugin operation handles (never an env var, raw secret, or credential-bearing URL), then
broker **cross-plugin endpoint discovery** over it — the kubernetes plugin discovers cluster + in-cluster
service endpoints (prometheus/loki/grafana/alertmanager/sql) and hands consumers a *weak reference*; the host
resolves it and injects credentials host-side, so neither the plugin nor the LLM ever sees a secret. Reverses
the `.dex`-style endpoint-registry deferral from D-10/D-12. See [epic design](../designs/endpoint-discovery.md).
**[D-20](D-20-scoped-private-net-egress.md) is pulled in as a hard dependency** (discovered endpoints are
usually private/in-cluster hosts). **The epic core (D-25 → D-30 + D-20) is complete.** Built in this order:
- [D-30 — Endpoint lifecycle: refresh runner, CLI & audit](D-30-endpoint-lifecycle-cli.md) · Core · **done** ·
  periodic rediscovery + `flux endpoint list/show/resolve/import` (weak refs + health, never secrets) + audit
- Follow-up hardenings (backlog, not epic-blocking):
  - [D-31 — Host-terminated raw-socket auth](D-31-host-terminated-rawsocket-auth.md) · Core · the host speaks
    the Postgres/AMI handshake so even trusted raw-socket plugins never hold a credential value
  - [D-32 — Retire the `host.endpoint` URL-handback](D-32-retire-url-handback.md) · Core · close the last 5–6
    residual URL-handback call sites and delete the capability (compile-enforced cutover)

### Plugin platform hardening — lifecycle, internal-network reach, distribution
Gaps surfaced while verifying the plugin install + running `scripts/smoke-plugins.sh` (the gitlab case fails
only because the SSRF guard refuses a private-network GitLab fixture — see D-20).
- [D-19 — Complete the `flux plugin` lifecycle surface](D-19-plugin-lifecycle-cli.md) · Core · add `uninstall`
  + a richer `status`/`info` (version, pin, liveness, declared surface); small, no design doc
- [D-21 — Plugin distribution for non-source users](D-21-plugin-distribution.md) · Core · scoping/epic-seed: how
  a non-repo user obtains the pack (bundled binaries / fetch-on-install / marketplace); produces a design + the
  follow-on stories, no code

### Downstream enablement — queued behind the active Slack-channel assistant stack above
These support multi-tenant managed-agent services (path-dep consumers). **D-02 and D-03 both shipped**
(see Done); the remaining consumer-specific items are not yet filed. See
[roadmap → Downstream enablement](../roadmap.md#downstream-enablement).

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
- [D-30 — Endpoint lifecycle: refresh runner, CLI & audit](D-30-endpoint-lifecycle-cli.md) · Core · the
  epic's final step: `EndpointBroker::refresh` re-discovers + reconciles each owner's set via
  `replace_owned` (stale dropped, other owners untouched) driven on-demand by `EndpointRunner::tick`
  (no always-on ticker — it would contend with the agent's plugin-host locks); a `flux endpoint`
  CLI (`list`/`show`/`resolve`/`import`) renders weak refs + health + the credential *location*, never
  a value (pinned by `cli::endpoint_list_redacts`); the agent `endpoint.import` op persists a weak ref
  to `~/.flux/endpoints.toml`; and a new `EndpointDiscovered` audit event fires per provider on
  `discover`/`refresh` (count only — no URL, no secret). The **endpoint-discovery epic core
  (D-25→D-30 + D-20) is complete**; D-31 (host-terminated raw-socket auth) and D-32 (retire the
  `host.endpoint` URL-handback) are filed as backlog hardenings.
- [D-29 — Migrate native plugins to references](D-29-migrate-plugins-to-references.md) · Agent · primary
  plugin IO is now reference-based (the `SystemHostCaps` named-vs-discovered split + host-kit `*_ref`
  helpers; 6 URL-handback callers migrated), and `sql` consumes a discovered Postgres endpoint (password
  via the gated `credential` capability, never a URL/model). Full removal of `host.endpoint` is a tracked
  follow-up (byte-IO / Atlassian gateway / config residuals)
- [D-28 — Kubernetes endpoint provider](D-28-kubernetes-endpoint-provider.md) · Agent · the reference
  provider (`kubernetes.endpoint.discover` → cluster/Service/Ingress/RDS `EndpointCandidate`s with a
  `credential_ref`, latest-namespace) + the broker op-name reconciliation + the agent-facing
  `endpoint.discover/select/info/list` ops, `endpoint` group, `kubernetes` signal, and a mapping skill —
  the "connect to my latest namespace backend RDS" demo path is now wired
- [D-27 — Reference-based IO & host-injected connect](D-27-reference-based-io.md) · Core · enforces the
  references-only invariant: `http.do`/`conn.dial` take an `endpoint_ref`, the host resolves + injects
  the credential (cross-plugin `Kubernetes`-scheme via the owning plugin's `secret.read`), gated
  deny-by-default + operator grant + first-use-approval seam + `CrossPluginResolve` audit; the gated
  `credential` capability serves raw-socket in-band auth (trusted plugin only, never the model)
- [D-26 — Discovery provider role & host fan-out broker](D-26-endpoint-discovery-broker.md) · Core ·
  manifest `discovers`/`discover` + the L5 `EndpointBroker` (fan-out over a `ProviderInvoker` seam, rank,
  re-entrancy guard) + `EndpointBrokerHostCaps`; wired into both `flux run` and `flux app run` so a
  consumer plugin's `endpoint.discover` reaches provider plugins through the host
- [D-20 — Scope private-network egress](D-20-scoped-private-net-egress.md) · Core · finished as the
  endpoint-epic Phase-2 prereq: the 0.2.7 scoped model gained **per-endpoint** grants
  (`PrivateNetConfig.endpoints`) and a **private-net-admit audit event** (`PrivateNetAdmit` via the
  `flux_plugin::EgressAudit` seam + a flux-cli event-store impl)
- [D-25 — Endpoint reference model & registry](D-25-endpoint-reference-model.md) · Core · the
  references-only spine of the endpoint epic: L0 `flux_secret::endpoint` schema (`EndpointRef`/
  `Candidate`/`Record`/`ResolvedEndpoint`), the `flux_plugin::ReferenceResolver` trait seam, and
  `flux_capabilities::endpoint::{EndpointRegistry, StaticResolver}` with `~/.flux/endpoints.toml`
  persistence (weak refs only)
- [I-02 — Reduce wasted agent-loop retries](I-02-agent-loop-retry-efficiency.md) · Improve · cargo
  wrappers normalize duplicate model-supplied scope flags, and the loop guard fingerprints repeated
  deterministic failures before replanning again
- [L-07 — Generate Flux skills from live catalogs](L-07-generated-flux-skills.md) · Language ·
  `flux skill` renders Claude-format root/CLI/language/plugin/ops skills from live Clap, Flux-Lang,
  ToolRegistry/OpRegistry, and plugin-manifest sources; `--install` writes root + sections, and project
  `.claude/skills` is loaded by default after `.flux/skills`
- [L-06 — Let agents register reusable composite ops](L-06-agent-registered-composite-ops.md) · Language ·
  `op.register` lets an agent install validated Flux-Lang composite ops into turn/session/project/global
  scope; project/global writes use guarded paths and registered ops remain scoped composite dispatch
- [L-05 — Publish public Docusaurus docs](L-05-public-docusaurus-docs.md) · Language · public
  Docusaurus site scaffold under `website/`, with Flux-Lang text syntax/semantics docs and GitHub Pages
  deployment to `codewandler.github.io/flux`
- [L-04 — Define custom ops by composing Flux-Lang](L-04-composite-ops.md) · Language · `.flux` modules
  can declare scoped composite ops that chain existing ops through the normal safety envelope; SDK,
  `flux flow run`, and `flux-app` install module composites; new argv-only `proc.run` process op is
  shell-group gated
- [D-24 — Harden provider schemas and CLI daemon ergonomics](D-24-provider-schema-and-cli-hardening.md) ·
  Agent · `emit_plan` now advertises the derived `DraftAst` schema, `flux plugin call` resolves short op
  names, served daemons honor SIGTERM, and `flux tui` fails clearly without a TTY
- [D-23 — Serve agents through flux app run](D-23-app-run-serve-a2a.md) · Agent · removed the standalone
  `flux serve` command; `flux app run --serve <addr> --yes` now serves the built-in coding agent, `.flux`
  programs can declare an `a2a` channel, and one-agent programs can be exposed ad hoc with `--serve`
- [D-22 — One guarded process-spawn path + plugin authoring guide](D-22-guarded-spawn-and-plugin-authoring.md) · Core · funneled all OS-process creation through one `flux_system` `build_command` (+ new `spawn_interactive`); `PluginHost::spawn` routed through it so the **plugin process is env-cleared** — a plugin can no longer read the host's secrets via `std::env` (gated `secret` is the only path), closing a bypass of the deny-by-default model; flux-runtime git-context also via `System::run`; new `plugins/AUTHORING.md` (linked from AGENTS.md + README); env-isolation regression test; full root gate green
- [D-17 — Telephony plugin pack](D-17-telephony-plugins.md) · Agent · added native `asterisk` (8 AMI ops over guarded `conn.*`) and `homer` (8 HTTP/JWT ops, including SIP search/call/QoS/PCAP export); full `plugins/` gate green
- [D-16 — Datastore & infra plugin pack](D-16-datastore-infra-plugins.md) · Agent · added native `sql` (6 PostgreSQL read/introspection ops over `ConnStream`, MySQL/SQLite explicit residuals), `docker` (33 core Docker Engine REST ops over Unix socket), and `aws` (11 read-only ops via host-managed `aws` CLI); full `plugins/` gate green
- [D-15 — Observability & AI plugin pack](D-15-observability-ai-plugins.md) · Agent · added native `alertmanager` (5 ops), `grafana` (20), `opsgenie` (8), and `huggingface` (9), with datasource contributions and env-gated smoke coverage; full `plugins/` gate green
- [D-14 — Deepen the 8 native plugins to full op-parity](D-14-deepen-native-plugins.md) · Agent · all 8 `plugins/` at fluxplane op + **behavioural** parity (+~160 ops): gitlab 6→64, slack 5→30, kubernetes 5→24, jira 3→21, confluence 3→15, prometheus 4→8, loki 3→5, websearch +`provider.list`. Added two **host protocol** capabilities (managed background processes `process.spawn/read/status/kill`; binary HTTP body `body_b64`/`response_binary`). jira/confluence auth re-ported to the reference (Bearer/`cloud_id` gateway + Basic fallback); k8s port-forward on managed processes; byte-exact attachments/files; jira ADF + transition scorer, slack mentions/unreads, gitlab `diff.lines` regex ported faithfully. One MockHost test per op; `plugins/` + host gate green
- [D-13 — Generated plugin skill (`flux plugin skill`)](D-13-plugin-skill-command.md) · Core · renders
  installed-plugin manifests into a Claude-format `flux-plugin` SKILL.md + `references/` (the flux analogue
  of fluxplane's `fluxplane-plugin skill`); now also exposed through `flux skill plugin`
- [D-12 — Plugin protocol parity extensions](D-12-plugin-protocol-parity.md) · Core · additive host caps for the missing fluxplane plugins — non-Bearer auth injection (A: Basic/header/query by purpose) + raw `conn.*` dialer (B) + `blob.*` store (C); clean extension of `flux.plugin.v1`, unblocks D-14..D-17 (commit `a21bc47`)
- [D-03 — Reusable A2A server helpers](D-03-a2a-server-helpers.md) · Agent · lifted flux-server's A2A routes into the reusable `flux_a2a::server` helper; unblocks downstream A2A consumers + fixed the `tasks/send` drift (commit `7dcc6b3`)
- [D-02 — Tenant/context-taggable event substrate](D-02-tenant-event-substrate.md) · Core · optional stream-level account/agent/correlation context envelope on `flux-events` runs + account-scoped reads (`list_for_account`/`account_streams`) (commit `c97c8a4`)
- [L-03 — Native-text module declarations (`.flux` does all of it)](L-03-native-text-program-grammar.md) · Language · the whole app — `agent`/`channel`/`datasource`/`trigger`/`journey` + flows — in native flux-lang text (settings inline, secrets as `secret "ENV"` refs); JSON-program path deleted (clean cutover); `flux app run`/`flux flow run` load native text; supersedes the JSON manifest (see [design](../designs/native-text-modules.md))
- [C-02 — Integration-stack hardening](C-02-integration-stack-hardening.md) · Core · `flux plugin call`/`install` + a `plugins/` CI job (`a8092dc`); feature-gated embeddings/semantic backend — `OpenAiEmbedder` + a `SemanticIndex` hybrid-rerank decorator, default build unchanged (`f912c24`); a live env-gated `scripts/smoke-plugins.sh` (`5fda8be`)
- [D-08 — Integration plugin pack](D-08-integration-plugin-pack.md) · Agent · 8 native plugins in the in-repo `plugins/` workspace (websearch/gitlab/jira/confluence/kubernetes/loki/prometheus/slack) on a shared `host-kit`; reach vendors only via host caps; contribute `flux-datasource` records through the L5 `DatasourceHostCaps` bridge (commits `0e9b93e`/`deafe68`/`6b20c41`)
- [D-09 — Agentic channel target](D-09-agentic-channel-target.md) · Agent · `trigger.agent` wakes an `AgentSpec` turn (per-thread session memory + declared grants, `0d8ac58`) + the registry wiring (`App::with_tools` loads datasource + plugin tools on the `flux app run` path, `e4710ad`)
- [D-10 — Process-plugin protocol redesign](D-10-process-plugin-protocol.md) · Core · enriched the plugin manifest (auth-by-purpose, datasource declarations, endpoints) + host capabilities (HTTP method/headers/body + bearer injection, secret-by-purpose, endpoint, datasource-record contribution) over the existing unified frame; `DatasourceHostCaps` L5 bridge (commits `f389bc7`/`7db537a`)
- [D-07 — Knowledge datasource (a real RAG layer)](D-07-knowledge-datasource-rag.md) · Core · new L0 `flux-datasource` schema crate + a `DatasourceBackend` trait with in-memory + **SQLite-FTS5** backends, the five retrieval ops (`search`/`get`/`list`/`relation`/`batch_get`), markdown + OpenAPI ingesters, reindex/freshness, and an unwired embeddings seam (commits `2642479`/`e6d7279`/`5241c97`)
- [D-01 — Parameterized flow execution (the behaviour-runner seam)](D-01-flow-input-seeding.md) · Agent · deterministic `FlowClient::parse` (no model round-trip) + a per-run input-seeding seam (`FlowStore::seed` + `FlowClient::execute_with`/`run_flow`) so a stored flow runs per invocation with injected `$var` settings — fresh-store isolation, flow-local binds shadow seeds, envelope unchanged; modules, zero new crates; serves downstream behaviour-runner/preset consumers (see [CHANGELOG](../../CHANGELOG.md))
- [D-06 — Realtime voice-to-voice as a first-class flux provider](D-06-realtime-voice-provider.md) · Agent · sibling `RealtimeProvider`/`RealtimeSession` seam (modules in flux-provider/flux-providers/flux-flow — zero new crates) + OpenAI-Realtime impl ported from a downstream realtime client; realtime tool calls run through `Executor` declared once; SDK `FlowClient::run_voice_session` + a Phase-2 engine-owned-turns spike (see [CHANGELOG](../../CHANGELOG.md))
- [D-04 — Event-trigger channels (cron/webhook/Slack)](D-04-event-trigger-channels.md) · Agent · new `flux-channels` L6 crate; channels declared in the Program + run by `flux app run` (each fires a bus event → trigger → journey via `App::deliver`); schedule/webhook/Slack adapters (see [CHANGELOG](../../CHANGELOG.md))
- [D-05 — Harden the sub-agent primitive for multi-tenant production](D-05-sub-agent-hardening.md) · Agent · SDK seam (`FlowClient::with_sub_agents`) + lifecycle limits (cancel/wall-clock) + pluggable approver + tested isolation + child audit; the primitive downstream multi-tenant consumers use (see [CHANGELOG](../../CHANGELOG.md))
- [C-01 — Crate consolidation, phases 2–4](C-01-crate-consolidation.md) · Core · hooks→plugin, browser+datasource→capabilities, context→runtime; removed dead integrations (35 → 31 crates)
- [A-02 — A2A client (`flux a2a <URL>`)](A-02-a2a-client.md) · Agent · consume a remote A2A agent like a local one; server clean-cutover to the current spec (see [CHANGELOG](../../CHANGELOG.md))
- [A-01 — Unify on FlowEngine, retire the classic Agent loop](A-01-unify-flowengine.md) · Agent · one loop everywhere; `flux-agent` repurposed as the `AgentSpec` home (see [CHANGELOG](../../CHANGELOG.md))
- [L-01 — Global, multi-format skill loading](L-01-global-skills.md) · Language · multi-dir + Agent-Skills/Claude format + `flux-markdown` (see [CHANGELOG](../../CHANGELOG.md))

## Done
Completed stories roll into [CHANGELOG.md](../../CHANGELOG.md): set `status: done` in the file and
remove its row here.
