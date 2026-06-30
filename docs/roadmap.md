# flux — roadmap & status

Status as of **0.2.4 (2026-06-25)**: public + installable at
[codewandler/flux](https://github.com/codewandler/flux); 31 crates, **450+ tests**, a permanently green
gate (tests, clippy `-D warnings`, fmt, the `flux-codegate` layering lint). See
[CHANGELOG.md](../CHANGELOG.md) for the released history and [architecture.md](architecture.md) for the
design.

## Delivered

The build proceeded breadth-first (every surface exists as a crate) and was then hardened in depth.

**Foundations & breadth (M0–M5)** — the workspace + layering lint; the content/message/streaming
model; the provider layer (wire codec × credential; five providers; credential store with PKCE login
and CLI-credential import; `provider/model` routing); the guarded IO boundary and the mandatory safety
envelope; built-in tools; SQLite sessions; the context projector; skills; markdown roles; multi-agent
orchestration; JS hooks; subprocess plugins; the SDK, HTTP server, integrations, browser/web egress,
datasource/RAG, evidence, and the OIDC identity seam.

**Hardening (M6–M9)** — provider retry/backoff; config loading + persistence; the authorization
policy wired into the envelope (default-deny + a usable local default); real secret redaction;
evidence + destructive-op escalation; capability & integration depth (`glob`/`grep`, `web_fetch`,
`search`, plugins-as-tools with host-capability callbacks, plugin lifecycle, skill activation,
policy-bounded sub-agents); streaming everywhere (CLI/TUI tokens, server SSE, in-TUI approval modal);
cancellation; autopilot (`/pd` dependency waves, `/goal`, `/loop`); context compaction; the layering
lint; CI; Anthropic prompt caching; the OIDC claims→identity seam.

**Review remediation** — two adversarial review passes were run against the hardened code and every
confirmed finding fixed with a regression test:
- *Post-M8/M9 review (R1–R8)* — session-shape breakers (empty-assistant-on-cancel, compaction
  splitting a tool_use/tool_result pair), uninterruptible autopilot, and CI/cache nits.
- *Full-tree security review (0.1.1)* — sandbox-escape, plugin-capability, server-auth, env-leak,
  policy-approval, SSRF, redaction, OAuth-state, and a batch of panic/DoS/correctness fixes. See the
  `[0.1.1]` CHANGELOG entry for the itemized list.

**Daily-driver readiness (0.2.0)** — repo-aware context (git working-tree + project-shape context
providers), a real reedline REPL (line editing, persistent history, reverse-search, visible thinking),
a whitespace-tolerant `edit` tool, `flux sessions` + `/resume`, mid-session `/model` switching, and a
live-provider smoke gate (`scripts/smoke-live.sh`). Validated end-to-end against a real provider.

**Public release (0.2.1)** — flux is open-source (MIT OR Apache-2.0) and installable at
`codewandler/flux`: dual-license files + CONTRIBUTING/SECURITY + issue/PR templates; a cargo-dist
release pipeline producing prebuilt binaries for all five targets + shell/PowerShell installers on every
tagged release; CI running the full gate on every push.

## Standing pre-release gate (do this before every release)

A **live-provider smoke test** is the manual gate that the offline mock can't replace (the mock
doesn't enforce provider message-shape rules — which is exactly how the session-shape breakers
slipped through). With a real key (e.g. `anthropic/opus`), exercise:
- a one-shot (`flux run -p`),
- an agentic file edit under the envelope (`flux run --yes`, scratch workspace),
- a multi-turn `--continue` that replays tool-call history,
- a compaction-then-continue past a tiny `FLUX_COMPACT_CHARS` (validates no 400 on the rewritten log),
- (semi-manual) a Ctrl-C mid-turn in the REPL, then a follow-up turn in the same session.

This is scripted as `scripts/smoke-live.sh` (model overridable via `FLUX_SMOKE_MODEL`) — run it
before every release.

