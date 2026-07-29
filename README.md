<h1 align="center">
  <img src="assets/readme-hero.svg" alt="flux — the model proposes, the runtime disposes" width="960">
</h1>

<p align="center">
  A Rust agent platform for typed stages, authored flows, guarded execution, and replayable evidence.
</p>

<p align="center">
  <a href="https://github.com/codewandler/flux/actions/workflows/ci.yml"><img src="https://github.com/codewandler/flux/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/codewandler/flux/releases/latest"><img src="https://img.shields.io/github/v/release/codewandler/flux" alt="Latest release"></a>
  <a href="https://codewandler.github.io/flux/"><img src="https://img.shields.io/badge/docs-codewandler.github.io%2Fflux-0bbf83" alt="Documentation"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-141a18" alt="MIT or Apache 2.0 license"></a>
</p>

<p align="center">
  <a href="#install"><strong>Install</strong></a> ·
  <a href="#quickstart"><strong>Quickstart</strong></a> ·
  <a href="https://codewandler.github.io/flux/"><strong>Documentation</strong></a> ·
  <a href="docs/architecture.md"><strong>Architecture</strong></a>
</p>

---

## The LLM is not the runtime

In flux, the model never becomes the execution engine—or the author of executable Flux code. It
declares intent, explores through exact provider-native operation schemas, and proposes literal
actions. An authored Flux-Lang loop and deterministic Rust runtime own what happens next.

```text
request → typed intent → scoped exploration → action batch → approval → guarded execution
                       authored Flux-Lang + deterministic runtime
```

That hard boundary makes effects reviewable before they run, authored workflows repeatable, and every
operation governable. The same execution envelope covers local tools, plugins, sub-agents, the SDK,
and the server.

## What flux includes

Three co-equal pillars:

- **Agent** — local CLI/TUI, Rust SDK, HTTP server, A2A support.
- **Flux-Lang** — typed, authored flows for orchestration and reliable structure, with the in-repo
  `flux-lsp` language server (diagnostics, completion, hover, formatting) and a
  [tree-sitter grammar](https://github.com/codewandler/flux-tree-sitter) for Helix/Neovim/Zed.
- **Improvement loop** — evidence-driven eval and self-improvement tooling.

Reach for flux when you want an action batch you can inspect and approve before it runs, guardrails
that are explicit rather than implied, deterministic replay/fork/diff, or an embeddable agent
surface inside your own product.

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

# Reveal intent, capability selection, and batch machinery
flux run --show-loop "summarize README.md into SUMMARY.txt"

# Inspect the authored outer loop; ejecting it does not activate it implicitly
flux loop show

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
| `openrouter` | Anthropic Messages | `OPENROUTER_API_KEY` |
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
web = ["localhost"]

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
- effectful native calls are frozen into an approved batch and re-checked at dispatch time
- all secrets are registered with the redactor and scrubbed from tool output/logs
- evidence is persisted per session and event for auditability

Sub-agents inherit the same safety chain; their loops and operation calls are validated with the same checks.

## Capabilities and operations

- Built-in tools cover file, search, web, and delegation operations. Optional shell (`bash`) sits
  behind the `shell` signal.
- Skills and markdown slash-commands load from both the `.flux/` and `.claude/` trees (project and
  user-global, nested multi-file skills included). Skills are manual-only by default, with an
  opt-in model-invoked mode.
- Plugin operations are manifest-scoped, with explicit enforced privileges. Approval and policy
  hooks (`.flux/hooks/*.js`) can validate, transform, or deny calls.
- REPL slash commands include `/tools`, `/sessions`, `/compact`, `/evidence`, and more.
- `flux tui` is the same daily driver in a dense borderless UI — mid-turn steering (type while a
  turn runs; queued messages stay editable in `/queue`), themes, history and transcript search
  (Ctrl-R / Ctrl-F), `@` path completion, hunk-view diffs, session picker/replay, and live model
  switching.

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

Full docs: **[codewandler.github.io/flux](https://codewandler.github.io/flux/)** —
[getting started](https://codewandler.github.io/flux/docs/getting-started) ·
[the agent loop](https://codewandler.github.io/flux/docs/agent/agent-loop) ·
[Flux-Lang](https://codewandler.github.io/flux/docs/language/overview) ·
[editor setup](https://codewandler.github.io/flux/docs/language/editors) ·
[SDK](https://codewandler.github.io/flux/docs/sdk/overview) ·
[plugins](https://codewandler.github.io/flux/docs/plugins/using-plugins).

In-repo: [`docs/architecture.md`](docs/architecture.md) ·
[`docs/vision.md`](docs/vision.md) · [`docs/usage.md`](docs/usage.md) (command surface map).

## Development

flux is a layered Rust workspace from contracts to extensions; the safety guarantees are enforced by
the runtime layer and checked by an architecture gate (`flux-codegate`).

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo test -p flux-codegate                              # architecture layering lint
```

**[AGENTS.md](AGENTS.md) is the authoritative contributor contract** — architecture boundaries,
safety invariants, conventions, and release mechanics.

## License

Licensed under MIT OR Apache-2.0, at your option.
