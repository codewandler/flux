# Changelog

All notable changes to this project are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Kubernetes endpoint provider + agent-facing discovery ops (D-28).** The kubernetes plugin is now a
  discovery provider: `kubernetes.endpoint.discover` declares the products it can find and returns weak
  `EndpointCandidate`s — kubeconfig contexts → cluster endpoints, in-cluster Services/Ingresses → product
  endpoints (prometheus/loki/grafana/alertmanager), and crossplane/RDS Secrets → `postgres`/`mysql`
  endpoints carrying a `kubernetes/<ns>/<secret>/<key>` `credential_ref` (a location, never a value), with
  "latest namespace" selection. The broker now resolves a provider's real (namespaced) op name. New
  agent ops `endpoint.discover`/`select`/`info`/`list` (read-only, in an `endpoint` group surfaced when a
  `kubernetes` signal — `KUBECONFIG`/`~/.kube/config` — is present) let the model discover and select an
  endpoint as a weak reference; the agent never sees a secret. This wires the "connect to my latest
  namespace backend RDS" path.
- **Reference-based plugin IO + host-injected connect (D-27).** Plugin `http.do`/`conn.dial` now accept
  an `endpoint_ref` (named or discovered `@endpoint/<id>`); the host resolves it, composes the URL
  through the existing egress guard, and injects credentials host-side — the plugin and the model never
  see a URL with credentials. A discovered endpoint's `credential_ref` is materialized via the owning
  plugin's `secret.read` (e.g. a Kubernetes-scheme ref → the kubernetes plugin), gated **deny-by-default**
  by a `[endpoint] cross_plugin_credentials` operator grant + a first-use-approval seam + a
  `CrossPluginResolve` audit event, on both the HTTP-injection and raw-socket paths (gated by the real
  consumer). Raw-socket protocols that must speak auth in-band (Postgres SCRAM) receive the credential via
  a new gated `credential` capability — trusted plugin only, registered with the redactor, never the
  model. Inline `user:pass@host` URLs are split into an injected header. The `Redactor` now shares its
  value store across clones so a mid-run materialized secret is scrubbed everywhere.
- **Cross-plugin endpoint discovery broker (D-26).** Plugin manifests can declare `discovers: [products]`
  (and a `discover` capability); a new L5 `flux_capabilities::endpoint` broker fans a consumer plugin's
  `endpoint.discover` host call out to every provider plugin that declares the product, aggregates and
  ranks their weak-reference candidates, and commits them to the session `EndpointRegistry` — with a
  re-entrancy guard and a `ProviderInvoker` seam. `EndpointBrokerHostCaps` wraps the existing host caps
  (deny-by-default `endpoint.discover`), and the broker is wired into both `flux run` and `flux app run`.
  Discovery results are weak references only — never a resolved URL or a secret.
- **Scoped private-network egress, finished (D-20).** The 0.2.7 scoped model gained **per-endpoint**
  grant granularity (`PrivateNetConfig.endpoints`, keyed `"<plugin>:<endpoint>"`, merged with the
  plugin-level grant) and a **private-network-admit audit event**: a new `EventKind::PrivateNetAdmit`
  is recorded whenever the host admits a private/internal address under a scoped grant, via a new
  `flux_plugin::EgressAudit` seam (no flux-plugin→flux-events dependency) with the event-store-backed
  impl wired at the `flux` CLI. Pulled in as the prerequisite for the endpoint-discovery epic.
- **Endpoint reference model & registry (D-25).** The references-only spine of the endpoint-discovery
  epic: a new L0 `flux_secret::endpoint` schema (`EndpointRef`/`EndpointCandidate`/`EndpointRecord` weak
  references that carry a `credential_ref` location, never a secret; and a host-only `ResolvedEndpoint`
  with no serializer), a `flux_plugin::ReferenceResolver` trait seam, and
  `flux_capabilities::endpoint::{EndpointRegistry, StaticResolver}` — a session registry with
  `put`/`resolve`/`list`/`replace_owned` and `~/.flux/endpoints.toml` persistence (weak refs only), plus
  a static config-binding resolver. No discovery wiring yet (that lands with D-26/D-27).