A second, **integration-plugin** smoke (`scripts/smoke-plugins.sh`) exercises the D-08 plugin pack against
real vendor APIs: for each integration whose credential is in the environment it builds the plugin,
registers it in an isolated registry, and drives one op via `flux plugin call`, asserting a non-error
result; plugins whose key is absent are **skipped** (not failed). Run it (with whatever keys you have —
`TAVILY_API_KEY`, `GITLAB_PERSONAL_TOKEN`, `SLACK_BOT_TOKEN`, …) before releasing anything touching the
plugins. The semantic/embeddings path (`--features embeddings`) is validated manually with a feature build
(`FLUX_EMBEDDINGS_API_KEY`); its rerank logic is covered by the default-build unit test.

## Next

### Downstream enablement

A ranked track that exists to **unblock and de-risk downstream products** that consume flux by **path
dependency** (no version boundary, so flux churn breaks them directly; tightening these seams also eases
that coupling): multi-tenant managed-agent services and Slack-channel assistants. Sourced from cross-repo
audits; filed as the **D- story track** (see the [board](stories/README.md)). Slack-channel assistants
consume the shipped channel transport (D-04) and drive the **active integration stack** — built in this
order: a knowledge/RAG datasource (**D-07**, which adds the shared `flux-datasource` schema) → a clean
**process-plugin protocol redesign** (**D-10**) → a native integration-plugin pack (**D-08**, in an in-repo
`plugins/` workspace) → an agentic channel target (**D-09**). The app these consumers author is now a single
**native flux-lang `.flux`** file — `agent`/`channel`/`datasource`/`trigger`/`journey` module declarations
with secrets as `secret "ENV"` references, replacing the JSON manifest
([L-03](stories/L-03-native-text-program-grammar.md), [design](designs/native-text-modules.md)).

1. **[D-01](stories/D-01-flow-input-seeding.md) — Parameterized flow execution (the behaviour-runner
   seam)** · ✅ **shipped.** A deterministic `FlowClient::parse(text)` (no model round-trip) + a per-run
   input-seeding seam (`FlowStore::seed` + `FlowClient::execute_with`/`run_flow`) so a stored, validated
   Flux-Lang flow runs per invocation with effective-settings injected as `$vars` (not baked into the AST)
   and custom ops registered — fresh-store isolation, flow-local binds shadow seeds, the safety envelope
   unchanged; one-shot (genuine cross-turn `await` stays on the engine). Modules, zero new crates.
   Unblocks downstream behaviour-runner and preset-framework consumers. Design:
   [flow-input-seeding.md](designs/flow-input-seeding.md).
2. **[D-02](stories/D-02-tenant-event-substrate.md) — Tenant/context-taggable event substrate** · *high.*
   Tag `flux-events` with an account/agent context + an account-scoped projection read API, so downstream
   run-persistence/transparency is a projection over the log, not a parallel store. "Build it in,
   not on" — decide while R-01 lands, or it's a retrofit.
3. **[D-03](stories/D-03-a2a-server-helpers.md) — Reusable A2A server helpers (current spec)** · *medium.*
   Lift flux-server's inline A2A routes (`message/send`/`message/stream`/`tasks/get`) into a reusable
   helper. Unblocks downstream A2A consumers **and** fixes drift where older consumers still serve the
   deleted `tasks/send` dialect (removed in the A-02 cutover, commit `06065f6`).
