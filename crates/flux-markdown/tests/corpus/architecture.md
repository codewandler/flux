# flux — architecture

How flux is built and why. This is the canonical design reference; [AGENTS.md](../AGENTS.md) is the
day-to-day contributor contract, and [vision.md](vision.md) is the *why*.

The shape follows one idea: **the LLM is not the runtime.** Every turn the model is a compiler
front-end — it emits a typed Flux-Lang plan (a graph) or answers in prose; the deterministic
`flux-flow` engine executes that plan, node by node, through the safety envelope below. The model has
no directly-callable tools, so even a read is a plan node and a turn is always an auditable graph.
Everything that follows — strict layers, the envelope, providers, sessions — is the substrate that
inversion executes against. For the compact semantic model of the language itself, see
[Flux-Lang semantics](language.md).

## Shape: one workspace, strict layers

flux is a single Cargo workspace. Crates are stratified into layers; **a crate may depend only on
its own layer or lower** (enforced by a test in `flux-codegate`). Inert *contracts* (pure types, pure
evaluators, no IO) are separated from the *runtime* (execution + guarded IO), which is separated from
the *surfaces* (CLI/TUI/server/SDK).

| Layer | Crates | Role |
|---|---|---|
| **L0 contracts** (pure) | `flux-core` `flux-policy` `flux-secret` `flux-spec` `flux-config` `flux-evidence` `flux-skill` `flux-markdown` `flux-lang` | types, authorization, secrets, tool specs, config, evidence, skills, markdown/frontmatter, the Flux-Lang language + reference interpreter (effects injected via traits) |
| **L1 providers** | `flux-provider` `flux-providers` `flux-credentials` | the `Provider` abstraction + the concrete clients (`flux-providers` modules: `messages` core, `anthropic`, `openai`, `openrouter`, `ollama`) + credential store |
| **L2 runtime** | `flux-system` `flux-runtime` `flux-tools` `flux-events` | guarded IO, the safety envelope (+ the `context` projector module), built-in tools, the event store |
| **L3 agent** | `flux-agent` `flux-orchestrate` `flux-flow` `flux-eval` `flux-cognition` | agent definitions (`AgentSpec`/`Role`) + multi-agent orchestration + the Flux-Lang engine (the one turn loop) + the eval harness + the model-op cognition pack |
| **L4 extensibility** | `flux-plugin` | subprocess plugins + the JS pre-tool `hooks` module |
| **L5 capabilities** | `flux-capabilities` `flux-auth` | web egress + datasource/RAG tools (`browser`/`datasource` modules); caller identity (separate) |
| **L6 surfaces** | `flux-sdk` `flux-server` `flux-tui` `flux-cli` `flux-app` | SDK, HTTP server, TUI, the `flux` binary, the multi-agent program runtime host (`flux run app.flux`) |

Why this matters: it keeps the safety core (L0–L2) small and auditable, and makes "route around the
envelope" structurally hard. Notable rules that fall out:
- **`flux-runtime` (L2) does not depend on `flux-auth` (L5).** Surfaces resolve identity
  (`LocalIdentity` / `OidcIdentity`) into a `(Caller, Trust)` and inject it via `Executor::with_identity`.
- `flux-evidence`, `flux-skill`, `flux-config`, and `flux-lang` are L0 leaves on purpose, so
  runtime/agent crates may depend on them without a layering violation. `flux-lang` is the language
  **and its reference interpreter** — it uses async but takes all effects (op dispatch, value store,
  observation sink) as injected traits, so it has no L1+ flux dependency. The L3 `flux-flow` engine
  adapts its safety envelope onto those traits and re-exports `flux-lang` as a facade.

## Crate map by pillar

The layer table above is the *dependency* view (what may depend on what). Here is the same workspace
grouped by **which pillar each crate serves** — the three pillars ([docs map](README.md)) plus the
shared machinery beneath them. "Disposition" flags a planned move; see
[designs/crate-consolidation.md](designs/crate-consolidation.md) and the
[story board](stories/README.md).

### Shared core & safety (machinery beneath all three pillars)
| Crate | Layer | Role | Disposition |
|---|---|---|---|
| `flux-core` | L0 | content / message / streaming types, core errors | — |
| `flux-spec` | L0 | tool specs, effects, risk, intents | — |
| `flux-policy` | L0 | authorization engine (default-deny grants) | — |
| `flux-secret` | L0 | secret refs + redaction | — |
| `flux-evidence` | L0 | observations / evidence log | — |
| `flux-config` | L0 | `.flux/config.toml` loading + precedence | — |
| `flux-skill` | L0 | multi-format skill defs + discovery/merge + activation (triggers or name/description fallback) | — |
| `flux-markdown` | L0 | frontmatter parse/validate (`serde_norway`) + feature-gated render wrappers over `codewandler/markdown` | — |
| `flux-provider` | L1 | the `Provider` abstraction (published) | — |
| `flux-providers` | L1 | concrete clients (anthropic / openai / openrouter / ollama) | — |
| `flux-credentials` | L1 | credential store (PKCE, token import) | — |
| `flux-system` | L2 | guarded IO — the *only* real fs / proc / net | — |
| `flux-runtime` | L2 | `Executor::dispatch` — the safety envelope; `context` module = the projector | absorbed `flux-context` (consolidation P4 ✅) |
| `flux-tools` | L2 | built-in tools (read / write / edit / grep / …) | — |
| `flux-events` | L2 | append-only event store (SQLite) | — |
| `flux-codegate` | infra | the layering lint (enforces L0→L6) | — |

