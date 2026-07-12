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
flux run -m fable   "design the migration plan"
flux run -m opus    "refactor this module"
flux run -m sonnet  "explain the auth flow"
flux run -m haiku   "summarise README.md"

# Fully qualified form
flux run -m anthropic/claude-opus-4-8      "write tests for the parser"
flux run -m anthropic/claude-sonnet-4-6    "review this PR"
flux run -m anthropic/claude-haiku-4-5     "quick lint pass"
```

### Model aliases

The short aliases track the **current** generation of each tier; an explicit id is passed through
verbatim (a future id works without a flux release). One owner:
`flux_providers::anthropic::resolve_model` (mirrored for pricing in `flux-core`).

| Alias | Resolves to | Tier |
|---|---|---|
| `fable` | `claude-fable-5` | Most capable |
| `opus` | `claude-opus-4-8` | Frontier coding/agentic |
| `sonnet` | `claude-sonnet-5` | Default — speed/quality balance |
| `haiku` | `claude-haiku-4-5` | Fastest, cheapest |

### Per-model request invariants

flux gates the optional Messages-API fields per model, so every alias and id above **works** —
the request never carries a field the model rejects (C-49):

- **Adaptive thinking** (`thinking: {"type": "adaptive"}`) is sent only to models that accept it
  (the 4.6 family and newer: Fable 5, Opus ≥ 4.6, Sonnet ≥ 4.6). Haiku 4.5 and every older model
  reject it with HTTP 400, so they get no `thinking` field at all.
- **Effort** (`output_config.effort`) follows the same gate.
- **Sampling params** (`temperature`, `top_p`) are omitted for the generations that reject them
  outright (Fable 5, Opus ≥ 4.7, Sonnet ≥ 5) and still sent to the models that accept them.
- Unknown/future ids default to the newest shape (adaptive thinking on, sampling params off) —
  a new Anthropic generation works day one.

The same gating applies wherever an Anthropic model is served: `anthropic`, `claude`, `aws`
(Bedrock inference-profile ids), and `openrouter-anthropic` (`anthropic/…` slugs).

### Config file

```toml
# .flux/config.toml  (or ~/.flux/config.toml for a user-wide default)
model = "anthropic/claude-sonnet-5"
```

### Notes

- `--think` / `--effort` exist as hidden flags but are not yet wired into the engine — currently no-ops.
- Prompt caching is applied automatically for long context windows.
- Streaming is fully supported; token deltas are shown in the TUI and REPL.

## Claude subscription (`claude`) — Claude Code / Claude Max OAuth

**Wire:** Anthropic Messages API (same codec and per-model invariants as `anthropic`)
**Auth:** OAuth Bearer token — imported from Claude Code (`~/.claude/.credentials.json`) or via `flux auth login claude`

The `claude` provider bills against a **Claude subscription** (Claude Max / Claude Code) instead
of the metered API. Costs shown by flux for these runs are the *equivalent* metered figure,
marked `(sub)`.

```bash
flux run -m claude          "explain the auth flow"   # bare `claude` = claude/sonnet
flux run -m claude/opus     "refactor this module"
flux run -m claude/fable    "design the migration plan"
flux run -m claude/claude-sonnet-4-6  "pin the previous sonnet"
```

Subscription-specific invariants, on top of the per-model gating above:

- Requests authenticate with `Authorization: Bearer …` plus the `anthropic-beta: oauth-2025-04-20`
  header (never `x-api-key`).
- The token is gated to the Claude Code product: flux prefixes the system prompt with the Claude
  Code identity line, then appends its own system segments.
- Model access follows the subscription: everything the alias table lists resolves, including
  `fable` (`claude-fable-5`).
- A spec with an empty model (`claude/`) is rejected client-side with a hint; it never reaches
  the API.

## AWS Bedrock

**Wire:** Anthropic Messages (Bedrock's `invoke-with-response-stream` on an Anthropic model streams native Anthropic Messages events inside AWS event-stream framing — the exact shape flux's `messages` codec already speaks, so the codec is a thin wrapper plus a deframer)  
**Auth:** the full AWS default credential chain — **no `aws` CLI binary required**

AWS Bedrock is the compliance-friendly path to Claude for orgs that cannot send data to `api.anthropic.com` directly. flux resolves credentials from the same sources `aws-config` walks, hand-rolled in `flux-providers::bedrock`:

1. **Static env** — `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` (+ optional `AWS_SESSION_TOKEN`, `AWS_REGION`). Covers prod with env-injected creds and `aws configure export-credentials` materialized into env.
2. **SSO** (dev laptop) — reads `~/.aws/config` for the profile's `sso_session`/`sso_account_id`/`sso_role_name`, reads the cached access token from `~/.aws/sso/cache/<sha1(session)>.json`, refreshs it via SSO-OIDC `CreateToken` if expired (and persists the refreshed token back, so you re-login only when the refresh token itself dies), then calls `sso:GetRoleCredentials`.
3. **IRSA** (k8s) — `AWS_ROLE_ARN` + `AWS_WEB_IDENTITY_TOKEN_FILE` → `sts:AssumeRoleWithWebIdentity`.
4. **EKS Pod Identity** (k8s) — `AWS_CONTAINER_CREDENTIALS_FULL_URI` → HTTP GET.

### Setup (dev, SSO)

```bash
# One-time: log in (re-run only when the refresh token expires — ~days, not the ~8h access token)
aws sso login --profile your-sso-profile

