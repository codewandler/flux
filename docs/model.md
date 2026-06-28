# flux — model configuration

This document describes how to configure providers and select models. flux uses a **`provider/model`** routing scheme: pass `-m <provider>/<model>` on the CLI, or set `model` in `.flux/config.toml`. The provider supplies the wire codec and credential; the model string is forwarded verbatim to that provider's API.

---

## Anthropic

**Wire:** Anthropic Messages API  
**Auth:** `ANTHROPIC_API_KEY` environment variable

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

### Usage

```bash
# Bare aliases resolve to Anthropic automatically
flux -m opus    "refactor this module"
flux -m sonnet  "explain the auth flow"
flux -m haiku   "summarise README.md"

# Fully qualified form
flux -m anthropic/claude-opus-4-5      "write tests for the parser"
flux -m anthropic/claude-sonnet-4-5    "review this PR"
flux -m anthropic/claude-haiku-4-5     "quick lint pass"
```

### Config file

```toml
# .flux/config.toml  (or ~/.flux/config.toml for a user-wide default)
model = "anthropic/claude-sonnet-4-5"
```

### Notes

- `--think` enables extended (adaptive) thinking on supported models.
- `--effort low|medium|high|xhigh|max` controls thinking depth / token budget.
- Prompt caching is applied automatically for long context windows.
- Streaming is fully supported; token deltas are shown in the TUI and REPL.

---

## OpenRouter

**Wire:** OpenAI Chat-compatible (OpenRouter proxies all models behind a single endpoint)  
**Auth:** `OPENROUTER_API_KEY` environment variable

```bash
export OPENROUTER_API_KEY=sk-or-...
```

OpenRouter gives you access to hundreds of models from different providers behind one key. The model string after `openrouter/` is forwarded directly to the OpenRouter API, so any model listed at <https://openrouter.ai/models> works.

### Usage

```bash
# General form: flux -m openrouter/<provider>/<model-slug>
flux -m openrouter/anthropic/claude-sonnet-4-5  "review this PR"
flux -m openrouter/google/gemini-2.5-pro         "explain the safety model"
flux -m openrouter/meta-llama/llama-3.3-70b-instruct  "summarise docs"
```

### Config file

```toml
model = "openrouter/anthropic/claude-sonnet-4-5"
```

### `openrouter-anthropic` — native tool calling (recommended for agentic use)

OpenRouter also exposes an **Anthropic Messages**–compatible endpoint (`/api/v1/messages`). The
`openrouter-anthropic` provider routes through it, so tool calls return as structured `tool_use`
content blocks instead of risking the inline `<tool_call>` text leakage some models exhibit on the
OpenAI Chat path. Because flux's agent loop is tool-driven, this is the more reliable choice.

```bash
flux -m openrouter-anthropic/z-ai/glm-4.6           "refactor the parser"
flux -m openrouter-anthropic/qwen/qwen3-coder       "add tests for the auth module"
flux -m openrouter-anthropic/deepseek/deepseek-chat "review this PR"
```

Same `OPENROUTER_API_KEY`; the slug is forwarded verbatim. The Chat-path `openrouter/…` provider
still exists (and now *recovers* tool calls that leak as text), but `openrouter-anthropic` avoids the
problem at the source and requests tool-capable routing (`provider.require_parameters`).

---

## GLM Z1 / GLM-4 (Zhipu AI) via OpenRouter