### Agent pillar
| Crate | Layer | Role | Disposition |
|---|---|---|---|
| `flux-flow` | L3 | the FlowEngine (the one turn loop) + the `AgentSink` streaming trait: compile NL→plan, execute the DAG, session store | — |
| `flux-agent` | L3 | the Agent pillar: `AgentSpec` + markdown `Role` definitions, assembled onto `FlowEngine` | — |
| `flux-orchestrate` | L3 | sub-agents + multi-agent orchestration | — |
| `flux-cognition` | L3 | model-backed ops (`ai.extract` / `rank` / `judge` / …) | — |

### Language pillar
| Crate | Layer | Role | Disposition |
|---|---|---|---|
| `flux-lang` | L0 | the Flux-Lang language: AST, analyzer, reference interpreter, schema (effects injected via traits) | — |

### Improvement pillar
| Crate | Layer | Role | Disposition |
|---|---|---|---|
| `flux-eval` | L3 | benchmark adapters, task mining, the self-improvement harness | — |

### Surfaces & extensions (deliver the Agent pillar)
| Crate | Layer | Role | Disposition |
|---|---|---|---|
| `flux-cli` | L6 | the `flux` binary (REPL, one-shot, subcommands) | — |
| `flux-tui` | L6 | ratatui chat UI | — |
| `flux-server` | L6 | axum HTTP API + SSE (bearer-auth) | — |
| `flux-sdk` | L6 | embeddable API (`Client` + `FlowClient`, DSL, recipes) | — |
| `flux-app` | L6 | multi-agent Program runtime host (`flux run app.flux`) | — |
| `flux-plugin` | L4 | subprocess plugins (NDJSON, capability-gated) + the JS pre-tool `hooks` module | absorbed `flux-hooks` (P2 ✅) |
| `flux-capabilities` | L5 | `browser` (`web_fetch`, SSRF-guarded; CDP deferred) + `datasource` (keyword index + search; RAG deferred) modules | merged `flux-browser` + `flux-datasource` (P3 ✅) |
| `flux-auth` | L5 | caller identity (`LocalIdentity`; OIDC seam) | kept standalone — identity ≠ tool capability |

## The safety envelope (the execution substrate)

Every plan node lowers onto one non-bypassable chain in `flux-runtime::Executor::dispatch` — the
substrate the flow engine executes against:

```
pre-tool hooks → authorization policy (default-deny) → permission rules → approval gate → guarded IO
```

1. **Pre-tool hooks** may observe / modify the input / deny the call (and short-circuit everything
   below). JS hooks run with a wall-clock interrupt so a runaway hook fails closed.
2. **Authorization policy** (`flux-policy`, pure, default-deny): the tool's declared `effects` +
   permission subjects are translated into `(action, resource)` requests and evaluated against grants
   (subjects × resources × actions, gated by trust + scopes). A `Deny` short-circuits; an
   `ApprovalRequired` (e.g. a grant marked `requires_approval`) forces the approval gate below — the
   policy is the floor, permission rules can't widen past it. A usable `default_local_grants()` keeps
   the local user working out of the box.
3. **Permission rules** (coder-style ergonomics layered on the policy): `Bash(git:*)`, `read`, etc.,
   deny-first then allow, otherwise prompt. "Always-allow" choices persist to `.flux/config.toml`.
4. **Approval gate**: forced for destructive intents, `Risk::Destructive`, policy `ApprovalRequired`,
   and unscoped writes — even under a permissive allow rule.
5. **Guarded IO** (`flux-system`): the *only* place real filesystem / process / network IO happens.
   Workspace-confined, symlink/escape-rejecting (including dangling symlinks), **argv-only** process
   execution with the parent environment cleared, output-capped, and an SSRF-guarded URL resolver
   (`flux_system::net::guard_url_scoped` / `guard_url`) shared by every egress path. Private or
   loopback egress is caller-scoped; plugin access requires both a manifest declaration and an
   operator config grant.

Secrets are registered with a `Redactor` and scrubbed from tool output (success and error). Evidence
observations (tool calls, destructive markers, skill activations, compaction) are recorded and
surfaced as events.

### Invariants worth never breaking
- All IO goes through `flux-system`; tools never touch `std::fs`/`std::process` directly.
- Every tool runs through `Executor::dispatch`; nothing calls a tool's `execute` directly in prod.
- A tool's `permission_subjects` must be accurate — a write that reports no subjects is forced to
  approval rather than silently authorized workspace-wide.
- Sub-agents inherit the policy and refuse destructive ops; a role's `tools: []` grants *zero* tools.