# Then just:
AWS_PROFILE=your-sso-profile flux run -m aws "refactor this module"
```

No `AWS_REGION` override needed — the chain resolves it from `~/.aws/config` (`eu-central-1`, `us-east-1`, …). The access token auto-refreshes on each run when expired (~8h); you only re-login when the refresh token dies.

### Setup (prod, k8s)

No setup — the IRSA / EKS Pod Identity env vars the webhook injects are read directly. No `aws` CLI in the image, no `aws configure export-credentials`.

### Usage

```bash
# Bare `aws` → sonnet default (region-aware: us./eu. profile prefix follows AWS_REGION)
flux run -m aws                  "explain the auth flow"

# Aliases
flux run -m aws/opus             "write tests for the parser"
flux run -m aws/sonnet           "review this PR"
flux run -m aws/haiku            "quick lint pass"

# Explicit inference-profile id (pass-through)
flux run -m aws/us.anthropic.claude-sonnet-4-6   "..."
flux run -m aws/eu.anthropic.claude-opus-4-6-v1  "..."
```

### Config file

```toml
# .flux/config.toml
model = "aws/sonnet"
```

### Region-aware model resolution

Bedrock cross-region inference-profile ids are **region-specific**: `us.anthropic.*` is invalid in `eu-central-1` (Bedrock 400 "The provided model identifier is invalid"), and `eu.anthropic.*` is invalid in `us-east-1`. `resolve_model` reads `AWS_REGION` (set by the credential chain) and picks the matching prefix — `eu-*` → `eu.`, everything else → `us.`. `haiku` stays `global.` (a global profile, valid everywhere) by default. If your IAM setup doesn't grant `bedrock:InvokeModel*` on `global.*` inference profiles — only region-specific ones — set `FLUX_BEDROCK_HAIKU_PROFILE` (e.g. `us`/`eu`) to pin haiku to a region-specific prefix instead. If you pass an explicit full id (`aws/us.anthropic.claude-sonnet-4-6`), make sure it matches your region.

### Notes

- **Metered, not subscription.** Bedrock is pay-per-token via AWS (Anthropic-direct rates); the per-turn cost annotation shows `· $X` (no `(sub)` label).
- **No `aws` CLI dependency.** The chain is hand-rolled — SSO token refresh, `GetRoleCredentials`, `AssumeRoleWithWebIdentity` are direct HTTPS calls. The `aws` CLI is only needed for the one-time `aws sso login`.
- **Streaming.** The codec POSTs `/model/{id}/invoke-with-response-stream` and deframes AWS's binary event-stream into native Anthropic streaming events, so token deltas stream live like every other provider.
- **SigV4 is hand-rolled** (~150 lines, pinned by known-answer tests cross-verified against an independent Python HMAC implementation) — no AWS SDK in the flux core.

---

## OpenAI

**Wire:** OpenAI Chat Completions API (`POST /v1/chat/completions`)  
**Auth:** `OPENAI_API_KEY` environment variable

```bash
export OPENAI_API_KEY=sk-...
```

The `openai` provider talks to the OpenAI API directly, over the same Chat Completions codec as
`openrouter`/`ollama`, with full streaming and tool-call support. The model string after `openai/`
is forwarded verbatim, so any current or future OpenAI model id works — but a model **must** be
named: there is no bare-`openai` default, and `openai/` with an empty model is rejected client-side
with the hint `openai/gpt-5.5`.

### Usage

```bash
flux run -m openai/gpt-5.5   "review this PR"
flux run -m openai/gpt-5     "explain the auth flow"
```

### Config file

```toml
model = "openai/gpt-5.5"
```

### Notes

- **Metered API**, billed by OpenAI per token — no `(sub)` label.
- GPT-5-family requests automatically use the replacement `max_completion_tokens` field.

## Codex (`codex`) — ChatGPT / Codex subscription

**Wire:** OpenAI Responses API, on the ChatGPT backend  
**Auth:** ChatGPT/Codex OAuth — `flux auth login codex` (imported from `~/.codex/auth.json`)

The `codex` provider bills against a **ChatGPT/Codex subscription** rather than the metered API, so
its costs are shown as the *equivalent* metered figure, marked `(sub)` — the same convention as
`claude`.

```bash
flux run -m codex           "explain the auth flow"   # bare `codex` = codex/gpt-5.5
flux run -m codex/gpt-5.5   "refactor this module"
```

- Bare `codex` (or `codex/`) resolves to the backend's default model, **`gpt-5.5`**.
- The ChatGPT-subscription backend serves the `gpt-5.5` family and rejects the legacy
  `*-codex`-suffixed ids (`gpt-5-codex`, …) with HTTP 400; flux maps those to `gpt-5.5`. Any other
  id is forwarded verbatim, so a future model works without a flux release.
- Single owner: `flux_providers::codex::resolve_model`.

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
# General form: flux run -m openrouter/<provider>/<model-slug>
flux run -m openrouter/anthropic/claude-sonnet-4-6  "review this PR"
flux run -m openrouter/google/gemini-2.5-pro         "explain the safety model"
flux run -m openrouter/meta-llama/llama-3.3-70b-instruct  "summarise docs"
```