- **Endpoint discovery & brokerage epic (planning).** Filed the design for cross-plugin endpoint discovery
  ([docs/designs/endpoint-discovery.md](docs/designs/endpoint-discovery.md)) and stories D-25–D-30: a
  references-only plugin-IO model (a plugin operation deals only in host-managed endpoint/credential
  references — never env vars, raw secrets, or credential-bearing URLs), a host fan-out discovery broker, a
  kubernetes endpoint provider, and an endpoint-lifecycle CLI. Reverses the `.dex`-style endpoint-registry
  deferral from D-10/D-12; D-20 (scoped private-net egress) is pulled in as a hard dependency. Design +
  backlog only — no code yet.

### Fixed

- Preserved kubeconfig access in `scripts/smoke-plugins.sh` when the Kubernetes plugin smoke uses an
  isolated `HOME`, so `kubectl` sees the same configured cluster as the caller.

## [0.2.7] - 2026-06-30

### Changed

- Clarified the root docs split: `AGENTS.md` is now explicitly the operating contract for coding agents,
  while `README.md` gives humans a faster product overview, common entry points, and contributor map.
- Hardened pre-push security edges: plugin HTTP callbacks now require declared host allow-lists, private
  network access is scoped per caller and per plugin config grant, server turns are serialized on shared
  engines, unauthenticated non-loopback server binds are refused, and persisted composite-op loading now
  goes through guarded `flux-system` paths.

## [0.2.6] - 2026-06-30

### Changed

- **Agent-loop retry efficiency (I-02).** Cargo wrapper ops now normalize model-supplied duplicate scope
  and warning flags before invoking Cargo, preventing failures like duplicate `--workspace` or
  `--all-targets`. The loop retry breaker also fingerprints deterministic cargo duplicate-argument and
  stale `edit` anchor failures, so semantically repeated failures are escalated even when the full
  transcript changes.

## [0.2.5] - 2026-06-30

### Added

- **Generated Flux skills (L-07).** Added `flux skill [cli|lang|plugin|ops]` to render Claude-format
  skills for Flux itself, plus `flux skill --install` / `flux skill <type> --install` to write a root
  routing skill and focused section skills (`flux-cli`, `flux-lang`, `flux-plugin`, `flux-ops`) into
  project `.flux/skills` or user-global `~/.claude/skills` with `--global`. The renderers are grounded
  in live sources of truth: Clap for CLI commands, `flux_lang::skill::render()` for Flux-Lang,
  `ToolRegistry`/`OpRegistry` plus group metadata for operations, and installed plugin manifests for
  plugin ops. Project-local `.claude/skills` is now loaded by default after `.flux/skills`; the legacy
  `flux plugin skill` command remains as a plugin-section alias.
- **Public Docusaurus docs site (L-05).** Added a standalone `website/` Docusaurus project for the public
  docs at `https://codewandler.github.io/flux/`, distinct from the repository's internal contributor and
  design docs. The initial public docs cover getting started, core concepts, CLI/provider basics,
  Flux-Lang text syntax, execution semantics, AST reference pointers, examples, SDK `FlowClient`, plugin
  authoring, and configuration defaults. A GitHub Pages workflow builds the site on PRs and deploys `main`.
- **Flux-Lang composite ops (L-04).** Native `.flux` modules can now declare reusable `op` definitions:
  typed, module-local composite operations implemented as ordinary Flux-Lang bodies. Composite calls are
  catalog-visible, analyze like normal ops, execute in a scoped symbol frame (params/locals do not leak),
  and every inner real op still dispatches through the existing authorization/approval/redaction/guarded-IO
  envelope. SDK `FlowClient`, `flux flow run`, and `flux-app` install module composites; validation rejects
  recursion, `await` in composites, duplicate/conflicting names, and understated transitive risk/effects.
  Added the shell-group-gated `proc.run` op for argv-only process execution through `flux_system::System`.