## Providers: wire codec × credential

A "provider" conflates two orthogonal axes, modeled separately and composed by `NativeProvider`:
- **`WireCodec`** — how a `Request` is serialized and its stream parsed (Anthropic Messages, OpenAI
  Chat, OpenAI Responses).
- **`Credential`** — auth/transport profile: tokens, base URL, gating headers, refresh.

`provider/model` routing selects a cell. v1 ships `anthropic`, `claude`, `openai`, `codex`,
`openrouter`. Adding a provider is a small composition, never a fork of the loop. Streaming is a
`Chunk` stream; usage accounting preserves input/cache tokens across `message_start`/`message_delta`.

## Agent loop, sessions, context

- **The turn loop is itself Flux-Lang.** `flux-flow`'s `FlowEngine::run_turn_cancellable` is a thin Rust
  *bootstrap* that runs `crates/flux-flow/assets/agent-loop.flux` — the loop logic lives in flux-lang, not
  Rust. Each turn that flow does: `plan` (re-enter the planner → a typed graph or a prose answer) →
  `match` on the result → `run_plan` (execute the graph through the same envelope) → feed the transcript
  back as `$feedback` → repeat until the model answers in prose. The reflexive ops `plan`/`run_plan` and
  the evidence ops `observe`/`evidence`/`grade`/`metrics` are what let the loop call the model and reason
  over its own runtime evidence (see `flux-flow/docs/ops-reference.md`). A workspace can override the loop
  with its own `.flux/agent-loop.flux`. The loop is cancellable (a `CancellationToken`). This is the
  **one** turn loop everywhere — CLI/server/TUI, the SDK (`flux_sdk::Client` assembles a `FlowEngine`
  via `AgentSpec`; `flux_sdk::FlowClient` is the declarative flow door), and sub-agents
  (`flux-orchestrate`). `flux-flow` owns the `AgentSink` streaming trait; `flux-agent` is the
  agent-definition crate (`AgentSpec` + markdown `Role`). The classic provider-native `Agent` loop is
  gone. The loop is filtered from the surface by default; watch it with
  `flux run --show-loop`, inspect its evidence with the REPL `/evidence`, and read or scaffold it with
  `flux loop show`/`eject` — see [the agent-loop guide](agent-loop.md).
- **Session shape is a hard invariant.** The persisted message log must always be a valid
  provider history: never an empty assistant message, never a split tool_use/tool_result pair, never a
  user-after-user sequence. The cancel, compaction, and max-iteration exit paths each append a final
  assistant/synthetic-result so the next turn isn't poisoned. (This is a recurring bug class — treat
  any new turn-termination path as suspect.)
- **`flux-events`** is a unified append-only event store (SQLite/WAL): one ordered log holds
  conversation messages, the flow run-trace, and per-turn telemetry. The "conversations view" is a
  *projection* over the log (replay message-kind events), and compaction is an append-only `Compacted`
  snapshot the projection resets to — history is never deleted. Turn events are just one event kind.
- **`flux-runtime`'s `context` module** projects an ordered provider chain (system / files / skills /
  task) under token budgets; long sessions compact older turns into a synthetic summary.

## Extensibility

- **Hooks** (`flux-plugin`'s `hooks` module): JavaScript pre-tool hooks (observe/modify/deny), run with a timeout.
- **Plugins** (`flux-plugin`): any-language subprocess binaries over a framed NDJSON protocol. A
  plugin's operations are projected as policy-gated tools; its privileged IO is requested back from the
  host as **capabilities the plugin declares in its manifest** (`process` allow-list, `secret` key
  allow-list, HTTP host allow-list, connection targets, private-network hosts) and the host checks
  every callback against that grant. A plugin gets no host capability by default; the plugin binary
  itself is trusted, pinned code, not an OS-sandboxed workload.
- **Roles & skills**: markdown with frontmatter, discovered from `.flux/`.

## Surfaces

- **`flux-sdk`** — high-level `Client` (run/stream, sessions) **and `FlowClient`**, the Flux-Lang
  lifecycle surface (compile→analyze→execute, `optimize`/`execute_optimized`, register op-packs/prelude).
- **`flux-app`** — the L6 runtime host that runs a multi-agent `.flux` **Program** (event bus, triggers,
  journeys; orchestration ops `emit`/`send`/`ask`/`spawn`), driven by `flux run app.flux`,
  deny-destructive by default.
- **`flux-server`** — axum HTTP API + SSE streaming; bearer-token authenticated except `/health` and
  the A2A discovery card, and refuses unauthenticated non-loopback binds.
- **`flux-tui`** — ratatui chat with live streaming + an in-TUI approval modal.
- **`flux-cli`** — the `flux` binary: REPL, `-p` one-shot, `--agent`, `--serve`, slash commands,
  `/pd` `/goal` `/loop` autopilot.

## The gate

A change is not done until `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo fmt --all --check`, and `cargo test -p flux-codegate` (the layering lint) are all
green. CI enforces them. This is a principle, not hygiene — see [vision.md](vision.md).
