# Changelog

All notable changes to this project are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Reusable A2A server protocol — `flux_a2a::server` (D-03).** Lifted the duplicated A2A server-side
  logic out of `flux-server/src/a2a.rs` into a reusable, **axum-free** module on the L1 `flux-a2a` crate:
  the `A2aTurn` runner seam, `dispatch` (`message/send` → a completed `Task`; JSON-RPC errors),
  `agent_card`, `extract_text`/`extract_context_id`, `rpc_ok`/`rpc_err`, `now_rfc3339`, and
  `status_update_value` (the `message/stream` frame `result`). `flux-server` now consumes it (keeping its
  axum routes + SSE + engine wiring); downstream's `managed-agents` re-homes its `channel-a2a` onto the same
  module — one definition for the A2A *server* the way `flux-a2a`'s types give the *wire* one. Current
  spec only (no `tasks/send`); `flux-codegate` confirms `flux-a2a` stays L1 (only new dep: `async-trait`).
  Serves managed-agents **E-02**.
- **`.flux` does all of it — native-text module declarations (L-03).** A `.flux` app is now written
  entirely in native flux-lang: `agent` / `channel` / `datasource` / `trigger` / `journey` declarations
  (each with an indented `key value` settings block) plus the journey flows, replacing the JSON program
  manifest. Settings are flux-lang values (strings/numbers/bools/lists/records, bare identifiers coerce to
  strings); `channel`/`datasource` default `kind` to the decl name. **Secrets are references, never
  inline** — `secret "ENV_NAME"` lowers to a `{"$secret":…}` marker in the pure parser and is resolved
  from the environment at load by the host (`flux_app::resolve_secrets`); a missing var errors naming the
  var, not the value. This is the **single** secret mechanism — the channel adapters' former
  `"secret:env/KEY"` string convention was removed (token fields now read the host-resolved value), with
  the marker shape owned by L0 (`flux_lang::program::{SECRET_KEY, secret_marker, as_secret_ref}`) and
  `build_channels` guarding against an unresolved marker so the resolve-before-consume order can't be
  skipped silently. The host builds the knowledge backend from the declared `datasource`s
  (markdown/openapi ingesters). **Clean cutover:** `flux_lang::program::Module::parse_str` now parses
  native text (`from_json`/`PROGRAM_KEYS` deleted); `flux app run` and `flux flow run` load native-text
  `.flux` (the latter still sniffs a leading `{` for checked-in JSON `DraftAst` loops). The bundled
  examples (`crates/flux-app/examples/{hello,support-bot}.flux`, `examples/channels-app.flux`) are
  rewritten in native text. No new node kinds; `flux-codegate` layering unchanged. See
  [docs/designs/native-text-modules.md](docs/designs/native-text-modules.md).
- **Tenant/agent context envelope on the event log (D-02).** `flux-events` runs can now carry an optional,
  stream-level `EventContext { account, agent_id, agent_version, correlation_id }`, set once at creation via
  `EventStore::create_session_with_context` (the 1-arg `create_session` delegates with an empty envelope, so
  the single-tenant path and every existing call site are unchanged). The context is surfaced on
  `StoredEvent` / `SessionInfo` / `SessionSummary`, and new account-scoped reads `list_for_account` /
  `account_streams` return only one tenant's runs — so a downstream multi-tenant service (managed-agents R-04 run
  persistence + the M4 transparency surface) replays per-account transcripts as *projections over the same
  log* (via the unchanged `conversation`/`turns` projections), not a parallel store. Additive, idempotent
  column migration; the `events` table and all projections are untouched. See
  [docs/designs/tenant-event-substrate.md](docs/designs/tenant-event-substrate.md).
