---
title: Providers and models
---

# Providers and models

flux keeps provider transport separate from the agent runtime. A **provider** is a *wire codec ×
credential* cell — how a request is serialized and how it authenticates — and the runtime stays
responsible for executing plans and gating IO. Adding a provider is a small composition, never a fork
of the loop.

## Routing

Select a provider and model with `-m <provider>/<model>`, or set `model` in `.flux/config.toml`. The
model string after the provider is forwarded verbatim, so any model that provider serves works.

```bash
flux run -m sonnet "fix the failing test"          # bare alias -> Anthropic
flux run -m anthropic/claude-sonnet-4-6 "review this PR"
flux run -m openai/gpt-5 "summarize this repository"
flux run -m ollama/qwen2.5-coder:7b "explain the provider layer"
```

Bare aliases resolve to Anthropic: `opus` → `claude-opus-4-8`, `sonnet` → `claude-sonnet-4-6`,
`haiku` → `claude-haiku-4-5-20251001`. The default model is `sonnet`.

## Supported providers

| `-m` prefix | Wire | Credential | Notes |
|---|---|---|---|
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY` | bare aliases `opus` / `sonnet` / `haiku` |
| `claude` | Anthropic Messages | Claude subscription OAuth | opt-in: `flux auth login claude` |
| `openai` | OpenAI Chat | `OPENAI_API_KEY` | full streaming + tool calls |
| `codex` | OpenAI Responses | ChatGPT/Codex OAuth | opt-in: `flux auth login codex` |
| `aws` | Anthropic Messages (Bedrock) | AWS chain (env / SSO / IRSA / EKS) | Claude via Bedrock; no `aws` CLI; region-aware ids; metered |
| `openrouter` | OpenAI Chat | `OPENROUTER_API_KEY` | proxy to hundreds of models |
| `openrouter-anthropic` | Anthropic Messages | `OPENROUTER_API_KEY` | native `tool_use`; preferred for agentic use |
| `ollama` | OpenAI Chat | none (local) | `OLLAMA_HOST` overrides `localhost:11434`; needs a tool-capable model |
| `ollama-anthropic` | Anthropic Messages | none (local) | recent Ollama builds; native `tool_use` |
| `mock` | — | none | offline test provider; exercises the full pipeline |

Because flux's loop is tool-driven, prefer models with reliable function/tool calling. For OpenRouter
and Ollama, the `*-anthropic` variants return structured `tool_use` blocks instead of risking inline
text-leaked tool calls.

## Model capability floor

flux's planner asks the model to emit a typed Flux-Lang plan (the `emit_plan` tool) that passes a
strict validator, with a bounded repair loop when a first attempt is rejected. Frontier and mid-tier
models (Claude, GPT-5-class, Codex, and the stronger OpenRouter models) clear this reliably;
**small/weak models can fail the planner contract even on trivial requests** — the repair loop
exhausts its budget on malformed tool-call JSON or an invalid plan and the run errors rather than
silently degrading. Routing is per-`-m`, so if a model can't produce a valid plan, point a capable
model at the planning turn (and, if you like, cheaper models at sub-agents).

The runtime also guards a related weak-model failure mode: a model that calls a read/search op and
then *repeats the same call* instead of making progress is caught by the loop's stall guards, which
escalate and then stop with an honest "could not make progress" instead of looping indefinitely.

## Credentials

```bash
flux auth status                 # what is configured, and from where
flux auth login claude           # Claude subscription (OAuth)
flux auth login codex            # ChatGPT/Codex subscription (OAuth)
```

Credential precedence: an environment variable (`ANTHROPIC_API_KEY`, `OPENROUTER_API_KEY`, …) wins,
then a stored credential from `flux auth login`, then an imported CLI credential (Claude/Codex).

## Prompt caching

Prompt caching is applied automatically for long contexts on providers that support it — no flag
needed.

:::note
`--think` / `--effort` flags exist but are hidden and **not yet wired into the plan engine** — they
currently only affect the raw `-p` prompt path. Extended-thinking control for full agent turns is
planned.
:::
