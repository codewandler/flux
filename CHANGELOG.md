# Changelog

All notable changes to this project are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Per-turn token usage flows through the unified loop and renders in the CLI.** The planner's token
  counts are now captured from the provider stream (`compile_turn` returns them), accumulated across a
  turn's planner calls by the loop host (output summed; input/cache reflect the final, largest prompt
  so re-sent context isn't multiply-counted), and handed to `sink.turn_end` by the engine. The CLI's
  turn-end rule now shows **context-window occupancy, generated tokens, and — under prompt caching —
  cached tokens with the hit-rate** (e.g. `1 step · 90ms · ctx 1.4k · out 60 · cache 1.2k (87% hit)`);
  it stays clean (no all-zero noise) on offline `-m mock` turns. The SDK `Client` now also populates
  `TurnOutput.usage`. (Previously usage was dropped through the flux-lang loop — `turn_end(None)`.)
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