- **Integration-stack hardening (C-02).** Three follow-ups over the shipped D-07/D-08/D-09/D-10 stack:
  - **`flux plugin call <name> <op> [json]`** — invoke one declared op of an installed plugin directly
    (spawns the binary via `PluginHost`, drives it through the `DatasourceHostCaps` bridge), plus
    **`flux plugin install [dir]`** to register every built `flux-plugin-*` binary in one shot. A new
    `plugins` CI job now builds/tests/clippy/fmt the nested `plugins/` workspace (previously untested
    because it's excluded from the root workspace).
  - **Semantic / embeddings retrieval** behind the D-07 `Embedder` seam, feature-gated (`embeddings`, off
    by default): an `OpenAiEmbedder` over an OpenAI-compatible `/v1/embeddings` (config from env, via the
    runtime-free `ureq` client + the `guard_url` SSRF check) and a `SemanticIndex` decorator over any
    `DatasourceBackend` that reranks keyword candidates by a blend of keyword score + query/record cosine
    similarity. The default build, keyword path, and gate are unchanged; the rerank logic has a hermetic
    stub-embedder test.
  - **`scripts/smoke-plugins.sh`** — a live, env-gated plugin smoke (skip-not-fail) driving `flux plugin
    call` against real vendor APIs for whichever keys are present; documented in the roadmap's standing
    pre-release gate.

- **Integration plugin pack — 8 native plugins (D-08).** A new in-repo `plugins/` cargo workspace
  (excluded from the root flux gate so vendor surface stays out of it) with eight subprocess plugins on a
  shared **`host-kit`** SDK: `websearch` (Tavily + DuckDuckGo), `gitlab` (projects/MRs/issues/pipelines),
  `jira` (issue search/show, projects), `confluence` (search/page/spaces), `kubernetes`
  (namespaces/pods/deployments/logs/events via `kubectl`), `loki` (LogQL), `prometheus` (PromQL/alerts/
  targets), `slack` (post/history/channels/users/thread). Plugins do **no privileged IO of their own** —
  every side effect is a host-capability callback (http with bearer-injection / process / secret-by-purpose
  / datasource-record contribution); list/search ops contribute `flux-datasource` records that reach the
  D-07 index via the L5 `DatasourceHostCaps` bridge. Hermetic `MockHost` tests throughout. See
  [`plugins/README.md`](plugins/README.md) and [`docs/designs/integration-plugins.md`](docs/designs/integration-plugins.md).
- **Process-plugin protocol — manifest + host-capability enrichment (D-10).** `flux-plugin`'s manifest
  is now the single host-introspected source of truth: it gains `auth` (auth-by-purpose), `datasources`
  (shared `flux-datasource` `Declaration`s a plugin contributes), and `endpoints` (env-resolved base URLs);
  `OperationSpec` gains `idempotency` + `secret_purposes` (reusing flux's own `Effect`/`Risk` vocabulary,
  not a ported access enum). `SystemHostCaps` grows `with_manifest`, secret-by-purpose resolution, full
  HTTP (method/headers/body + bearer injection), and `endpoint` resolution. The transport was already a
  single unified Request/Response frame (no `target` field), so this is an additive enrichment, not a
  cutover. A new L5 **`DatasourceHostCaps`** (in flux-capabilities) services a plugin's
  `datasource.records`/`search`/`get` against the D-07 index. See
  [`docs/designs/process-plugin-protocol.md`](docs/designs/process-plugin-protocol.md).
- **Knowledge datasource — a real RAG layer (D-07).** A new L0 **`flux-datasource`** crate holds the
  shared record/retrieval schema (`Record` addressable by `(source, entity, id)`, `Declaration`/
  `EntitySchema`, and the `Search`/`Get`/`List`/`Relation`/`BatchGet` I/O types) — so the knowledge index
  and (future) integration plugins agree on one shape. `flux-capabilities::datasource` is rebuilt onto it:
  a **`DatasourceBackend`** trait with two impls — the in-memory `MemoryBackend` (default, keyword/TF) and
  a persistent **`SqliteBackend`** (a `records` table + an FTS5 virtual table over title+body, ranked by
  the built-in `bm25()`, WAL) — five agent-facing retrieval ops (`search`/`get`/`list`/`relation`/
  `batch_get`, registered via `register_datasource_ops`), markdown + OpenAPI ingesters
  (`ingest_markdown`/`ingest_openapi`), `reindex`/`freshness`, and an unwired `Embedder` (semantic) seam.
  The CLI's `search` is unchanged for users; the model also gains the four new verbs. See
  [`docs/designs/datasource-rag.md`](docs/designs/datasource-rag.md).
- **Agentic channel target — `trigger.agent` (D-09, mechanism).** A channel trigger naming an `agent`
  now wakes a `FlowEngine` agent turn (the model drives RAG + granted tools) instead of a journey, with
  per-thread `(agent, conversation) → EventStore` session memory and grants from the `AgentDecl`'s `tools`
  under a headless `DenyApprover`. Reuses the existing `TriggerDecl.agent` field; the journey route is
  unchanged. **Registry wiring (completing D-09):** a non-breaking `App::with_tools` seam + the CLI's
  `flux app run` now index workspace docs into a shared `DatasourceBackend` and register the D-07
  datasource ops + every discovered D-08 plugin's tools into the host registry (plugin-contributed records
  land in the same index via `DatasourceHostCaps`) — so the agent target drives RAG `search` + the granted
  integration ops over one knowledge index. See [`docs/designs/agentic-channel-target.md`](docs/designs/agentic-channel-target.md).
