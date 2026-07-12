---
title: Providers and models
description: "Provider/model routing, credentials, and the current model capability matrix for text turns."
---

# Providers and models

flux keeps model transport separate from the agent runtime. A provider is a wire codec plus a
credential source: how a request is serialized and how it authenticates. The runtime still owns plans,
tool dispatch, approval, and guarded IO.

Use this page to choose `-m <provider>/<model>`, configure credentials, and understand which providers
support prompt caching or subscription credentials.

## Routing

Select a provider and model with `-m <provider>/<model>`, or set `model` in `.flux/config.toml`. The
model string after the provider is forwarded verbatim, so any model that provider serves works.

```bash
flux run -m sonnet "fix the failing test"          # bare alias -> Anthropic
flux run -m anthropic/claude-sonnet-4-6 "review this PR"
flux run -m openai/gpt-5 "summarize this repository"
flux run -m ollama/qwen2.5-coder:7b "explain the provider layer"
```

Bare aliases resolve to Anthropic and track the current generation of each tier: `fable` →
`claude-fable-5`, `opus` → `claude-opus-4-8`, `sonnet` → `claude-sonnet-5`, `haiku` →
`claude-haiku-4-5`. The default model is `sonnet`. Bare `claude` is shorthand for `claude/sonnet`
(the [Claude Code subscription](./claude-code.md) provider).

flux gates the optional Messages-API fields per model, so every alias and id works: adaptive
thinking and `output_config.effort` are only sent to models that accept them (the 4.6 family and
newer — Haiku 4.5 and older reject them with HTTP 400), and `temperature`/`top_p` are omitted for
the generations that reject sampling params (Fable 5, Opus ≥ 4.7, Sonnet ≥ 5). Unknown or future
ids default to the newest shape, so a new Anthropic generation works on day one. The gating
applies wherever an Anthropic model is served: `anthropic`, `claude`, `aws` (Bedrock
inference-profile ids), and `openrouter-anthropic` (`anthropic/…` slugs).

## Supported providers

| `-m` prefix | Wire | Credential | Notes |
|---|---|---|---|
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY` | bare aliases `fable` / `opus` / `sonnet` / `haiku` |
| `claude` | Anthropic Messages | Claude subscription OAuth | [Claude Code subscription](./claude-code.md); bare `claude` = `claude/sonnet` |
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

For the full credential model — where tokens are stored, how secret values are kept out of the
model's context, the Vault backend, and OAuth login for plugins — see
[Credentials & secrets](../security/credentials.md).

## Prompt caching

Prompt caching is applied automatically for long contexts on providers that support it — no flag
needed.

:::note
`--think` / `--effort` flags are hidden and accepted for CLI compatibility, but are currently
**no-ops** — the raw `-p` prompt path that consumed them was removed in the engine cutover.
Extended-thinking control for full agent turns is planned.
:::

## Related docs

- [Credentials and secrets](../security/credentials.md) — where provider tokens live and how redaction works.
- [Configuration](../reference/config.md) — set a default model and permission rules.
- [Usage & cost](./cost.md) — how model usage is reported.
