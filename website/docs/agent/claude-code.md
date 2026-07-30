---
title: Claude Code (subscription)
description: "Run flux on a Claude Code / Claude Max subscription: the claude provider, its model aliases, and the per-model request invariants."
---

# Claude Code (subscription)

The `claude` provider runs flux against the same backend as the `anthropic` provider, but
authenticates with a **Claude Code / Claude Max subscription** OAuth token instead of a metered
API key. If you already use Claude Code, flux imports its credential automatically — no separate
key, and usage bills against the subscription you already pay for.

```bash
flux auth status                 # shows `claude` once a credential resolves
flux auth login claude           # or log in explicitly (OAuth)

flux run -m claude "explain the auth flow"        # bare `claude` = claude/sonnet
flux run -m claude/opus "refactor this module"
flux run -m claude/fable "design the migration plan"
```

The credential resolves in this order: a token stored by `flux auth login claude`, then an
imported Claude Code credential (`~/.claude/.credentials.json`).

## Models

The short aliases track the **current** generation of each tier. An explicit id is passed
through verbatim, so a newly released model works without a flux upgrade. Model access follows
your subscription.

| Spec | Resolves to | Tier |
|---|---|---|
| `claude/fable` | `claude-fable-5` | Most capable — hardest, long-horizon work |
| `claude/opus` | `claude-opus-5` | Frontier coding / agentic |
| `claude/sonnet` (and bare `claude`) | `claude-sonnet-5` | Default — speed/quality balance |
| `claude/haiku` | `claude-haiku-4-5` | Fastest, cheapest |
| `claude/<full-id>` | verbatim | e.g. `claude/claude-sonnet-4-6` to pin a previous generation |

## Request invariants

flux gates every optional Messages-API field per model, so **each spec above works** — a request
never carries a field its model rejects:

- **Adaptive thinking** (`thinking: {"type": "adaptive"}`) is sent only to models that accept it —
  the 4.6 family and newer (Fable 5, Opus ≥ 4.6, Sonnet ≥ 4.6). Haiku 4.5 and older models reject
  it with HTTP 400, so they get no `thinking` field at all.
- **Effort** (`output_config.effort`) follows the same gate.
- **Sampling params** (`temperature`, `top_p`) are omitted for the generations that reject them
  outright (Fable 5, Opus ≥ 4.7, Sonnet ≥ 5) and still sent to models that accept them.
- **Unknown/future ids default to the newest shape** (adaptive thinking on, sampling params off),
  so a new Anthropic generation works on day one.
- **Prompt caching** is applied automatically for long contexts.

Subscription-specific invariants on top:

- Requests authenticate with `Authorization: Bearer …` plus the `anthropic-beta: oauth-2025-04-20`
  header — never `x-api-key`.
- The token is product-gated to Claude Code: flux prefixes the system prompt with the Claude Code
  identity line, then appends its own system segments.
- A malformed spec fails **client-side** with a hint (`claude/` → "names provider `claude` but no
  model — add one, e.g. `claude/sonnet`"); it never reaches the API.

## Cost reporting

Subscription turns are marked `(sub)` in the CLI/TUI cost line and in `flux usage`: the dollar
figure is the *equivalent metered* cost — what the same tokens would have cost on the `anthropic`
API — not an incremental charge. See [Usage & cost](./cost.md).

## Related docs

- [Claude Code compatibility](./claude-compat.md) — this page is about the *provider*; that one
  covers loading Claude Code's skills/commands file formats.
- [Providers and models](./providers.md) — every provider, routing, and the capability floor.
- [Credentials and secrets](../security/credentials.md) — where the OAuth token lives.