- **Parameterized flow execution — the behaviour-runner seam (D-01).** Run a *stored, validated* Flux-Lang
  flow **per invocation** with input values injected at call time, instead of re-compiling from natural
  language or baking inputs into the AST. Two thin `flux-sdk` additions over a new `flux-flow` store
  primitive — modules, zero new crates:
  - `FlowStore::seed(session_id, name, value)` (`flux-flow`) — pre-bind a named input so a flow's `$name`
    resolves to it before the run (`put_value` via `Value::from_json` + `bind` as `Hidden`, so a seed
    resolves for the interpreter but stays out of the model-facing `view`).
  - `FlowClient::parse(text)` — deterministic text → AST (wraps `flux_lang`'s parser; **no** provider
    round-trip, the non-NL partner of `compile`).
  - `FlowClient::execute_with(ast, inputs)` + `run_flow(text, inputs)` — execute a flow with `inputs`
    seeded as `$vars`, through the **same `Executor` safety envelope** (seeding injects *data*, never a
    capability). Each call runs against a **fresh store** (per-run isolation); a flow-local `bind` shadows
    a seed. One-shot — genuine cross-turn `await` flows stay on `FlowEngine`. Serves managed-agents R-01
    (behaviour runner) + A-03 (presets as flows). Hermetic example: `examples/parameterized_flow.rs`.
- **Realtime voice-to-voice as a first-class provider (D-06).** A **sibling, session-oriented** model seam
  beside the half-duplex `Provider`, so a full-duplex speech-to-speech model (OpenAI Realtime) is a flux
  provider whose tool calls run through the **same `Executor` safety envelope** as a text turn — declared
  **once** from the live `ToolRegistry` (no more model-facing-vs-runtime double declaration). Built as
  modules (zero new crates):
  - `flux_core::audio` (L0) — `AudioFormat`/`AudioEncoding`.
  - `flux_provider::realtime` (L1) — `RealtimeProvider`/`RealtimeSession`/`RealtimeEvent`/`RealtimeConfig`/
    `TurnDetection`; events carry decoded bytes and plain strings only (the seam never names a runtime type).
  - `flux_providers::realtime` (L1, behind the **`realtime`** Cargo feature) — the OpenAI-Realtime WebSocket
    impl lifted from the managed-agents `realtime` crate (GA shape; one `openai_realtime(...)` constructor;
    idempotent barge-in cancel).
  - `flux_flow::voice` (L3) — `VoiceSessionDriver` (routes `ToolCall` → `Executor::dispatch` off the audio
    loop; debounced `create_response`; idempotent barge-in), `VoiceSink`, `tool_defs_from_registry`, plus a
    Phase-2 *engine-owned-turns* spike (`run_flow_turns` + a `VoiceTurnHandler` seam — a flux-lang flow
    drives turns; per-turn `run_turn`, not yet cross-turn `await`).
  - `flux_sdk::flow::FlowClient::run_voice_session(...)` (L6) — the one-call consumer seam (mirrors
    `with_sub_agents`). Audio resampling stays in the consumer/channel (model-native format only). The
    managed-agents rewiring lands in that repo as a follow-up.