### Config file

```toml
model = "openrouter/anthropic/claude-sonnet-4-6"
```

### `openrouter-anthropic` — native tool calling (recommended for agentic use)

OpenRouter also exposes an **Anthropic Messages**–compatible endpoint (`/api/v1/messages`). The
`openrouter-anthropic` provider routes through it, so tool calls return as structured `tool_use`
content blocks instead of risking the inline `<tool_call>` text leakage some models exhibit on the
OpenAI Chat path. Because flux's agent loop is tool-driven, this is the more reliable choice.

```bash
flux run -m openrouter-anthropic/z-ai/glm-4.6           "refactor the parser"
flux run -m openrouter-anthropic/qwen/qwen3-coder       "add tests for the auth module"
flux run -m openrouter-anthropic/deepseek/deepseek-chat "review this PR"
```

Same `OPENROUTER_API_KEY`; the slug is forwarded verbatim. The Chat-path `openrouter/…` provider
still exists (and now *recovers* tool calls that leak as text), but `openrouter-anthropic` avoids the
problem at the source and requests tool-capable routing (`provider.require_parameters`).

---

## GLM (Zhipu AI) via OpenRouter

Zhipu AI's GLM series is available on OpenRouter under the `z-ai` namespace. The two slugs that
matter for flux:

| Model | OpenRouter slug | Notes |
|---|---|---|
| GLM-4.6 | `z-ai/glm-4.6` | The reliable agentic route — use it via `openrouter-anthropic` (see below) |
| GLM-5.2 | `z-ai/glm-5.2` | Emits malformed/empty plan JSON on the Chat route; not recommended for flux |

> **Slug tip:** model slugs change as Zhipu releases new checkpoints. Always verify the exact identifier at <https://openrouter.ai/models?q=glm> before pinning a slug in config.

### Usage

```bash
export OPENROUTER_API_KEY=sk-or-...

# Recommended: GLM-4.6 over the Anthropic Messages endpoint (structured tool_use)
flux run -m openrouter-anthropic/z-ai/glm-4.6 "write unit tests for the auth module"
```

### Config file

```toml
# .flux/config.toml
model = "openrouter-anthropic/z-ai/glm-4.6"
```