- **Agent-registered composite ops (L-06).** Added the model-facing root op `op.register`, letting an agent
  register exactly one validated Flux-Lang composite op into `turn`, `session`, `project`, or `global` scope.
  Session definitions persist in the flow store; project/global definitions are normalized `.flux` source
  written through guarded `System` paths (`.flux/ops/<name>.flux` and `@global_ops/<name>.flux`). Registered
  ops are folded into later planner/execution catalogs and still run as scoped composites, so every inner real
  op continues through `Executor::dispatch`.
- **Single guarded process-spawn path + plugin authoring guide (D-22).** All OS-process creation now funnels
  through one `flux_system::System` constructor (`build_command`: argv-only, workspace-pinned cwd, env
  **cleared** to a minimal non-secret allow-list) — `run_with_env`, the streamed runner, `spawn_background`,
  and a new **`spawn_interactive`** (piped stdin/stdout, inherited stderr, `kill_on_drop`) all layer only
  their own stdio on top. `PluginHost::spawn` now launches plugins through `spawn_interactive`, so the
  **plugin process is env-cleared**: a plugin can no longer read the host's secrets via `std::env`, closing a
  bypass of the deny-by-default `secret` gating (regression test `plugin_cannot_read_host_env`).
  `flux-runtime`'s git-context call is routed through `System::run` too (gaining a wall-clock timeout). New
  **`plugins/AUTHORING.md`** — the canonical plugin guide (lifecycle, the host-does-all-IO invariant, the
  capability set, the rules) — linked from `AGENTS.md` and `plugins/README.md`.
- **One daemon command for served agents (D-23).** The standalone `flux serve` command is removed; use
  `flux app run --serve <addr> --yes` to expose the built-in coding agent over the same REST/SSE/A2A HTTP
  surface. A `.flux` program can declare an `a2a` channel, and `flux app run <program.flux> --serve <addr>`
  injects an ad-hoc A2A channel for a sole-agent program. The HTTP implementation is shared through
  `flux-server`, including bearer-token enforcement for non-loopback binds.
- **Provider schema + CLI daemon hardening (D-24).** Added `flux_spec::tool_input_schema` for
  schemars-derived tool input contracts and switched the planner's synthetic `emit_plan`/`ask_user` tools
  to typed schemas; `emit_plan` now advertises the full `DraftAst`/`Node` JSON Schema instead of a bare
  object placeholder. `flux plugin call <plugin> <op>` now accepts short op names by resolving them against
  the plugin manifest's fully qualified operation names, while still preserving explicit full names.
  `flux-server` and `flux app run` channel hosts now honor SIGTERM as well as Ctrl-C, and `flux tui` fails
  early with a clear error when stdin/stdout are not real terminals.