- **Event-trigger channels — background agents woken by events (D-04).** A new `flux-channels` (L6) crate
  lets a `.flux` **program** be woken by external events: a cron schedule, an inbound webhook, or a Slack
  mention. Channels are declared in the program as ordinary `ChannelDecl`s and run by the **app runner** —
  `flux app run <program.flux>` (a new explicit subcommand; `flux run <app.flux>` routes through the same
  path). Each channel fires a bus event **under its own name**; a `trigger { on: "<channel name>", run:
  "<journey>" }` routes it to a journey via the existing `App::deliver` → triggers → journeys path (the
  event payload is seeded into the journey's flow store). flux-app is unchanged — the heavy adapter deps
  (`axum`, `cron`/`chrono`, feature-gated `slack-morphism`) live only in `flux-channels`, which depends on
  flux-app.
  - **schedule** (`kind = "schedule"`): full cron (5-field crontab **or** 6/7-field seconds-first) +
    `on:"startup"`; UTC, fire-and-forget.
  - **webhook** (`kind = "webhook"`): an axum server per channel; `POST` delivers the JSON body and
    replies with the journeys' results, or `202` when `async = true`; optional bearer token, **required**
    for a non-loopback bind (mirrors flux-server).
  - **slack** (`kind = "slack"`, feature `slack`): socket-mode mentions/messages → delivery; posts the
    journeys' result back to the thread; `allow_users`/`allow_channels` policy; tokens via `secret:env/…`.
  - Deliveries are **serialized** (`App::deliver` drains the broadcast bus's cascades, so concurrent
    deliveries would double-process via fan-out); journeys themselves run on independent per-run stores.
    10 hermetic tests + 3 feature-gated Slack unit tests; `examples/channels-app.flux`. See
    [`docs/designs/event-trigger-channels.md`](docs/designs/event-trigger-channels.md).
- **Sub-agents are production-hardened for multi-tenant consumption (D-05).** The `flux-orchestrate`
  sub-agent primitive — single-tenant and wired only in the CLI — now has the seams a downstream service
  (managed-agents R-03/A-05) needs:
  - **SDK seam.** `FlowClient::with_sub_agents(SubAgents { … })` registers the `task` tool and installs
    the spawner into every run's context, so a consumer drives sub-agents without re-assembling the
    executor/registry/context by hand. `SubAgents::into_spawner` is the single construction path; the CLI
    refactors onto it (unchanged behaviour). Hermetic `flux-sdk` example `sub_agent.rs` (mock, no API key).
  - **Lifecycle limits.** Configurable `SpawnLimits { max_iterations, max_tokens, wall_clock }`; the
    wall-clock deadline **fires the child's cancel token** (cooperative, valid-history termination) rather
    than dropping the future mid-turn. The `task` tool now threads a child of the parent turn's cancel
    token (installed on `ToolContext` per turn by the engine) into the sub-agent — cancelling the parent
    cancels the child, fixing the old orphan-token behaviour.
  - **Pluggable approver.** `LocalSpawner::with_approver` lets a consumer approval-gate a sub-agent's
    mutations instead of the hardcoded auto-approve-non-destructive default.
  - **Audit threading.** `LocalSpawner::with_audit(EventStore)` persists a child's run (and its inner tool
    calls) into a shared tenant event store — the flow store now shares it — instead of a throwaway
    in-memory one. (The account/agent tag + explicit parent-session link land with D-02.)
  - **Ergonomics.** In-memory roles (`RoleRegistry::from_roles` / `FromIterator<Role>`) for programmatic
    consumers, and a depth-aware recursion guard (`with_max_depth`, default `1` keeps children leaves;
    `> 1` is a bounded opt-in). 8 new failing-first tests in `flux-orchestrate`. Isolation stays
    composition over the existing envelope — no new sandbox. See
    [`docs/designs/sub-agent-hardening.md`](docs/designs/sub-agent-hardening.md).
- **Per-turn token usage flows through the unified loop and renders in the CLI.** The planner's token
  counts are now captured from the provider stream (`compile_turn` returns them), accumulated across a
  turn's planner calls by the loop host (output summed; input/cache reflect the final, largest prompt
  so re-sent context isn't multiply-counted), and handed to `sink.turn_end` by the engine. The CLI's
  turn-end rule now shows **context-window occupancy, generated tokens, and — under prompt caching —
  cached tokens with the hit-rate** (e.g. `1 step · 90ms · ctx 1.4k · out 60 · cache 1.2k (87% hit)`);
  it stays clean (no all-zero noise) on offline `-m mock` turns. The SDK `Client` now also populates
  `TurnOutput.usage`. (Previously usage was dropped through the flux-lang loop — `turn_end(None)`.)
  Per-turn usage is now also **persisted** to the unified event store on the `TurnEnded` event
  (serde-default, so older logs still decode) and summed back by the eval runner (`load_usage` →
  `RunResult.tokens`), so `mean_tokens` becomes a real keep/revert tiebreaker for the self-improvement
  loop instead of always reading 0.
- **Stable-baseline self-improvement loop on the synthetic suite.** A new no-Docker loop —
  `examples/improve-synthetic.flux` (adapter `synthetic`, **trials = 5**, strict `score_compare`) with
  its runner `bench/run-synthetic-loop.sh` — drives the keep/revert loop against the 16 deterministic,
  objectively-graded coding riddles. The candidate's edits are measured via `gate_check`'s
  `target/debug/flux` rebuild (no musl), so a round is cheap enough to run trials ≥ 5 for a
  statistically clean gain. The flow is added to the loop's `PROTECTED` paths and the flow-validation
  test.
