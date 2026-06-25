# Changelog

All notable changes to this project are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/).

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

[0.2.2]: https://github.com/codewandler/flux/releases/tag/v0.2.2
[0.2.1]: https://github.com/codewandler/flux/releases/tag/v0.2.1
[0.2.0]: https://github.com/codewandler/flux/releases/tag/v0.2.0
[0.1.1]: https://github.com/codewandler/flux/releases/tag/v0.1.1
[0.1.0]: https://github.com/codewandler/flux/releases/tag/v0.1.0
