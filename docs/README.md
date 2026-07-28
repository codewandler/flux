# flux docs

Start here to find anything inside the repository. These are **internal contributor docs**: design
records, story status, implementation notes, and crate references. The public user documentation lives
in [`../website`](../website) and is deployed to `https://codewandler.github.io/flux/`.

**flux is a deterministic agent platform** built on one thesis —
**the LLM is not the runtime**: typed model stages interpret intent and gather evidence, while an
authored Flux-Lang loop freezes effects into an action batch and a deterministic Rust runtime executes
it through one mandatory safety envelope (authorization → approval → guarded IO). The model never
authors executable Flux.

## The three pillars
1. **The Agent** — a zero-config personal coding agent (CLI/TUI), an embeddable Rust SDK, and an
   HTTP server. What most people touch. *Within this pillar*, surface priority is
   CLI/TUI → SDK → platform.
2. **The Language (Flux-Lang)** — a small language for authored, analyzer-validated flows, reusable
   operations, agent outer loops, and durable journeys. It places deterministic control flow around
   explicit model stages and guarded effects; it is not model output or a general-purpose language.
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
| Latest repository architecture review | [designs/architecture-review-2026-07-14/review.md](designs/architecture-review-2026-07-14/review.md) — ranked findings, evidence, and linked remediation stories |
| Using the CLI / TUI / SDK / server | [usage.md](usage.md) |
| Choosing a model/provider | [model.md](model.md) |
| The self-hosted agent loop | [agent-loop.md](agent-loop.md) |
| Agent-to-agent (A2A) | [a2a.md](a2a.md) |
| A2A protocol support (what's supported / partial / not yet) | [a2a-conformance.md](a2a-conformance.md) |
| **Language pillar** (essence) | [language.md](language.md) — Flux-Lang semantics in one place |
| **Language pillar** (deep) | [../crates/flux-lang/docs/](../crates/flux-lang/docs/) — PRD, reference, syntax, STATUS |
| Generated agent skills | [designs/generated-flux-skills.md](designs/generated-flux-skills.md) — `flux skill` root/section skills grounded in live catalogs |
| Claude Code interop (commands + skills compatibility) | [designs/claude-interop.md](designs/claude-interop.md) — audit + epic D-186…D-192 |
| **Improvement pillar** (deep) | [self-improvement/](self-improvement/) |
| Public docs site source | [../website](../website) — Docusaurus docs for users and integrators |
| Design records — active work **and** shipped-epic references (the design of a shipped epic stays here as its record) | [designs/](designs/) |
| Fully finished / superseded material (post-mortems, retired research) | [archive/](archive/) |
| Schema-migration drift ledger (D-31/D-34/D-36..D-45) | [archive/drift-reports.md](archive/drift-reports.md) |

## Working on flux
Every contributor — human or agent — starts at [AGENTS.md](../AGENTS.md) → **"Start here"**: read the
pillars, open the [board](stories/README.md), take the top `ready` story, follow the workflow, keep
the gate green. New or unscoped work? Create a story from
[`stories/_TEMPLATE.md`](stories/_TEMPLATE.md) first, so the next agent inherits the context.

Editor support: syntax highlighting comes from the **flux-tree-sitter** repo (Helix/Neovim/Zed),
the TextMate grammar + IntelliJ plugin from **flux-editors**, and language intelligence from the
in-repo `crates/flux-lsp` server. The public setup guide is
[`website/docs/language/editors.md`](../website/docs/language/editors.md) (published at
`/docs/language/editors`).