- **A2A client — `flux a2a <URL>`.** flux can now *consume* a remote A2A agent, not just expose one:
  `flux a2a <URL>` connects to any spec-conformant Agent-to-Agent agent and drives it from the CLI
  like a local agent — an interactive REPL, or a one-shot turn from command-line prompt words or
  piped stdin. Streamed replies render live (`message/stream`); Ctrl-C cancels a turn. A2A is an
  *agent* protocol, not a model protocol, so the client is thin: one user turn maps to one remote
  task (the remote runs its own loop), carrying the A2A `contextId`/`messageId`/`taskId` so a
  stateful remote keeps memory. A new leaf crate **`flux-a2a`** (L1) owns the spec wire types and
  the `A2aClient` (`fetch_agent_card` / `message/send` / `message/stream` / `tasks/get`), shared with
  the server.
- **Global, multi-format skills.** Skills are now discovered from the project's `.flux/skills` **and**
  the user-global dirs `~/.flux/skills`, `~/.agents/skills`, and `~/.claude/skills` (project wins on a
  name clash), so skills kept for other agents work in flux without per-project copies. Beyond the
  flux-native `triggers:` format, flux reads the cross-agent [Agent Skills](https://agentskills.io) /
  Claude format (`name` + `description`, no triggers); trigger-less skills activate on
  `name`/`description` keywords. A new **`flux-markdown`** crate (L0) owns frontmatter parsing
  (`serde_norway`) shared by `flux-skill` and `flux-orchestrate`, and wraps the `codewandler/markdown`
  crates for the TUI/CLI render paths behind off-by-default features. Activation is centralized in
  `flux_skill::active_for` (ranked + capped) and used by both the `flux-flow` and `flux-agent` loops.
- **Native tool calling for OpenRouter and local Ollama via the Anthropic Messages protocol.** Two new
  providers — `openrouter-anthropic` and `ollama-anthropic` — route through each gateway's Messages
  endpoint (`/api/v1/messages`, `/v1/messages`), so tool calls return as structured `tool_use` content
  blocks that can't leak as inline `<tool_call>` text the way some models do on the OpenAI Chat path.
  Both are built on a new shared **`flux-messages`** crate (wire schema + body/stream helpers + a
  per-`(provider, model)` quirks profile); `flux-anthropic` now composes the same core.

### Changed

- **Crate consolidation, phases 2–4 — workspace 35 → 31 crates.** Continuing the within-layer-merge
  pattern from phase 1 (the providers collapse), four thin single-consumer crates were folded into
  their same-layer neighbours (the `flux-codegate` layering lint stayed green throughout, one commit
  per phase): `flux-hooks` → a `hooks` module of **`flux-plugin`** (L4); `flux-browser` +
  `flux-datasource` → a new **`flux-capabilities`** crate with `browser`/`datasource` modules (L5);
  `flux-context` → a `context` module of **`flux-runtime`** (L2, additive to the published surface).
  **`flux-auth` was kept standalone** — caller identity is a distinct concern from tool capabilities
  (and `flux-runtime` must not depend on it). The orphan **`flux-integrations`** crate (Slack
  webhook/notify helpers, no consumers — never wired in) was **removed**; its code remains in git
  history for a future flux-server-native rebuild. No behavior change; all public entry points
  (`flux-plugin::hooks`, `flux_capabilities::{browser,datasource}`, `flux_runtime::context`) keep
  working.
- **One agent loop everywhere; the classic `Agent` loop is gone.** The SDK `Client` and the sub-agent
  spawner (`flux-orchestrate`) now run on the same `FlowEngine` flux-lang loop as the CLI/TUI/server —
  the legacy provider-native `flux-agent::Agent::run_turn` loop has been **deleted** (no fallback, no
  bridge). The `AgentSink` streaming trait moved to `flux-flow` (the engine crate). `flux-agent` is
  repurposed into the **Agent-pillar** crate: it owns **`AgentSpec`** (model, persona, skills, tool
  selection, permissions, settings) + `assemble`/`into_engine` (→ `FlowEngine`), keeps
  `DEFAULT_SYSTEM_PROMPT`, and absorbs the markdown `Role` agent-definition format (moved from
  `flux-orchestrate`). The SDK `Client` keeps its `TurnOutput` API.
- **A2A server speaks the current spec (breaking for A2A callers).** `flux serve`'s A2A endpoint
  moved from the early-draft `tasks/send` / `tasks/sendSubscribe` methods to the current spec's
  `message/send` / `message/stream`, with message parts keyed by `kind` (was `type`), a `Task` /
  `TaskStatusUpdateEvent` result shape built from the shared `flux-a2a` types, and SSE frames as
  plain JSON-RPC responses. The discovery card is now also served at `/.well-known/agent-card.json`
  (the `…/agent.json` path remains as an alias). The old draft methods are gone (clean cutover).
- **CLI: every entry point is now a subcommand (breaking).** The implicit top-level "run a turn"
  behavior and the top-level mode flags are gone, so `flux --help` shows only the command list plus the
  global `--color`. Migrate: `flux --serve <addr>` → `flux serve <addr>`, `flux --tui` → `flux tui`,
  `flux --plan "…"` → `flux plan "…"`, and a flag-led one-shot like `flux -m X "…"` / `flux --yes "…"`
  → `flux run -m X "…"` / `flux run --yes "…"`. `flux` with no arguments still opens the REPL; an
  unrecognized first word is now a clap "unrecognized subcommand" error instead of a bespoke refusal.
  The agent/turn flags (`-m`, `--yes`, `--max-tokens`, `-c`, …) live on the agent-path subcommands
  (`run`/`plan`/`tui`/`serve`) and no longer leak onto `sessions`/`loop`/`eval`/… help.

### Fixed

- **Self-improvement tag scalar is partial-credit-aware.** `SuiteScore::scalar()` now returns
  `round(mean_check_pass_rate * 1000)` instead of `round(pass_rate * 1000)`, so a candidate that
  improves only on sub-checks (partial credit) tags meaningfully (e.g. `improve-tbench-833`) instead of
  the misleading `improve-tbench-0`. Unchanged for binary adapters where `mean_check_pass_rate ==
  pass_rate` (e.g. the synthetic suite).
- **OpenRouter / local-model wire robustness (Messages path).** The shared parser tolerates the
  malformations real gateways and models emit: `null` usage counters, the OpenAI-style `[DONE]` stream
  sentinel, and tool-input JSON with trailing junk or an unterminated tail (off-by-one braces / open
  strings are repaired best-effort). Each has a regression test.
- **Inline tool-call recovery on the OpenAI Chat path.** When a model emits tool calls as text
  (`<tool_call>…</tool_call>` or `<function=…><parameter=…>`) instead of structured `tool_calls` —
  seen with GLM via OpenRouter and local models on multi-call turns — flux recovers them into
  `tool_use` blocks instead of stalling the turn on what looks like prose.

## [0.2.4] — 2026-06-25

Markdown rendering in the CLI — the highest-frequency dogfood readability gap (F2, [#1](https://github.com/codewandler/flux/issues/1)).

### Added

- **Assistant output now renders Markdown to the terminal.** The REPL, agentic mode, and `-p` one-shot
  feed streaming model text through the [`codewandler/markdown`](https://github.com/codewandler/markdown)
  renderer: on a TTY it redraws the reply in place as tokens arrive (headings, bold/italic, inline and
  fenced code with syntax highlighting, lists, links, GFM tables); piped (`flux … | cat`) it stays clean
  plain text with no escape sequences. Closes #1.

## [0.2.3] — 2026-06-25

Dogfood-driven fixes — surfaced by driving flux's own agentic mode on real coding tasks (see
[docs/archive/notes/dogfood-notes.md](docs/archive/notes/dogfood-notes.md)). flux completed every task; the friction was in the
tooling/UX layer.

### Fixed

- **`grep`/`glob` scoped to a file `path` now searches that file** instead of silently returning "no
  matches". The shared workspace walk (`System::walk_files`) only ever listed directories, so a file
  path produced an empty result — wasting agent turns and risking false "symbol not found" conclusions.

### Changed

- **The CLI shows a multi-line preview of tool output** (up to 12 lines, indented, with a `… (+N more
  lines)` note) instead of collapsing each result to a single 200-character line — so test output, grep
  matches, and file reads are actually visible. Display only; the model still receives the full result.

## [0.2.2] — 2026-06-25

Post-publish adoption — making the published release discoverable and installable from the front door.
No functional code changes.

### Added

- **README install section** — prebuilt-binary one-liners (shell + PowerShell, via the auto-tracking
  `releases/latest/download/…` URL) and a from-source fallback, plus CI / release / license status
  badges.

### Changed

- `docs/roadmap.md` refreshed to 0.2.1 status (cargo-dist binaries moved to *Delivered*; the
  0.2.0 daily-driver and 0.2.1 publish milestones recorded; dogfooding and SDK/crates.io noted as the
  next candidate phases).

## [0.2.1] — 2026-06-25

First publicly installable release — no functional changes from 0.2.0.

### Added

- **Prebuilt binaries + installer** — `flux` for Linux, macOS (x86_64 + aarch64), and Windows,
  with a `curl … | sh` / PowerShell installer, produced on each tagged release (cargo-dist).
- Dual-license files (MIT + Apache-2.0), contributor and security policies, and GitHub issue/PR
  templates.

## [0.2.0] — 2026-06-25

Daily-driver readiness: make flux a coding agent you actually reach for. Validated end-to-end against
a live provider (see `scripts/smoke-live.sh`).

### Added

- **Repo-aware context** — each turn's system prompt now includes the git working-tree state (branch,
  short status, recent commits, diff stat) and the project's shape (detected stack + top-level
  listing), so the agent no longer starts each turn blind.
- **A real REPL** — line editing, persistent history, reverse-search, and multiline input (reedline);
  a prompt-level Ctrl-C now clears the line instead of being swallowed.
- **Mid-session controls** — `/model <spec>` switches model/provider without restarting; `flux
  sessions` (and the REPL `/sessions`) list recent sessions with message counts, and `/resume <id>`
  reattaches to one.
- **A live-provider smoke gate** (`scripts/smoke-live.sh`) — exercises the real-provider
  message-shape paths the offline mock can't, as a standing pre-release check.
- Extended thinking is now visible in the REPL, and the usage line shows cache tokens when prompt
  caching is active.

### Changed

- **Stronger coding-agent system prompt** — an explicit inspect → smallest change → verify → summarize
  contract that honors `AGENTS.md`/`CLAUDE.md` conventions.
- **The `edit` tool is resilient to whitespace mismatches** — when the exact text isn't found it
  falls back to a whitespace-tolerant, line-aligned match (leading indentation must still match, and
  CRLF endings are preserved), and its errors now report occurrence line numbers / indentation hints
  instead of just failing.

## [0.1.1] — 2026-06-25

Security and robustness hardening from a full source-tree review. No API additions; existing
behavior is preserved except where it was unsafe.

### Security

- **Sandbox escape closed** — the workspace path guard now resolves symlinks component-by-component
  (including *dangling* symlinks, which `Path::exists()` skips), so a write through an in-workspace
  symlink pointing outside the root is rejected.
- **Subprocess isolation** — model-run commands no longer inherit flux's environment; only a minimal
  non-secret allow-list (`PATH`, `HOME`, …) is passed, so secrets like `ANTHROPIC_API_KEY` can't be
  read by a spawned command.
- **Plugin capability model** — host capabilities (`process.run`, `secret`, `http.do`) are now granted
  per-plugin from the manifest and checked on every call: a plugin can only run allow-listed programs,
  read allow-listed env keys, and reach the network if it declared `http`. Plugin operations also
  declare effects, so they pass through the authorization floor like built-in tools.
- **HTTP daemon authentication** — `flux --serve` now requires a bearer token (`FLUX_SERVER_TOKEN`) on
  every route except `/health`, and refuses a non-loopback bind without one (it auto-approves tools, so
  an open listener was remote code execution).
- **Authorization floor** — a policy grant marked `requires_approval` now forces the approval prompt
  even under a permissive permission rule (previously the `ApprovalRequired` decision was discarded).
- **Sub-agent scoping** — a role with `tools: []` now grants *zero* tools (an explicit empty allowlist),
  instead of inheriting the parent's full toolset.
- **SSRF guard** — web egress (`web_fetch` and plugin `http.do`) share one guard that resolves hostnames
  to IPs and blocks private/loopback/link-local/unique-local/CGNAT and IPv4-mapped ranges, plus internal
  hostnames — closing hostname- and IPv6-based metadata-endpoint access.
- **Secret redaction** — registered secrets are stored trimmed (so a trailing newline no longer defeats
  redaction) and punctuation-glued credential shapes (`api_key=sk-…`) are now scrubbed.
- **OAuth login** — the Claude PKCE login validates the callback `state` against the locally generated
  value (CSRF / code-injection guard).
- **Credential store** — written atomically with `0600` from creation (no world-readable window); a
  corrupt store is now an error instead of being silently overwritten (which wiped other providers'
  tokens).
- **Defense-in-depth** — policy path globs are normalized before matching (a `..` traversal can't widen a
  grant), subject-scoped deny rules fail safe to a prompt when no subjects are reported, unscoped writes
  force approval, user+project policy grants concatenate (a project policy no longer drops user grants),
  and `bash` permission parsing surfaces programs hidden behind `VAR=`/`$(…)`/backticks and flags
  unresolved shell expansion for approval.

### Fixed

- **Session shape** — reaching the per-turn iteration cap while still calling tools now appends a final
  assistant message, so the next turn isn't poisoned by an invalid user-after-user sequence (the third
  of the cancel/compaction/iteration-cap family).