4. **[D-04](stories/D-04-event-trigger-channels.md) — Event-trigger channels (cron/webhook/Slack)** ·
   ✅ **shipped.** A `flux-channels` (L6) crate so agents **wake on external events** (schedule, webhook,
   Slack). Routes each event to a **journey** declared in the `.flux` program, run by `flux app run`
   (the App-runner route, superseding the design's `EngineTarget`; that agentic target is now **D-09**).
   Background agents woken by events; Slack-channel assistants consume the Slack adapter directly.
5. **[D-05](stories/D-05-sub-agent-hardening.md) — Harden the sub-agent primitive for multi-tenant
   production** · ✅ **shipped.** Closed the five gaps a downstream service hits: a consumable `flux-sdk`
   seam (`FlowClient::with_sub_agents` over a reusable `SubAgents` assembly — the CLI consumes the same
   helper), lifecycle limits (parent-cancellation threading + wall-clock-as-cancel + configurable
   `SpawnLimits`), a pluggable approver (`with_approver`) + a tested workspace-confinement isolation
   guarantee, and child tool calls threaded into a shared audit store (`with_audit`; the account tag +
   explicit parent-session link ride D-02). Isolation is per-scope composition, not new sandboxing.
   Unblocks multi-tenant sub-agent consumers. Design: [sub-agent-hardening.md](designs/sub-agent-hardening.md).
   Two lifecycle gaps documented (parent-turn cancel finalization; per-engine concurrent-turn cancel
   slot) — see the design's "Known limitations".
6. **[D-06](stories/D-06-realtime-voice-provider.md) — Realtime voice-to-voice as a first-class flux
   provider** · ✅ **shipped.** A **sibling, session-oriented provider seam**
   (`RealtimeProvider`/`RealtimeSession`, full-duplex) beside the half-duplex `Provider`, plus an
   OpenAI-Realtime impl ported from a downstream realtime client. Realtime tool calls route through the
   **same `Executor` envelope** with tools declared **once** from the live `ToolRegistry`, so downstream
   consumers can delete parallel voice-model stacks (bespoke WS clients, double tool-declaration, scattered keys).
   Built as **modules, zero new crates** (L0 `flux_core::audio`, L1 `flux_provider::realtime` +
   `flux_providers::realtime` behind a feature, L3 `flux_flow::voice`, SDK `FlowClient::run_voice_session`)
   + a Phase-2 engine-owned-turns spike (`run_flow_turns`/`VoiceTurnHandler`; per-turn `run_turn`, not yet
   cross-turn `await`). Downstream consumer rewiring is a separate pass outside this repo. Design:
   [realtime-voice-provider.md](designs/realtime-voice-provider.md).
7. **[D-07](stories/D-07-knowledge-datasource-rag.md) — Knowledge datasource (a real RAG layer)** ·
   *Slack assistant · ready (rank 1).* Turn `flux-capabilities::datasource` from an in-memory keyword index into a
   real knowledge layer: a new **L0 `flux-datasource` schema crate** (record/declaration/lookup, shared with
   the plugin layer), a persistent sqlite index, `search`/`list`/`get`/`relation`/`batch_get`, and
   reindex/freshness — keyword/BM25 behind a pluggable embeddings seam. Grounds Slack assistant answers in
   help-center + OpenAPI docs. Design: [datasource-rag.md](designs/datasource-rag.md).
8. **[D-10](stories/D-10-process-plugin-protocol.md) — Process-plugin protocol redesign** · *Slack assistant ·
   ready (rank 2).* Redesign `flux-plugin`'s wire protocol/manifest/binding-SDK so a plugin can call ops,
   contribute & query **datasource records** (feeding D-07), and request host capabilities (HTTP with
   secret-by-purpose injection, process/env/blob/conn) over **one clean unified frame** — informed by
   fluxplane's evolved protocol but dropping its cruft (dual modes, three command families, per-call grant
   negotiation). Clean cutover of `flux.plugin.v1`. Blocks D-08. Design:
   [process-plugin-protocol.md](designs/process-plugin-protocol.md).
9. **[D-08](stories/D-08-integration-plugin-pack.md) — Integration plugin pack** · *Slack assistant (epic) ·
   ready (rank 3).* Native flux plugins (capability-gated, over the D-10 protocol) for the DevOps surface —
   Slack ops, websearch, GitLab, Jira, Confluence, Kubernetes, Loki, Prometheus — in an **in-repo
   `plugins/` cargo workspace** (excluded from root, so heavy deps stay out of the main gate; *reverses* the
   earlier sibling-repo plan). Each emits `flux-datasource` records reaching D-07's index via an L5
   `DatasourceHostCaps` bridge. Slice 1 (Slack ops + websearch) unblocks the assistant MVP. Design:
   [integration-plugins.md](designs/integration-plugins.md).
