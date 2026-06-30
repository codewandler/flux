# flux

<p align="center">
  <img src="assets/flux-logo.png" alt="Flux logo" width="420">
</p>

[![CI](https://github.com/codewandler/flux/actions/workflows/ci.yml/badge.svg)](https://github.com/codewandler/flux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/codewandler/flux)](https://github.com/codewandler/flux/releases/latest)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

**The LLM is not your runtime.** Most coding agents let the model drive execution step by step — slow, expensive, and hard to audit. flux inverts that: the model compiles your request into a typed **Flux-Lang plan** (a small graph), and a deterministic Rust runtime executes it through one mandatory safety envelope. You see the plan before it runs. Every file read, shell command, and web fetch is a node in that graph, not a hidden black box.

What that buys you:
- **Auditability** — a turn is a readable graph, not a stream of opaque tool calls
- **Safety by construction** — a single non-bypassable chain (authorization → approval → guarded IO) covers every operation; no tool, plugin, or sub-agent can route around it
- **Token efficiency** — tool outputs are stored as symbols, not re-sent on every turn
- **Repeatability** — a plan is an artifact; re-running it costs zero extra model calls

flux is one platform on that thesis, with three **co-equal pillars**:

1. **The Agent** — a zero-config CLI/TUI coding agent, an embeddable Rust SDK, and an HTTP server. What most people touch.
2. **The Language (Flux-Lang)** — the typed plan format the agent compiles into: machine-generated, human-readable, lightly human-editable. Not a language you hand-write from scratch.
3. **The Improvement Loop** — an eval + self-improvement harness (`flux-eval`) kept in-repo because it's used directly to make flux better at real coding work.

All three live in one strictly-layered Cargo workspace. New here? [`docs/README.md`](docs/README.md) is the full map.

---

## Install

**Prebuilt binary** — installs `flux` into `~/.cargo/bin` (Linux, macOS, Windows; x86_64 + aarch64):

```bash
# Linux / macOS
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/codewandler/flux/releases/latest/download/flux-cli-installer.sh | sh
```

```powershell
# Windows (PowerShell)
powershell -ExecutionPolicy Bypass -c "irm https://github.com/codewandler/flux/releases/latest/download/flux-cli-installer.ps1 | iex"
```

**From source** — requires **Rust 1.85+** (`rustup update stable`):

```bash
cargo install --git https://github.com/codewandler/flux flux-cli
# …or clone and build: cargo build --release   → target/release/flux
```

Prebuilt binaries, installers, and checksums are attached to every
[tagged release](https://github.com/codewandler/flux/releases/latest).

---

## Quickstart

```bash
# Ask flux to do something — risky steps prompt for approval; --yes auto-approves
flux run "add a test for the parser"

# Preview the plan before running it
flux plan "summarize README.md into SUMMARY.txt"

# Print the plan as JSON and exit (never runs)
flux plan -o json "print hello world 3 times"

# Interactive REPL — session auto-saved; /help for slash commands
flux
flux run -c                   # continue the most recent session

# ratatui TUI with live token streaming and an in-UI approval modal
flux tui

# HTTP/A2A daemon (REST + SSE streaming)
flux app run --serve 127.0.0.1:8787 --yes
```

No API key needed to try the engine: `-m mock` runs an offline provider through the full pipeline.

```bash
flux run --yes -m mock "summarise this repo"
```

See [`docs/usage.md`](docs/usage.md) for the full guide.

---

## Providers & auth

A provider is a **wire codec × credential** cell. Select one with `-m <provider>/<model>`
(bare aliases `opus`/`sonnet`/`haiku` resolve to Anthropic):

| `-m` provider | Wire | Auth | Notes |
|---|---|---|---|
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY` | API key |
| `claude` | Anthropic Messages | Claude subscription OAuth | imports `~/.claude/.credentials.json`; opt-in |
| `openai` | OpenAI Chat | `OPENAI_API_KEY` | API key |
| `codex` | OpenAI Responses | ChatGPT/Codex OAuth | imports `~/.codex/auth.json`; opt-in |
| `openrouter` | OpenAI Chat | `OPENROUTER_API_KEY` | API key |

```bash
flux auth status                 # show what credentials are available and from where
flux auth login claude           # PKCE login for the Claude subscription path
flux run -m claude/opus "hi"
flux run -m openrouter/anthropic/claude-sonnet-4.5 "hi"
```

`--think` toggles adaptive thinking; `--effort low|medium|high|xhigh|max` controls depth.

> The subscription paths (`claude`, `codex`) reuse credentials from existing CLI tools. They are
> opt-in and never the default; the API-key paths are the supported way to run flux.

---

## Configuration (`.flux/config.toml`)

Precedence: CLI flags > project `.flux/config.toml` > user `~/.flux/config.toml` > defaults.

```toml
model = "claude/opus"            # default model (-m overrides)
allow_private_net = false        # let web_fetch / plugins reach loopback/private addresses

[permissions]                    # deny wins, then allow, otherwise prompt
allow = ["read", "glob", "grep", "search", "Bash(git:*)"]
deny  = ["Bash(rm:*)"]

[[policy.grants]]                # optional fine-grained authorization grants
subjects  = [{ kind = "user", id = "*" }]
resources = [{ kind = "path", path = "src/**" }]
actions   = ["workspace.write"]
```

"Always-allow" choices from approval prompts are saved back here automatically.

---

## Safety model

Every plan node lowers onto one non-bypassable chain before it touches the real world:

```
pre-tool hooks → authorization policy (default-deny) → permission rules → approval gate → guarded IO
```

- **Policy** (pure, default-deny): grants over subjects × resources × actions, gated by trust and scopes. A sensible local default keeps the agent productive out of the box.
- **Destructive operations are forced to approval** even under a permissive allow-rule (`rm -rf`, `git push --force`, …).
- **Guarded IO** is the only place real filesystem/process/network access happens — workspace-confined, symlink/escape-rejecting, **argv-only** (no shell injection), SSRF-guarded fetch.
- **Secrets** are registered with a redactor and scrubbed from all tool output and logs.
- **Evidence** — tool calls, destructive markers, skill activations, and compaction are recorded as auditable events.

Sub-agents inherit the same policy and cannot approve destructive operations themselves.

---

## Capabilities

**Built-in tools:** `read`, `write`, `edit`, `bash`, `glob`, `grep`, `web_fetch` (SSRF-guarded), `search` (auto-indexed workspace docs), `task` (delegate to a sub-agent role).

**Skills** — markdown knowledge packs discovered from the project's `.flux/skills` **and** the
user-global dirs `~/.flux/skills`, `~/.agents/skills`, and `~/.claude/skills` (project wins on a name
clash). Both the flux-native format (`triggers:` frontmatter) and the cross-agent [Agent
Skills](https://agentskills.io)/Claude format (`name` + `description`, no triggers) are read, so
skills you already keep for other agents work in flux unchanged. Each turn flux activates the skills
whose triggers match — or, for trigger-less skills, whose `name`/`description` keywords match — and
injects their bodies into that turn's context (ranked and capped to keep the prompt lean).

**Sub-agent roles** (`.flux/agents/<role>.md`): scout / planner / worker / reviewer / evaluator / summarizer — built-in defaults, overridable with your own markdown files.

**Plugins** (`~/.flux/plugins/*.toml`): subprocess binaries in any language over a framed NDJSON protocol. Their operations become policy-gated tools; privileged IO is requested back from the host via declared capabilities. A plugin gets **only** what its manifest declares — runnable programs, readable secret keys, and HTTP access are all explicit allow-lists checked on every call.
- `flux plugin add <name> <program> [args…] | ls | pin <name> <ver> | rollback <name>`

**Hooks** (`.flux/hooks/*.js`): JavaScript pre-tool hooks that can observe, modify, or deny a call.

### REPL slash commands

```
/help  /tools  /session  /clear
/plan               toggle plan mode — show the plan without running it
/run                execute the plan you just reviewed
/model <spec>       switch model/provider mid-session (e.g. /model opus)
/sessions           list recent sessions; /resume <id> reattaches
/pd <goal>          plan-and-dispatch: planner → parallel dependency waves of workers
/goal <condition>   drive turns toward a goal, judged by an evaluator sub-agent
/loop <n> <task>    run a task up to n times
/exit               (Ctrl-C interrupts a running turn; Ctrl-D exits)
```

The REPL has line editing, persistent history, and reverse-search. `flux sessions` lists past sessions from the shell.

Long sessions are **compacted** automatically: older turns are summarized once the session exceeds a budget (`FLUX_COMPACT_CHARS`, default 48 k characters; `0` disables).

---

## HTTP API (`flux app run --serve`)

`flux app run --serve <addr> --yes` exposes the built-in coding agent over HTTP/A2A. With a
`<program.flux>` argument, the same flag exposes that program's sole agent; programs can also declare an
`a2a` channel directly.

| Route | Purpose |
|---|---|
| `GET  /health` | liveness |
| `POST /sessions` | create a session → `{ id, model }` |
| `GET  /sessions/:id` | session info |
| `POST /sessions/:id/messages` | run a turn → `{ text, tool_calls, usage }` |
| `GET  /sessions/:id/stream?input=…` | **Server-Sent Events**: `text` / `tool` / `done` |
| `POST /webhook` | external trigger → fresh session + one turn |

Every route except `GET /health` requires `Authorization: Bearer $FLUX_SERVER_TOKEN`. A non-loopback bind
without the token set is refused (the daemon auto-approves tools, so an open listener is RCE).

---

## Presets (prebuilt flows)

`flux preset` exposes the [`flux_sdk::recipes`](crates/flux-sdk/src/recipes) cookbook — reusable,
parameterized Flux-Lang flows (loops, retry/timeout/budget, fallback, fan-out, dispatch, and a nested
`retry { timeout { fallback {…} } }`) — straight from the binary. Name a preset, fill its op-name slots
and input with `key=value` arguments, then **scaffold** the flow (default) or **run** it (`--run`) through
the same envelope as every other turn.

```sh
flux preset list                                   # the cookbook
flux preset help retry_with_backoff                # a preset's keys + whether it runs offline

# scaffold (print the flow; -o json is the form `flux flow run <file>` ingests):
flux preset map_each item=f source='["README.md","Cargo.toml"]' op=read collect=out
flux preset retry_with_backoff max=3 backoff=exponential delay_ms=200 op=read input='"README.md"' bind=r -o json

# run it (reads real files through the envelope; --yes auto-approves):
flux preset map_each item=f source='["README.md"]' op=read collect=out --run --yes
```

Recipes are op-agnostic templates, so a preset runs offline whenever its ops resolve in the live registry
(the built-ins: `read`/`grep`/`glob`/`write`/…). The model-flavored presets (`route_intent`,
`answer_with_fallback`) need a provider (`-m provider/model`) and are scaffold-by-default; without one,
`--run` fails fast at analysis with a precise "unknown operation" diagnostic.

---

## Library use (`flux-sdk`)

```rust
let provider = Box::new(flux_providers::anthropic::anthropic_from_env()?);
let client = flux_sdk::Client::builder().model("anthropic/opus").build(provider, ".")?;
let out = client.run("Summarize the README").await?;
println!("{}", out.text);
```

---

## Architecture

flux is a single Cargo workspace of strictly-layered crates — inner crates never depend on outer ones, enforced by a test. The layers:

| Layer | Role |
|---|---|
| **Contracts (L0)** | pure types, policy, secrets, tool specs, config, evidence, skills — no IO |
| **Providers (L1)** | wire codec × credential cells; Anthropic, OpenAI, OpenRouter |
| **Runtime (L2)** | guarded IO, the safety envelope, built-in tools, sessions, context |
| **Agent (L3)** | the Flux-Lang engine (the one turn loop) + agent definitions (`AgentSpec`/`Role`) + multi-agent orchestration |
| **Extensibility (L4)** | JavaScript hooks + subprocess plugins |
| **Capabilities (L5)** | browser/web egress, datasource/RAG, caller identity |
| **Surfaces (L6)** | SDK, HTTP server, integrations, TUI, the `flux` CLI |

The thesis runs all the way down: the agent's **turn loop is itself written in Flux-Lang** (`agent-loop.flux`) — the model compiles each step into a typed plan the runtime executes, and even the loop that orchestrates those steps is a plan you can read, gated by the same safety envelope. Watch it with `flux run --show-loop`, inspect its evidence with `/evidence`, and read or customize it with `flux loop show`/`eject` — see [docs/agent-loop.md](docs/agent-loop.md).

See [docs/architecture.md](docs/architecture.md) for the full design, [docs/vision.md](docs/vision.md) for the project's direction, and [AGENTS.md](AGENTS.md) for the contributor guide.

---

## Development

```bash
cargo test --workspace                                   # all tests
cargo clippy --workspace --all-targets -- -D warnings    # lints (must be clean)
cargo fmt --all --check                                  # formatting
cargo test -p flux-codegate                              # architecture layering lint
```

CI runs all of the above on every pull request. See [CHANGELOG.md](CHANGELOG.md) for release notes and [docs/roadmap.md](docs/roadmap.md) for what's next.

## License

MIT OR Apache-2.0