- **Plugin host protocol — managed background processes + binary HTTP body (D-14 enabler).** Two additive
  capabilities on `flux.plugin.v1`, extending the host the way D-12 added auth/conn/blob:
  - **Managed background processes** — `process.spawn`/`read`/`status`/`kill`, a per-session registry in
    `SystemHostCaps` beside `conns`/`blobs` (so a process started in one op call is stopped/queried in a
    later one — one host instance is shared across a plugin's tool calls). Backed by a new
    `flux_system::System::spawn_background` returning a `ManagedChild` (piped stdout/stderr drained into
    capped buffers, `kill_on_drop`). Same safety envelope as `run_with_env`: argv-only, env **cleared** +
    minimal allow-list + caller overrides, workspace-pinned cwd; `process.spawn` is gated by the manifest's
    `process` allow-list exactly like `process.run` (deny-by-default). This is what lets a plugin host a
    long-lived `kubectl port-forward`.
  - **Binary HTTP body** — `http.do` accepts a base64 `body_b64` request body and, with `response_binary:
    true`, returns the raw response bytes as `body_b64` (16 MiB cap, no char-truncation). host-kit exposes
    `Host::process_*` and `Host::http_bytes`. Byte-exact file upload **and** download (was lossy through the
    UTF-8 `String` body before).
- **fluxplane-plugins parity — the 8 native plugins at full op + behavioural parity (D-14).** Brought every
  plugin in the in-repo `plugins/` pack to its fluxplane counterpart's operation set (**+~160 ops**) *and*
  to faithful behaviour (not just op names):
  - **gitlab 6 → 64** — full MR review/diff/discussion workflow, branches, repo files/tree/commits/tags,
    CI/CD, releases + links + changelog, issues, snippets, `repository.archive` → host blob; `mr.diff.lines`
    uses real **regex** matching (matching the reference), `mr.merge` sends the modern `auto_merge` (not the
    deprecated field), `pipeline.create` validates its `variables`.
  - **slack 5 → 30** — edit/delete, threads, search, reactions, bookmarks, presence, emoji; **`mentions`**
    does the reference's replied/acked/pending thread classification and **`unreads`** uses real `last_read`
    cursor math; files upload/download **byte-exact** via `http_bytes`.
  - **kubernetes 5 → 24** — renamed `k8s.*` → `kubernetes.*`; full inventory, scale/restart/history,
    logs/events, secret.read, endpoint.discover, one-shot `pod.exec`; **port-forward start/stop/list run on
    the host managed-process capability** (spawns `kubectl port-forward`, parses the readiness line for the
    real local port, kills on stop).
  - **jira 3 → 21** / **confluence 3 → 15** — full issue/page CRUD + transitions/comments/attachments/
    links/user-search; **attachments byte-exact** via `http_bytes`; jira ports the markdown→ADF renderer and
    the transition-selection scorer faithfully; confluence renders storage↔markdown with `body_format`.
  - **prometheus 4 → 8** (series/targets/rules/alerts; rejects empty `query`), **loki 3 → 5** (metric,
    recent_logs; Basic + `X-Scope-OrgID` tenant header, auth purposes named per the reference), **websearch**
    `provider.list` + provider selector. Each HTTP plugin has an `index.build` op driving its datasource
    contribution exhaustively (`{indexed: n}`).
  - **Auth re-port:** jira/confluence drop the hand-rolled Basic-auth base64 — primary is **Bearer
    `api_token` via the `cloud_id` gateway** (`api.atlassian.com/ex/jira|confluence/{cloud_id}`, the
    fluxplane reference), with **Basic (email:token) retained as a configurable fallback**, selected per
    request from the configured env. The host injects both schemes (D-12 `AuthScheme`); no base64 in-plugin.

  Op shapes + behaviour were ported from the fluxplane manifests/clients; every op keeps a MockHost unit
  test (incl. non-UTF-8 byte round-trips and managed-process lifecycle), and the nested `plugins/` workspace
  gate stays green (203 plugin tests). Deeper fidelity closed too: confluence full storage↔markdown
  conversion, prometheus typed `query`/`query_range` results, loki SHA1 entry ids + RFC3339Nano timestamps,
  slack `mentions`/`unreads` `since`/`unhandled`/`tickets`. New **plugin-local** dependencies: `regex`
  (gitlab `diff.lines`), `pulldown-cmark` + `quick-xml` (confluence storage↔markdown), `sha1` + `time`
  (loki). See [docs/designs/fluxplane-plugins-parity.md](docs/designs/fluxplane-plugins-parity.md).
- **fluxplane-plugins parity — the 9 missing portable plugins are native (D-15/D-16/D-17).** Added the
  remaining single-vendor plugins from the fluxplane pack: **alertmanager** (5 ops), **grafana** (20),
  **opsgenie** (8), **huggingface** (9), **aws** (11 read-only ops via the host-managed `aws` CLI),
  **docker** (33 core Docker Engine REST ops over the guarded Unix `conn.*` stream), **sql** (6 PostgreSQL
  read/introspection ops over `host-kit::ConnStream`), **asterisk** (8 AMI ops over guarded TCP), and
  **homer** (8 Homer SIP-capture ops with JWT login + blob-backed PCAP export). The plugin smoke script now
  has skip-safe entries for the new pack. Honest residuals are documented: Docker's streaming/hijack ops need
  a later stream design, SQL live Postgres interop still needs an env-gated smoke, MySQL is a clear unsupported
  error, and SQLite is unsupported by design because plugins have no host file capability.
- **Reusable A2A server protocol — `flux_a2a::server` (D-03).** Lifted the duplicated A2A server-side
  logic out of `flux-server/src/a2a.rs` into a reusable, **axum-free** module on the L1 `flux-a2a` crate:
  the `A2aTurn` runner seam, `dispatch` (`message/send` → a completed `Task`; JSON-RPC errors),
  `agent_card`, `extract_text`/`extract_context_id`, `rpc_ok`/`rpc_err`, `now_rfc3339`, and
  `status_update_value` (the `message/stream` frame `result`). `flux-server` now consumes it (keeping its
  axum routes + SSE + engine wiring); downstream services can mount the same module instead of
  re-implementing the protocol. Current spec only (no `tasks/send`); `flux-codegate` confirms `flux-a2a`
  stays L1 (only new dep: `async-trait`). Serves downstream A2A consumers.
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
  `account_streams` return only one tenant's runs — so a downstream multi-tenant service replays per-account
  transcripts as *projections over the same log* (via the unchanged `conversation`/`turns` projections), not a parallel store. Additive, idempotent
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
    a seed. One-shot — genuine cross-turn `await` flows stay on `FlowEngine`. Serves downstream
    behaviour-runner and preset-framework consumers. Hermetic example: `examples/parameterized_flow.rs`.
- **Realtime voice-to-voice as a first-class provider (D-06).** A **sibling, session-oriented** model seam
  beside the half-duplex `Provider`, so a full-duplex speech-to-speech model (OpenAI Realtime) is a flux
  provider whose tool calls run through the **same `Executor` safety envelope** as a text turn — declared
  **once** from the live `ToolRegistry` (no more model-facing-vs-runtime double declaration). Built as
  modules (zero new crates):
  - `flux_core::audio` (L0) — `AudioFormat`/`AudioEncoding`.
  - `flux_provider::realtime` (L1) — `RealtimeProvider`/`RealtimeSession`/`RealtimeEvent`/`RealtimeConfig`/
    `TurnDetection`; events carry decoded bytes and plain strings only (the seam never names a runtime type).
  - `flux_providers::realtime` (L1, behind the **`realtime`** Cargo feature) — the OpenAI-Realtime WebSocket
    impl ported from a downstream realtime client (GA shape; one `openai_realtime(...)` constructor;
    idempotent barge-in cancel).
  - `flux_flow::voice` (L3) — `VoiceSessionDriver` (routes `ToolCall` → `Executor::dispatch` off the audio
    loop; debounced `create_response`; idempotent barge-in), `VoiceSink`, `tool_defs_from_registry`, plus a
    Phase-2 *engine-owned-turns* spike (`run_flow_turns` + a `VoiceTurnHandler` seam — a flux-lang flow
    drives turns; per-turn `run_turn`, not yet cross-turn `await`).
  - `flux_sdk::flow::FlowClient::run_voice_session(...)` (L6) — the one-call consumer seam (mirrors
    `with_sub_agents`). Audio resampling stays in the consumer/channel (model-native format only). The
    downstream consumer rewiring lands outside this repo as a follow-up.
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
  sub-agent primitive — single-tenant and wired only in the CLI — now has the seams downstream
  multi-tenant SDK consumers need:
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
