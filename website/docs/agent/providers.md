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