[Zhipu AI's GLM series](https://openrouter.ai/thudm) is available on OpenRouter under the `thudm` namespace. The latest capable model (GLM-Z1, comparable to the 5.x generation) is routed as:

| Model | OpenRouter slug | Context | Notes |
|---|---|---|---|
| GLM-Z1-32B (latest) | `z-ai/glm-5.2` | 32 k tokens | Reasoning-optimised; strong at code & maths |
| GLM-Z1-9B | `thudm/glm-z1-9b` | 32 k tokens | Lighter, faster |
| GLM-4-32B | `thudm/glm-4-32b` | 128 k tokens | Long-context general-purpose |
| GLM-4-Plus | `thudm/glm-4-plus` | 128 k tokens | Flagship GLM-4 variant |

> **Slug tip:** model slugs change as Zhipu releases new checkpoints. Always verify the exact identifier at <https://openrouter.ai/models?q=glm> before pinning a slug in config.

### Usage

```bash
export OPENROUTER_API_KEY=sk-or-...

# Latest GLM-Z1 (the "5.2" generation reasoning model)
flux -m openrouter/z-ai/glm-5.2 "write unit tests for the auth module"

# Long-context variant for big codebases
flux -m openrouter/thudm/glm-4-32b  "explain the entire provider layer"
```

### Config file

```toml
# .flux/config.toml
model = "openrouter/z-ai/glm-5.2"
```

> **Tool-calling reliability:** GLM emits tool calls far more reliably through the Messages endpoint —
> prefer **`openrouter-anthropic/z-ai/glm-4.6`** for agentic use. `glm-5.2` can still emit malformed
> or empty tool JSON on some routes (e.g. Novita); flux repairs the common cases (off-by-one braces,
> trailing characters), but an *empty* plan body can't be recovered. If you hit frequent failures, pin
> a different upstream via OpenRouter provider routing or use `z-ai/glm-4.6`.

### Mid-session model switch

You can switch models without restarting a session using the `/model` REPL command:

```
/model openrouter/z-ai/glm-5.2
```

---

## Ollama (local models)

**Wire:** OpenAI Chat-compatible ([Ollama](https://ollama.com) exposes `/v1/chat/completions`)
**Auth:** none — runs entirely on your machine

Ollama lets you run open-weight models locally with no API key and no network. flux talks to it
through the same Chat Completions codec as `openai`/`openrouter`, so everything (streaming, tool
calls) works the same — the only requirement is that the **model supports function/tool calling**,
since flux's agent loop is tool-driven.

### Setup

```bash
# 1. Install Ollama (https://ollama.com), then pull a tool-capable model:
ollama pull qwen2.5-coder:7b      # serves automatically on http://localhost:11434

# 2. Point flux at it:
flux -m ollama/qwen2.5-coder:7b "explain the provider layer"
```

The model string after `ollama/` is forwarded verbatim, including the tag (`:7b`, `:14b`, …), so
any name from `ollama list` works.

### `ollama-anthropic` — native tool calling

Recent Ollama also serves an **Anthropic Messages**–compatible endpoint (`/v1/messages`). The
`ollama-anthropic` provider uses it, so local models return native `tool_use` blocks rather than
risking inline-text tool-call leakage:

```bash
flux -m ollama-anthropic/qwen2.5-coder:7b "explain the provider layer"
```

It honours `OLLAMA_HOST` the same way; requires a recent Ollama build with Messages-API support.

### Remote / custom host

Set `OLLAMA_HOST` to target a non-default address (a bare `host:port` gets `http://` prepended):

```bash
export OLLAMA_HOST=http://192.168.1.10:11434
flux -m ollama/devstral "review this PR"
```

### Recommended models

flux is a tool-driven coding agent, so pick a model with solid **function calling**:

| Model | Pull tag | ~Size (Q4) | Notes |
|---|---|---|---|
| Qwen2.5-Coder 7B | `qwen2.5-coder:7b` | ~4.7 GB | Best small coding model with reliable tool calls — the default pick |
| Devstral 24B | `devstral` | ~14 GB | Mistral's purpose-built *agentic* coding model; best tool-use quality if you have the RAM |
| Qwen3 8B | `qwen3:8b` | ~5 GB | Newer; strong tools + optional reasoning |
| Qwen2.5-Coder 14B | `qwen2.5-coder:14b` | ~9 GB | Same family, more capable, heavier |
| Llama 3.1 8B | `llama3.1:8b` | ~4.7 GB | Reliable general-purpose tool calling |

> Tiny models (Llama 3.2 3B, Qwen2.5 3B) technically support tools but are too weak for real
> agentic coding. Don't pass `--effort` to non-reasoning local models — it sends an OpenAI
> `reasoning_effort` field they may not understand.

> **Expectations:** even the strongest small local models are noticeably weaker than Sonnet at
> multi-step tool sequences. Great for offline / CI / cheap iteration; not a drop-in Sonnet
> replacement.

### Config file

```toml
# .flux/config.toml
model = "ollama/qwen2.5-coder:7b"
```

### Mid-session model switch

```
/model ollama/qwen2.5-coder:7b
```

---

## Choosing a model

| Use case | Recommended | Rationale |
|---|---|---|
| Daily coding, file edits | `anthropic/claude-sonnet-4-5` | Fast, strong at code, supports caching |
| Long planning / reasoning | `anthropic/claude-opus-4-5` | Highest capability; use `--think` |
| Quick summarise / lint | `anthropic/claude-haiku-4-5` | Cheapest, low latency |
| Multi-provider fallback | `openrouter/anthropic/claude-sonnet-4-5` | Same model, OpenRouter routing |
| GLM / Zhipu AI work | `openrouter/z-ai/glm-5.2` | Latest GLM reasoning model |
| Local / offline coding | `ollama/qwen2.5-coder:7b` | Runs on your machine, no key; needs a tool-capable model |
| Offline / CI / testing | `-m mock` | No key required, full pipeline exercised |

---

## Credential precedence

1. Environment variable (`ANTHROPIC_API_KEY`, `OPENROUTER_API_KEY`, …)
2. Stored credential (`flux auth login <provider>`)
3. CLI-credential import (Claude subscription `~/.claude/.credentials.json`, Codex `~/.codex/auth.json`)

Run `flux auth status` to see what credentials are currently resolved and from which source.

---

## All supported providers

| `-m` prefix | Wire | Env var | Notes |
|---|---|---|---|
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY` | Supported; bare aliases `opus`/`sonnet`/`haiku` |
| `claude` | Anthropic Messages | — | Claude subscription OAuth; opt-in (`flux auth login claude`) |
| `openai` | OpenAI Chat | `OPENAI_API_KEY` | Full streaming + tool-call support |
| `codex` | OpenAI Responses | — | ChatGPT/Codex OAuth; opt-in (`flux auth login codex`) |
| `openrouter` | OpenAI Chat | `OPENROUTER_API_KEY` | Proxy to 300 + models; append any OpenRouter slug; recovers inline-text tool calls |
| `openrouter-anthropic` | Anthropic Messages | `OPENROUTER_API_KEY` | OpenRouter's native Messages endpoint — structured `tool_use`, no text leakage; preferred for agentic use |
| `ollama` | OpenAI Chat | — | Local models; no key; `OLLAMA_HOST` overrides `localhost:11434`; needs a tool-capable model |
| `ollama-anthropic` | Anthropic Messages | — | Local Ollama's Messages endpoint (recent builds) — native `tool_use` |
| `mock` | — | — | Offline test provider; no key, exercises the full pipeline |

See [docs/architecture.md](architecture.md) for the provider layer design and [docs/usage.md](usage.md) for the full CLI reference.
