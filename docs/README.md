# flux docs

Start here to find anything. **flux is a deterministic agent platform** built on one thesis —
**the LLM is not the runtime**: the model compiles a request into a typed, readable Flux-Lang plan,
and a deterministic Rust runtime executes that plan through one mandatory safety envelope
(authorization → approval → guarded IO). You see the plan before it runs; the same plan re-runs.

## The three pillars
1. **The Agent** — a zero-config personal coding agent (CLI/TUI), an embeddable Rust SDK, and an
   HTTP server. What most people touch. *Within this pillar*, surface priority is
   CLI/TUI → SDK → platform.
2. **The Language (Flux-Lang)** — the typed plan format the agent compiles into:
   **machine-generated** (emitted from NL as JSON or native text), **human-readable** (audit every
   plan before it runs), **lightly human-editable** (nudge a plan, don't write one from scratch).
   *Not* a hand-written general-purpose language.
3. **The Improvement Loop** — `flux-eval` + the self-improvement harness, kept in-repo because it is
   used directly to make flux better at real coding work.

Everything else (providers, tools, skills, orchestration, plugins) is machinery serving the three.

## Map
| If you want… | Read |
|---|---|
| Why flux exists; the principles | [vision.md](vision.md) |
| The design — layers, safety envelope, loop, **crate map** | [architecture.md](architecture.md) |
| Status + what's next | [roadmap.md](roadmap.md) |
| **What to work on right now** | [stories/README.md](stories/README.md) — the backlog/status board |
| Using the CLI / TUI / SDK / server | [usage.md](usage.md) |
| Choosing a model/provider | [model.md](model.md) |
| The self-hosted agent loop | [agent-loop.md](agent-loop.md) |
| Agent-to-agent (A2A) | [a2a.md](a2a.md) |
| **Language pillar** (deep) | [../crates/flux-lang/docs/](../crates/flux-lang/docs/) — PRD, reference, syntax, STATUS |
| **Improvement pillar** (deep) | [self-improvement/](self-improvement/) |
| Active designs | [designs/](designs/) |
| Finished / superseded material | [archive/](archive/) |

## Working on flux
Every contributor — human or agent — starts at [AGENTS.md](../AGENTS.md) → **"Start here"**: read the
pillars, open the [board](stories/README.md), take the top `ready` story, follow the workflow, keep
the gate green. New or unscoped work? Create a story from
[`stories/_TEMPLATE.md`](stories/_TEMPLATE.md) first, so the next agent inherits the context.

Editor support (IntelliJ plugin + TextMate grammar) lives in a separate repo, **flux-editors**.