- **Panics & DoS** — char-boundary-safe truncation of fetched/plugin bodies; `saturating_add` in the
  `read` tool's line range; byte→char offset in search snippets; caps on captured process output, framed
  plugin reads, and the OpenAI tool-call accumulator; and a wall-clock interrupt on JS hooks.
- **Provider accounting** — Anthropic input/cache token counts from `message_start` are preserved into the
  final usage chunk instead of being zeroed by the `message_delta`; OpenAI Responses truncation now maps
  to a `MaxTokens` stop reason.
- **Resilience** — `--continue` surfaces real SQLite errors instead of silently starting fresh; a failed
  worker in a parallel `/pd` wave no longer discards its completed siblings.

## [0.1.0] — 2026-06-24

First release.

### Added

- **CLI** — zero-config interactive REPL; `-p` one-shot; `--tui` (ratatui); `--agent` agentic mode under
  the safety envelope; `-c/--continue` to resume; `--serve` HTTP daemon; slash commands
  (`/help` `/tools` `/session` `/clear` `/pd` `/goal` `/loop`).
- **Providers** — `anthropic`, `claude`, `openai`, `codex`, `openrouter`, modeled as wire codec ×
  credential. `provider/model` routing, `flux auth status|login`, import of existing CLI credentials,
  PKCE login, JWT-exp token refresh, adaptive thinking + `--effort`, and Anthropic prompt caching.
