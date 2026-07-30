---
title: Providers and models
description: "Provider/model routing, credentials, and the current model capability matrix for text turns."
---

# Providers and models

flux keeps model transport separate from the agent runtime. A provider is a wire codec plus a
credential source: how a request is serialized and how it authenticates. The runtime still owns the
authored outer loop, capability ceilings, action batches, approval, and guarded IO.

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
`claude-fable-5`, `opus` → `claude-opus-5`, `sonnet` → `claude-sonnet-5`, `haiku` →
`claude-haiku-4-5`. The default model is `sonnet`. Bare `claude` is shorthand for `claude/sonnet`
(the [Claude Code subscription](./claude-code.md) provider).

flux gates the optional Messages-API fields per model, so every alias and id works: adaptive
thinking and `output_config.effort` are only sent to models that accept them (the 4.6 family and
newer — Haiku 4.5 and older reject them with HTTP 400), and `temperature`/`top_p` are omitted for
the generations that reject sampling params (Fable 5, Opus ≥ 4.7, Sonnet ≥ 5). Unknown or future
ids default to the newest shape, so a new Anthropic generation works on day one. The gating
applies wherever an Anthropic model is served: `anthropic`, `claude`, `aws` (Bedrock
inference-profile ids), and `openrouter` (`anthropic/…` slugs).

## Supported providers

| `-m` prefix | Wire | Credential | Notes |
|---|---|---|---|
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY` | bare aliases `fable` / `opus` / `sonnet` / `haiku` |
| `claude` | Anthropic Messages | Claude subscription OAuth | [Claude Code subscription](./claude-code.md); bare `claude` = `claude/sonnet` |
| `openai` | OpenAI Chat | `OPENAI_API_KEY` | full streaming + tool calls |
| `codex` | OpenAI Responses | ChatGPT/Codex OAuth | opt-in: `flux auth login codex` |
| `aws` | Anthropic Messages (Bedrock) | AWS chain (env / SSO / IRSA / EKS) | Claude via Bedrock; no `aws` CLI; region-aware ids; metered |
| `openrouter` | Anthropic Messages | `OPENROUTER_API_KEY` | proxy to hundreds of models; native `tool_use`, and prompt caching on `anthropic/…` slugs |
| `ollama` | OpenAI Chat | none (local) | `OLLAMA_HOST` overrides `localhost:11434`; needs a tool-capable model |
| `ollama-anthropic` | Anthropic Messages | none (local) | recent Ollama builds; native `tool_use` |
| `mock` | — | none | offline test provider; exercises the full pipeline |

Because flux's loop is tool-driven, prefer models with reliable function/tool calling. For OpenRouter
and Ollama, the `*-anthropic` variants return structured `tool_use` blocks instead of risking inline
text-leaked tool calls.

## Model capability floor

The default loop uses two ordinary function-calling contracts: a small typed intent declaration and
provider-native calls against the exact live operation schemas. The model never has to reproduce an
operation inside a Flux AST. Invalid arguments return schema diagnostics in the same native ledger so
the model can correct the call locally; rounds and token use are bounded.

OpenRouter Gemini models receive a provider-compatible view of each live operation schema on the
`openrouter` wire. Legal JSON Schema shorthands such as an untyped
array or a nullable type are translated without changing the registered contract. If a constraint
cannot be represented exactly in Gemini's function-schema subset, the request stops locally and the
error names the operation and schema path—before a billable model call. Returned arguments are still
validated against the complete original schema before approval or execution.

Use a model with reliable function calling. A model that cannot emit the intent tool, repeatedly
calls unavailable operations, or does not make progress stops with an explicit error rather than
gaining a fallback execution path. Smaller models can still be useful for narrow config/SDK stages
whose input, output, and gather-tool ceilings are tightly constrained.

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

## Reasoning controls

`--think` asks capable models to expose adaptive thinking. `--effort
low|medium|high|xhigh|max` sets the provider-mapped reasoning effort. They are independent: use one
or both. The setting follows the whole agent call graph—intent, exploration, repair, presentation,
context compaction, cognition operations, and sub-agents that do not override it.

```bash
flux run -m codex/gpt-5.6 --effort low "summarize this repository"
flux run -m anthropic/claude-sonnet-5 --think --effort high "debug this failure"
```

Provider capability gates still apply. For example, Anthropic models that reject adaptive thinking
or effort receive neither unsupported field, while OpenAI-family providers map the common effort
levels to the values their wire accepts. Reasoning output, when the provider returns it, streams on
the thinking channel; merely opening the TUI or attaching a streaming consumer does not enable it.

## Diagnosing model latency

Set `FLUX_MODEL_TRACE=1` to write one credential-free JSON summary before each native model request
and one correlated terminal record to stderr. It reports request and cache-segment sizes, selected
thinking/effort, response-header time, first thinking/tool/text time, retries, usage, and total stream
time. It applies consistently to intent, exploration, repair, presentation, compaction, cognition
calls, and sub-agents.

```bash
FLUX_MODEL_TRACE=1 flux run -m codex/gpt-5.6 --effort low "explain this failure"
```

`FLUX_MODEL_TRACE=full` additionally prints the exact JSON request body. **That body is sensitive:**
it can contain your prompts, source documents, tool results, and system instructions. Credential
headers are never included, but redirect and retain full traces only as carefully as the underlying
workspace data.

## Related docs

- [Credentials and secrets](../security/credentials.md) — where provider tokens live and how redaction works.
- [Configuration](../reference/config.md) — set a default model and permission rules.
- [Usage & cost](./cost.md) — how model usage is reported.