10. **[D-09](stories/D-09-agentic-channel-target.md) — Agentic channel target** · *Slack assistant · ready
    (rank 4).* Let a channel wake an `AgentSpec` `run_turn` (model drives RAG + tools) **alongside** the
    shipped journey route, with per-conversation thread memory + declared op grants — builds the
    `EngineTarget` the D-04 design deferred, via a new `Deliverer` (the Slack adapter is unchanged). Also
    wires the `flux app run` path to **load plugins + register datasource tools** (today CLI-only). Design:
    [agentic-channel-target.md](designs/agentic-channel-target.md).

### fluxplane-plugins parity (epic)

flux shipped **8** native plugins (D-08) over the D-10 protocol; the fluxplane pack they were modelled on has
**26 marketplace plugins**, and flux's 8 cover a fraction of their ops (gitlab 6/60+, slack 5/30, jira 3/~20,
k8s 5/24). This epic drives **full native parity**: every *portable* fluxplane plugin rewritten as a native
flux plugin at full op coverage, plus a generated plugin skill so the catalog is self-documenting. Builtin/
provider-covered plugins (clock/system/sleep/git/openai/ollama/duckduckgo/tavily) and fluxplane's
aggregator/generator surfaces (vision/websearch-aggregator/openapi) are explicit non-goals. Epic design:
[fluxplane-plugins-parity.md](designs/fluxplane-plugins-parity.md). Built in this order:

- **[D-12](stories/D-12-plugin-protocol-parity.md) — Plugin protocol parity extensions** · *core, leads.*
  Three additive host capabilities the missing plugins need: non-Bearer auth injection (Basic/header/query by
  purpose — Slice A), a guarded raw `conn.*` socket dialer (Slice B), and a `blob.*` store (Slice C). Clean
  extension of `flux.plugin.v1`; the dialer lives in flux-system. Gates D-15/D-16/D-17 and lets D-14 delete
  jira/confluence's hand-rolled base64. Design:
  [plugin-protocol-parity.md](designs/plugin-protocol-parity.md).