> **Tool-calling reliability:** GLM emits tool calls far more reliably through the Messages endpoint —
> prefer **`openrouter-anthropic/z-ai/glm-4.6`** for agentic use. `glm-5.2` can still emit malformed
> or empty tool JSON on some routes (e.g. Novita); flux repairs the common cases (off-by-one braces,
> trailing characters), but an *empty* plan body can't be recovered. If you hit frequent failures, pin
> a different upstream via OpenRouter provider routing or use `z-ai/glm-4.6`.

### Mid-session model switch

You can switch models without restarting a session using the `/model` REPL command:

```
/model openrouter-anthropic/z-ai/glm-4.6
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
flux run -m ollama/qwen2.5-coder:7b "explain the provider layer"
```

The model string after `ollama/` is forwarded verbatim, including the tag (`:7b`, `:14b`, …), so
any name from `ollama list` works.

### `ollama-anthropic` — native tool calling

Recent Ollama also serves an **Anthropic Messages**–compatible endpoint (`/v1/messages`). The
`ollama-anthropic` provider uses it, so local models return native `tool_use` blocks rather than
risking inline-text tool-call leakage:

```bash
flux run -m ollama-anthropic/qwen2.5-coder:7b "explain the provider layer"
```

It honours `OLLAMA_HOST` the same way; requires a recent Ollama build with Messages-API support.

### Remote / custom host

Set `OLLAMA_HOST` to target a non-default address (a bare `host:port` gets `http://` prepended):

```bash
export OLLAMA_HOST=http://192.168.1.10:11434
flux run -m ollama/devstral "review this PR"
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
> agentic coding.

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
| Daily coding, file edits | `sonnet` (= `anthropic/claude-sonnet-5`) | Fast, strong at code, supports caching |
| Long planning / reasoning | `opus` (= `anthropic/claude-opus-4-8`) | Frontier coding/agentic capability |
| Hardest, long-horizon work | `fable` (= `anthropic/claude-fable-5`) | Most capable model |
| Quick summarise / lint | `haiku` (= `anthropic/claude-haiku-4-5`) | Cheapest, low latency |
| On a Claude subscription | `claude/sonnet`, `claude/opus`, … | Same models, billed to the subscription |
| Multi-provider fallback | `openrouter/anthropic/claude-sonnet-4-6` | Same model, OpenRouter routing |
| GLM / Zhipu AI work | `openrouter-anthropic/z-ai/glm-4.6` | Reliable GLM tool calling via the Messages endpoint |
| Local / offline coding | `ollama/qwen2.5-coder:7b` | Runs on your machine, no key; needs a tool-capable model |
| AWS / Bedrock (compliance) | `aws/sonnet` | Claude via AWS Bedrock; SSO/IRSA, no `aws` CLI; metered |
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
| `anthropic` | Anthropic Messages | `ANTHROPIC_API_KEY` | Supported; bare aliases `fable`/`opus`/`sonnet`/`haiku` |
| `claude` | Anthropic Messages | — | Claude subscription OAuth; opt-in (`flux auth login claude`); bare `claude` = `claude/sonnet` |
| `openai` | OpenAI Chat | `OPENAI_API_KEY` | Full streaming + tool-call support |
| `codex` | OpenAI Responses | — | ChatGPT/Codex OAuth; opt-in (`flux auth login codex`) |
| `aws` | Anthropic Messages (Bedrock) | `AWS_*` / SSO / IRSA / EKS Pod Identity | Claude via AWS Bedrock; full credential chain, no `aws` CLI; metered; region-aware model ids |
| `openrouter` | OpenAI Chat | `OPENROUTER_API_KEY` | Proxies a large catalog of models; the `provider/model` slug after `openrouter/` is forwarded verbatim; recovers inline-text tool calls |
| `openrouter-anthropic` | Anthropic Messages | `OPENROUTER_API_KEY` | OpenRouter's native Messages endpoint — structured `tool_use`, no text leakage; preferred for agentic use |
| `ollama` | OpenAI Chat | — | Local models; no key; `OLLAMA_HOST` overrides `localhost:11434`; needs a tool-capable model |
| `ollama-anthropic` | Anthropic Messages | — | Local Ollama's Messages endpoint (recent builds) — native `tool_use` |
| `mock` | — | — | Offline test provider; no key, exercises the full pipeline |

See [docs/architecture.md](architecture.md) for the provider layer design and [docs/usage.md](usage.md) for the full CLI reference.
