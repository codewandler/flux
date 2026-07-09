# flux

<p align="center">
  <img src="assets/flux-logo.png" alt="Flux logo" width="420">
</p>

[![CI](https://github.com/codewandler/flux/actions/workflows/ci.yml/badge.svg)](https://github.com/codewandler/flux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/codewandler/flux)](https://github.com/codewandler/flux/releases/latest)
[![Docs](https://img.shields.io/badge/docs-codewandler.github.io%2Fflux-blue)](https://codewandler.github.io/flux/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

flux is a deterministic agent platform built around one thesis:

**the LLM is not the runtime.**

The model turns requests into a typed, inspectable Flux-Lang plan. A deterministic Rust runtime
executes that plan through one mandatory safety chain:

```text
authorization -> approval -> guarded IO
```

That gives you reproducibility, reviewability, and a consistent security posture across CLI, tools,
and integrations.

## What flux includes

Flux is one platform with three co-equal pillars:

- **Agent**: local CLI/TUI, Rust SDK, HTTP server, and A2A support.
- **Flux-Lang**: typed plans for orchestration and structure, with editor support — the in-repo
  `flux-lsp` language server (diagnostics, completion, hover, formatting) and a
  [tree-sitter grammar](https://github.com/codewandler/flux-tree-sitter) for Helix/Neovim/Zed
  highlighting.
- **Improvement loop**: evidence-driven eval and self-improvement tooling.

Use flux when you want:

- a plan you can inspect and review before execution
- guardrails that are explicit and consistent
- deterministic replay/fork/diff workflows
- an embeddable agent surface inside your own product

## Install

### Prebuilt binary

```bash
# Linux / macOS
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/codewandler/flux/releases/latest/download/flux-cli-installer.sh | sh
```

```powershell
# Windows (PowerShell)
powershell -ExecutionPolicy Bypass -c "irm https://github.com/codewandler/flux/releases/latest/download/flux-cli-installer.ps1 | iex"
```

### From source

```bash
cargo install --git https://github.com/codewandler/flux flux-cli
# optional: the .flux language server for your editor (diagnostics, completion, hover, formatting)
cargo install --git https://github.com/codewandler/flux flux-lsp
# …or clone and build: cargo build --release   → target/release/flux
#    (from a clone, `task install` installs flux and flux-lsp together)
```

Release assets (including checksums and plugin-release checks) are in each
[GitHub release](https://github.com/codewandler/flux/releases/latest). Plugin packs ship separately as
`plugins-v*` and install via the plugin CLI.

## Quickstart

```bash
# Start a normal turn
flux run "add a test for the parser"

# Inspect what would run
flux plan "summarize README.md into SUMMARY.txt"

# Show plan as JSON and exit (never runs)
flux plan -o json "print hello world 3 times"

# Start REPL / TUI / server
flux
flux tui
flux app run --serve 127.0.0.1:8787 --yes
```

Run without API keys using the offline provider:

```bash
flux run --yes -m mock "write a quick note"
```

## Providers and auth

A provider is a **wire codec × credential** pair selected with `-m <provider>/<model>`.

```bash
flux auth status
flux auth login claude   # optional opt-in OAuth path
```

| `-m` provider | Wire | Auth |
| --- | --- | --- |
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY` |
| `openai` | OpenAI Chat | `OPENAI_API_KEY` |
| `openrouter` | OpenAI Chat | `OPENROUTER_API_KEY` |
| `openrouter-anthropic` | Anthropic Messages | `OPENROUTER_API_KEY` |
| `ollama` | OpenAI Chat | local |
| `ollama-anthropic` | Anthropic Messages | local |
| `claude` | Anthropic Messages | Claude subscription OAuth (`~/.claude/.credentials.json`) |
| `codex` | OpenAI Responses | ChatGPT/Codex OAuth (`~/.codex/auth.json`) |
| `aws` | Bedrock Anthropic | AWS env / SSO / IRSA / EKS Pod Identity |

## Configuration (`.flux/config.toml`)

Configuration precedence is:

1. CLI flags
2. project `.flux/config.toml`
3. user `~/.flux/config.toml`
4. defaults

```toml
model = "claude/opus"

[private_net]
web_fetch = ["localhost"]

[private_net.plugins]
prometheus = ["prometheus.local"]

[permissions]
allow = ["read", "glob", "grep", "search", "Bash(git:*)"]
deny  = ["Bash(rm:*)"]

[[policy.grants]]
subjects = [{ kind = "user", id = "*" }]
resources = [{ kind = "path", path = "src/**" }]
actions = ["workspace.write"]
```

`allow_private_net = true` is still honored for compatibility, but plugin private-network grants still
require explicit `[private_net.plugins]`.

## Safety and execution model

Every operation goes through:

```
capability scope floor → policy (deny by default) → permissions → approval → guarded IO
```

No bypass path exists:

- workspace file/process/net operations are guarded by `flux-system`
- unsafe/ambiguous commands can be denied at analysis or approval time
- destructive intent is surfaced at plan level and re-checked at dispatch time
- all secrets are registered with the redactor and scrubbed from tool output/logs
- evidence is persisted per session and event for auditability

Sub-agents inherit the same safety chain; their plans are validated with the same checks.

## Capabilities and operations

- Built-in tools include file, search, web, and delegation operations.
- Optional shell support (`bash`) is available behind the `shell` signal.
- Skills are loaded from project `.flux/skills` and standard global skill directories.
- Plugin operations are manifest-scoped; privileges are explicit and enforced.
- Approval and policy hooks (`.flux/hooks/*.js`) can validate/transform/deny calls.
- REPL slash commands include `/plan`, `/run`, `/tools`, `/session`, `/compact`, `/evidence`, and more.

## HTTP API

`flux app run --serve <addr> --yes` hosts a Flux agent.

| Route | Purpose |
| --- | --- |
| `GET /health` | Liveness |
| `GET /.well-known/agent-card.json` | A2A discovery card |
| `POST /a2a` | A2A message send/stream endpoints |
| `POST /sessions` | Start a new session |
| `GET /sessions/:id` | Session status |
| `POST /sessions/:id/messages` | Run one turn |
| `GET /sessions/:id/stream?input=...` | SSE stream |
| `POST /webhook` | External trigger |

All non-health routes require bearer auth by default. A non-loopback bind without `FLUX_SERVER_TOKEN` is rejected.

## Presets and programs

flux includes a preset library (`flux preset`) for common flow structures (retries, fan-out, loops,
fallbacks). Programs (`flux run --program`) let you compose multi-agent journeys in Flux-Lang.

```bash
flux preset list
flux preset retry_with_backoff max=3 delay_ms=200 op=read input='"README.md"' bind=r --run --yes
```

## SDK

For an embed path in Rust, use the SDK client, which assembles the same flow engine and safety
pipeline used by the CLI.

```rust
let provider = Box::new(flux_providers::anthropic::anthropic_from_env()?);
let client = flux_sdk::Client::builder().model("anthropic/opus").build(provider, ".")?;
let out = client.run("Summarize the README").await?;
println!("{}", out.text);
```

## Plugin packs

Official integrations (GitLab, Slack, Kubernetes, SQL, etc.) are published as a signed plugin pack.

```bash
flux plugin install gitlab
flux plugin install --all
```

The plugin release index is minisign-checked and each archive hash is verified before install.

## Documentation

- Public docs: [codewandler.github.io/flux](https://codewandler.github.io/flux/)
- Getting started: [Getting started](https://codewandler.github.io/flux/docs/getting-started)
- Agent loop: [The agent loop](https://codewandler.github.io/flux/docs/agent/agent-loop)
- Language guide: [Flux-Lang overview](https://codewandler.github.io/flux/docs/language/overview)
- Editor support: [Editor setup](https://codewandler.github.io/flux/docs/language/editors)
- SDK: [SDK overview](https://codewandler.github.io/flux/docs/sdk/overview)
- Plugins: [Using plugins](https://codewandler.github.io/flux/docs/plugins/using-plugins)

See `docs/usage.md` for the internal command surface map and additional CLI details.

## Architecture

flux is a layered Rust workspace from contracts to extensions. The safety guarantees are enforced by the
runtime layer and checked by a dedicated architecture gate (`flux-codegate`). For full detail:

- [`docs/architecture.md`](docs/architecture.md)
- [`docs/vision.md`](docs/vision.md)
- [AGENTS.md](AGENTS.md) (agent operating instructions)

## Development

For contributors:

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo test -p flux-codegate
```

## License

Licensed under MIT OR Apache-2.0, at your option.