- **Safety envelope** — default-deny authorization policy (grants over subjects × resources × actions
  with trust + scopes), layered permission rules with inline approval, destructive-operation escalation
  even under permissive rules, and secret redaction of tool output.
- **Guarded IO** — workspace-confined filesystem with symlink/escape rejection, argv-only process
  execution (no shell), and an SSRF-guarded web fetch.
- **Built-in tools** — `read`, `write`, `edit`, `bash`, `glob`, `grep`, `web_fetch`, `search`, `task`.
- **Sessions** — SQLite-backed, resumable, with automatic context compaction for long sessions.
- **Multi-agent orchestration** — sub-agent roles bounded by the inherited policy; `/pd` dependency-wave
  plan-and-dispatch (parallel workers); `/goal` (evaluator-driven autopilot); `/loop`.
- **Streaming & control** — token-by-token rendering in the CLI, TUI, and over server-sent events;
  in-TUI approval modal; Ctrl-C cancellation of an in-flight turn or command.
- **Extensibility** — JavaScript pre-tool hooks (observe/modify/deny); any-language subprocess plugins
  over a framed protocol with host-capability callbacks, projected as policy-gated tools, plus
  `flux plugin add|ls|pin|rollback`.
- **Skills** — markdown skills with triggers, activated and injected per turn.
- **Surfaces** — a high-level library SDK, an HTTP API + SSE server, and Slack/webhook integrations.
- **Identity** — local single-user default with an OIDC seam for multi-user deployments.
- **Tooling** — an architecture layering lint that fails on inner→outer crate dependencies, and CI
  running build/test/clippy/fmt.

[0.2.3]: https://github.com/codewandler/flux/releases/tag/v0.2.3
[0.2.2]: https://github.com/codewandler/flux/releases/tag/v0.2.2
[0.2.1]: https://github.com/codewandler/flux/releases/tag/v0.2.1
[0.2.0]: https://github.com/codewandler/flux/releases/tag/v0.2.0
[0.1.1]: https://github.com/codewandler/flux/releases/tag/v0.1.1
[0.1.0]: https://github.com/codewandler/flux/releases/tag/v0.1.0