- **[D-13](stories/D-13-plugin-skill-command.md) — Generated plugin skill (`flux plugin skill`)** · *core.*
  Renders the installed plugin manifests into a trigger-activated `flux-plugins` SKILL.md + `references/` (the
  flux analogue of fluxplane's `fluxplane-plugin skill`); adds a frontmatter writer to flux-markdown.
  Independent of D-12. Design: [plugin-skill-generation.md](designs/plugin-skill-generation.md).
- **[D-14](stories/D-14-deepen-native-plugins.md) — Deepen the 8 native plugins** to their full fluxplane op
  sets (and drop the base64 hand-rolling). · *epic, per-plugin.*
- **[D-15](stories/D-15-observability-ai-plugins.md) — Observability & AI pack** (alertmanager, grafana,
  opsgenie, huggingface; HTTP, needs D-12 auth).
- **[D-16](stories/D-16-datastore-infra-plugins.md) — Datastore & infra pack** (sql, docker, aws; needs D-12
  conn + blob).
- **[D-17](stories/D-17-telephony-plugins.md) — Telephony pack** (asterisk, homer; serves downstream voice
  surfaces; asterisk needs D-12 conn).

### Subscription providers & cross-provider cost (epic)

flux already drives the two **subscription / passthrough** model backends — `claude` (Claude Max / Claude-Code
OAuth) and `codex` (ChatGPT/Codex OAuth) — by **reusing the desktop apps' tokens** and refreshing them, with no
full interactive OAuth2 login (that is the deliberate later stage). `flux-credentials` imports from
`~/.claude/.credentials.json` / `~/.codex/auth.json`, refreshes via a 0600 store, and `-m claude|codex/...`
routes to them; the `claude` (Bearer + `oauth-2025-04-20` + Claude-Code system prefix) and `codex` (Responses
API on the ChatGPT backend) providers are wired. This epic **hardens** that against the live-backend quirks,
makes codex's **websocket** the default transport (HTTP fallback), and adds the missing cross-cutting piece:
**full usage + cost tracking across all providers**. Epic design:
[subscription-providers-and-cost.md](designs/subscription-providers-and-cost.md). Built in this order
(C-03/C-04/C-05 parallelize — mostly disjoint files):

- **[C-03](stories/C-03-codex-provider-hardening.md) — Codex provider hardening** · *core.* `account_id` from
  the `id_token` JWT claims (real `auth.json` nests it there → missing `chatgpt-account-id` rejects), cache +
  reasoning token capture in the Responses usage, and reasoning continuity under `store:false`. Foundation for
  C-07.
- **[C-04](stories/C-04-claude-401-refresh.md) — Claude verify + force-refresh-on-401** · *core.* Refresh today
  is expiry-time-only; add a single 401→refresh→retry path on the credential/`NativeProvider` seam (shared by
  both subscription providers), and a hermetic verify of the claude request shape.
- **[C-05](stories/C-05-pricing-cost-model.md) — Cross-provider pricing & cost model** · *core.* Per-model
  per-tier rates (input/output/cache-write/cache-read/reasoning) + `cost(&Usage, model)`; a **built-in table
  overlaid by `~/.flux/pricing.toml`**; normalize the OpenAI Chat/Responses codecs to populate cache fields
  (they zero them today). Subscription spend is labelled as *equivalent metered cost*.
- **[C-06](stories/C-06-usage-cost-accounting.md) — Usage & cost accounting** · *core, needs C-05.* Per-model
  attribution + sub-agent rollup + a `cost_summary` event-log projection + a `flux usage` command + a server
  endpoint + cache-aware CLI/TUI/server output. The full "usage + cost across all providers" surface.
- **[C-07](stories/C-07-codex-websocket-transport.md) — Codex WebSocket transport (default)** · *core, needs
  C-03.* WS (`wss://chatgpt.com/backend-api/codex/responses`) as the primary path with transparent HTTP-SSE
  fallback (a transport seam in `NativeProvider`; auth on the tungstenite handshake, per the realtime provider).
  Upstream WS is experimental — the fallback is non-negotiable and test-covered.
- **[C-08](stories/C-08-full-oauth2-login.md) — Full OAuth2 login (codex PKCE)** · *core, later stage.* A
  flux-native `flux auth login codex` to parity with claude's PKCE login. Explicitly deferred — import + refresh
  cover the near term.

**Candidate phases (vision tail, in priority order):**
- **Crate consolidation** ✅ **all phases shipped** — shrank the workspace by merging coherent
  *same-layer* siblings (layering lint stayed green throughout). Phase 1 collapsed the five L1 provider
  crates into `flux-providers` (37→33). Phases 2–4 folded `flux-hooks`→`flux-plugin`,
  `flux-browser`+`flux-datasource`→`flux-capabilities`, `flux-context`→`flux-runtime`, and removed the
  dead `flux-integrations` (the workspace had drifted to 35; landed at **31**). `flux-auth` was kept
  standalone (caller identity ≠ tool capability). See
  [designs/crate-consolidation.md](designs/crate-consolidation.md).
- **Dogfood & harden** (tier 1) — drive flux's agentic mode on real coding work, capture friction as
  issues, and fix the top biters. Validates the daily-driver claim on real tasks.
  - **Generic `bash` is now opt-in** (off-by-default `shell` group; `enable_shell`/`FLUX_ENABLE_BASH`/
    `/shell`). Session-data analysis drove the dedicated-op coverage that makes default-off viable:
    `expr` extended with comparison/boolean/string ops, `now`/`cwd`/`sys_info`, `len`/`first`/`last`/
    `filter`, and the `go`/`node`/`python`/`make` toolchain ops. See
    [archive/designs/bash-replacement.md](archive/designs/bash-replacement.md).
  - **The flux-lang agent loop is now observable.** The self-hosted loop (`agent-loop.flux`) shipped
    transparent (zero surface change); these make it visible: `flux run --show-loop` reveals the
    `plan → run_plan → observe` machinery live, the REPL `/evidence` prints the audit trail, and
    `flux loop show`/`eject` reads or scaffolds the loop (`.flux/agent-loop.flux` override). See
    [agent-loop.md](agent-loop.md).
- **SDK + crates.io** (tier 2) — **P7 landed the bulk:** a **Rust eDSL** (`flux_lang::dsl`, re-exported
  as `flux_sdk::dsl`) whose builder primitives compile to the Flux-Lang AST — loops
  (`each`/`repeat`/`loop_for`/`race`) and control-flow (`match`/`route`/`fallback`/`timeout`/`budget`)
  first-class, all 36 node kinds covered (drift-guarded by `dsl_covers_every_node_kind`), authored in
  Rust then run through the existing `FlowClient` lifecycle. The public API is **stabilized**
  (`#![warn(missing_docs)]`, crate READMEs, three runnable no-API-key examples, crates.io metadata) and
  **publish-prepped** (the 16-crate closure carries versions; topo order + runbook in
  [`crates/flux-sdk/PUBLISHING.md`](../crates/flux-sdk/PUBLISHING.md); `cargo package` validated).
  A **recipe cookbook** (`flux_sdk::recipes` — routing/lookup/batch/resilience/fanout/dispatch/compose:
  reusable, parameterized flow builders) was then folded into the SDK and made **✅ reachable from the
  binary** via the **`flux preset`** subcommand (`list`/`help`, scaffold a recipe to a tree or JSON, or
  `--run` it through the envelope; op-resolution gates offline-runnability) — the DSL/recipes line is no
  longer library-only. **Blocked on a name decision before publishing:** the crate name `flux-core` is already taken on
  crates.io by an unrelated project — the namespace must be vanity-prefixed (`codewandler-flux-*`) or
  `flux-core` renamed (see the runbook §1). The real `cargo publish` is left to the maintainer (token +
  irreversible).
- **flux-lang evolution — ✅ shipped** (P0–P6 + flux-app): the agent-cognition layer landed — the
  artifact **prelude** (11 `Named` types), `ctx`/`ctx_append` context-pack nodes (36 node kinds),
  op-input JSON Schema, typed HIR with arg type-checking (`analyze::lower`), the **text parser**
  (`parse`/`format`) and **optimizer** (`optimize` + `PhysicalPlan` execution); the **`flux-cognition`**
  (L3) model-op pack and **`flux-app`** (L6) multi-agent runtime host (`flux run app.flux`,
  deny-destructive by default); and the **`flux-sdk` `FlowClient`** lifecycle. **P6** added **`await`
  cross-turn suspend/resume**, the **Tier-1 control-flow primitives** (`match`/`route`/`fallback`/
  `timeout`/`budget`), and polish (`fluxlang compile`, token-efficient `format_compact`, a deterministic
  thing resolver). See [designs/flux-lang-evolution.md](designs/flux-lang-evolution.md) and the
  [PRD status RTM](../crates/flux-lang/docs/STATUS.md). **P7** added the **Tier-2 control-flow
  primitives** — `scope` (RAII cleanup), `saga`/`compensate` (reverse-order unwind), `once`
  (at-most-once side effect), `checkpoint` (durable resume point) — on a narrow `DurableStore` seam
  (`FlowStore` folds them out of the append-only event log), plus a **dead-step optimizer pass**
  (drop read-only binds whose result is never used) and **common-subexpression elimination** (dedupe an
  identical read-only, deterministic call into a `Stage::Alias` — one dispatch, reused result).
  **P8** removed the language's top authoring friction: `bind` now accepts a `var` (`$b = $a` alias)
  or `lit` (`$x = 5`/`[1,2,3]`/`{…}`) directly, and two pure **value-template** nodes (`obj`/`list`)
  let a record/list assemble from variables (`return { ok: true, n: $count, intent: $x.intent }`) —
  42 node kinds. Remaining (optional): native `{k:expr}`/`[expr]` text spelling + a strict-JSON-schema
  vs. native-text **emission A/B** (measure planner accuracy before switching the model's surface);
  deeper optimizer passes (predicate pushdown, batch/model-call fusion); `checkpoint`∘`await`.

**Environment-gated (need a live key or external infra):**
- **Homebrew tap** — an auto-updating `brew install codewandler/tap/flux` formula via cargo-dist
  (`publish-jobs = ["homebrew"]` + `tap`/`formula` in `dist-workspace.toml`); needs a
  `HOMEBREW_TAP_TOKEN` PAT with push access to a `codewandler/homebrew-tap` repo.
- Switch `openai`'s default wire from Chat to Responses, verified with a live round-trip.
- `web_search` server tool; live token-count endpoint.
- Wire a real OIDC IdP behind the existing `OidcIdentity` seam (the multi-user platform tier).

**Deferred behind existing seams (add on concrete demand):**
- A `deno_core` / `rustyscript` hook backend (async / TypeScript / npm) behind the `PreToolHook` seam.
- A `chromiumoxide` CDP browser tool (navigate/screenshot; needs Chrome) behind `flux-capabilities`' `browser` module.

## Known divergences / decisions pending

Drift made visible, so it stops being silent. Each maps to a story on the
[board](stories/README.md):

- **Two turn loops.** The CLI/TUI/server run the pure-DAG `FlowEngine`, but the SDK's
  `flux_sdk::Client` still drives the classic `flux-agent::Agent` loop. Unify onto
  `FlowEngine`/`FlowClient` and retire `flux-agent::Agent` (ref
  [designs/flux-flow.md](designs/flux-flow.md) §11). → [A-01](stories/A-01-unify-flowengine.md).
- ~~**Crate consolidation phases 2–4**~~ ✅ done (35 → 31). → [C-01](stories/C-01-crate-consolidation.md).
- **crates.io publish** blocked on the `flux-core` name (needs a vanity prefix); deferred.
- **Self-improvement headline gain** still lacks a trials ≥ 3, grader-confirmed result.
  → [I-01](stories/I-01-headline-gain.md).
- **No cost tracking.** Per-turn token usage is captured + persisted, but there is no pricing layer and no
  aggregation/reporting, and the OpenAI codecs drop cache tiers. → [C-05](stories/C-05-pricing-cost-model.md)
  / [C-06](stories/C-06-usage-cost-accounting.md).
- **Codex transport is HTTP-SSE only** while the upstream codex client uses a websocket transport (with HTTP
  fallback). → [C-07](stories/C-07-codex-websocket-transport.md).
- **Subscription-provider login is import-only for codex** (claude has PKCE); full OAuth2 for codex is the
  deferred later stage. → [C-08](stories/C-08-full-oauth2-login.md).

## Backlog (product improvements)

- **Load skills from a user/global dir** (e.g. `~/.flux/skills`) in addition to the project
  `.flux/skills`, so global skills needn't be copied per-project. → [L-01](stories/L-01-global-skills.md).

## Direction

The through-line is **the LLM is not the runtime**: the model is a compiler front-end that emits a
Flux-Lang plan, and the deterministic engine runs it — **non-bypassable safety** is the hard
invariant that buys. Priority is **personal coding agent → reusable SDK → multi-user platform**. See
[vision.md](vision.md). The annotated original design & planning document (with full
milestone-by-milestone detail) is retained outside the repo by the author; this roadmap is the
in-repo canonical summary.
